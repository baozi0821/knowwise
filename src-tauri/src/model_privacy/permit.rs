//! 外发许可 EgressPermit(《M08》§4 / 《01》§4.8)。
//!
//! 许可只驻留内存、短期有效、单次消费:重试需要新许可,重放旧许可失败。
//! 撤回授权或切换参考模型时按谓词批量吊销。许可不落库、不进日志。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::shared::{AppError, Clock};
use sha2::{Digest, Sha256};

use super::errors::{model_err, ModelErrorCode};
use super::models::{ModelPurpose, ProcessingLocation};

/// 进程内外发许可(非 IPC DTO;字段对齐《M08》§4)。
#[derive(Debug, Clone, PartialEq)]
pub struct EgressPermit {
    pub permit_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub document_id: String,
    pub snapshot_id: String,
    pub purpose: ModelPurpose,
    pub location: ProcessingLocation,
    pub provider_id: String,
    pub model_id: String,
    pub allowed_source_ids: Vec<String>,
    pub payload_hash: String,
    pub expires_at: i64,
    /// 作用域:Task 许可在消费时复核任务;Document 许可跳过任务门控。
    pub scope: crate::model_privacy::gateway::EgressScope,
}

impl EgressPermit {
    pub fn is_expired(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at
    }
}

/// 载荷来源:业务模块提交的原文片段与其来源 ID(最小来源,不隐式扩大)。
#[derive(Debug, Clone, PartialEq)]
pub struct EgressPayloadPart {
    pub source_id: String,
    pub text: String,
    /// 图片附件字节的 sha256 hex;纯文本载荷为 None。
    /// 摘要进入许可哈希:同一文本 + 同一图片稳定,换图片即失配。
    pub attachment_digest: Option<String>,
}

/// 载荷哈希:绑定任务/Attempt/快照/目的/提供方/模型与全部来源文本。
/// 消费时重算比对,任何替换(含提示词内容)都会导致 PRIVACY_EGRESS_DENIED。
// 载荷哈希的输入即许可的全部绑定域;少一个参数就意味着哈希少绑一项。
#[allow(clippy::too_many_arguments)]
pub fn payload_hash_of(
    task_id: &str,
    attempt_id: &str,
    snapshot_id: &str,
    purpose: ModelPurpose,
    location: ProcessingLocation,
    provider_id: &str,
    model_id: &str,
    parts: &[EgressPayloadPart],
) -> String {
    let mut hasher = Sha256::new();
    // v2:附件摘要进入载荷哈希(纯文本请求仍按同一结构确定性计算)。
    hasher.update(b"knowwise.egress.v2\0");
    for field in [
        task_id,
        attempt_id,
        snapshot_id,
        purpose.as_str(),
        location.as_str(),
        provider_id,
        model_id,
    ] {
        hasher.update(field.as_bytes());
        hasher.update([0u8]);
    }
    for part in parts {
        hasher.update(part.source_id.as_bytes());
        hasher.update([0u8]);
        hasher.update(part.text.as_bytes());
        hasher.update([0u8]);
        match &part.attachment_digest {
            Some(digest) => {
                hasher.update(b"attachment\0");
                hasher.update(digest.as_bytes());
            }
            None => hasher.update(b"text\0"),
        }
        hasher.update([0u8]);
    }
    hex::encode(hasher.finalize())
}

fn egress_denied(reason: &'static str) -> AppError {
    model_err(ModelErrorCode::PrivacyEgressDenied).with_detail("reason", reason.into())
}

struct PermitEntry {
    permit: EgressPermit,
    consumed: bool,
}

/// 许可注册表:内存、短期、单次消费。
pub struct PermitRegistry {
    entries: Mutex<HashMap<String, PermitEntry>>,
    clock: std::sync::Arc<dyn Clock>,
}

impl PermitRegistry {
    pub fn new(clock: std::sync::Arc<dyn Clock>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            clock,
        }
    }

    /// 签发新许可(TTL 内有效);permit_id 由调用方(IdGenerator)生成。
    pub fn issue(&self, permit_id: &str, permit: EgressPermit) {
        let mut map = self.entries.lock().unwrap();
        // 顺带清理已过期条目,防止累积。
        let now = self.clock.now_ms();
        map.retain(|_, e| !e.permit.is_expired(now) && !e.consumed);
        map.insert(
            permit_id.to_string(),
            PermitEntry {
                permit,
                consumed: false,
            },
        );
    }

    /// 消费许可(取出即失效;重放被拒)。过期/未知/已消费分别给出稳定 reason。
    pub fn consume(&self, permit_id: &str) -> Result<EgressPermit, AppError> {
        let now = self.clock.now_ms();
        let mut map = self.entries.lock().unwrap();
        let Some(entry) = map.get_mut(permit_id) else {
            return Err(egress_denied("permit_unknown"));
        };
        if entry.consumed {
            return Err(egress_denied("permit_replayed"));
        }
        if entry.permit.is_expired(now) {
            entry.consumed = true;
            return Err(egress_denied("permit_expired"));
        }
        entry.consumed = true;
        Ok(entry.permit.clone())
    }

    /// 撤回授权/切换模型时按谓词吊销(立即失效,即使未过期)。
    pub fn revoke_where(&self, pred: impl Fn(&EgressPermit) -> bool) -> usize {
        let mut map = self.entries.lock().unwrap();
        let before = map.len();
        map.retain(|_, e| !pred(&e.permit));
        before - map.len()
    }

    pub fn revoke_all(&self) -> usize {
        let mut map = self.entries.lock().unwrap();
        let n = map.len();
        map.clear();
        n
    }

    pub fn len_active(&self) -> usize {
        let now = self.clock.now_ms();
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| !e.consumed && !e.permit.is_expired(now))
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len_active() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::TestClock;

    fn permit(id: &str, expires_at: i64) -> EgressPermit {
        EgressPermit {
            scope: crate::model_privacy::gateway::EgressScope::Task,
            permit_id: id.into(),
            task_id: "t1".into(),
            attempt_id: "a1".into(),
            document_id: "d1".into(),
            snapshot_id: "s1".into(),
            purpose: ModelPurpose::Generate,
            location: ProcessingLocation::Remote,
            provider_id: "p1".into(),
            model_id: "m1".into(),
            allowed_source_ids: vec!["src-1".into()],
            payload_hash: "h".into(),
            expires_at,
        }
    }

    #[test]
    fn single_use_replay_rejected() {
        let clock = std::sync::Arc::new(TestClock::new(1_000));
        let reg = PermitRegistry::new(clock);
        reg.issue("k1", permit("k1", 2_000));
        assert!(reg.consume("k1").is_ok());
        let err = reg.consume("k1").unwrap_err();
        assert_eq!(err.code, "PRIVACY_EGRESS_DENIED");
        assert_eq!(err.details.unwrap()["reason"], "permit_replayed".into());
    }

    #[test]
    fn expired_permit_rejected() {
        let clock = std::sync::Arc::new(TestClock::new(1_000));
        let reg = PermitRegistry::new(clock.clone());
        reg.issue("k1", permit("k1", 1_500));
        clock.set(1_600);
        let err = reg.consume("k1").unwrap_err();
        assert_eq!(err.details.unwrap()["reason"], "permit_expired".into());
    }

    #[test]
    fn unknown_permit_rejected() {
        let clock = std::sync::Arc::new(TestClock::new(1_000));
        let reg = PermitRegistry::new(clock);
        let err = reg.consume("nope").unwrap_err();
        assert_eq!(err.details.unwrap()["reason"], "permit_unknown".into());
    }

    #[test]
    fn revoke_where_targets_matching_permits() {
        let clock = std::sync::Arc::new(TestClock::new(1_000));
        let reg = PermitRegistry::new(clock);
        reg.issue("a", permit("a", 2_000));
        let mut p2 = permit("b", 2_000);
        p2.document_id = "d2".into();
        reg.issue("b", p2);
        let n = reg.revoke_where(|p| p.document_id == "d1");
        assert_eq!(n, 1);
        assert!(reg.consume("a").is_err());
        assert!(reg.consume("b").is_ok());
    }

    #[test]
    fn payload_hash_binds_every_field() {
        let parts = vec![EgressPayloadPart {
            source_id: "b1".into(),
            text: "hello".into(),
            attachment_digest: None,
        }];
        let base = payload_hash_of(
            "t",
            "a",
            "s",
            ModelPurpose::Generate,
            ProcessingLocation::Remote,
            "p",
            "m",
            &parts,
        );
        assert_eq!(
            base,
            payload_hash_of(
                "t",
                "a",
                "s",
                ModelPurpose::Generate,
                ProcessingLocation::Remote,
                "p",
                "m",
                &parts
            )
        );
        // 任一字段变化 → 哈希变化。
        assert_ne!(
            base,
            payload_hash_of(
                "t2",
                "a",
                "s",
                ModelPurpose::Generate,
                ProcessingLocation::Remote,
                "p",
                "m",
                &parts
            )
        );
        assert_ne!(
            base,
            payload_hash_of(
                "t",
                "a",
                "s",
                ModelPurpose::Embed,
                ProcessingLocation::Remote,
                "p",
                "m",
                &parts
            )
        );
        assert_ne!(
            base,
            payload_hash_of(
                "t",
                "a",
                "s",
                ModelPurpose::Generate,
                ProcessingLocation::Local,
                "p",
                "m",
                &parts
            )
        );
        assert_ne!(
            base,
            payload_hash_of(
                "t",
                "a",
                "s",
                ModelPurpose::Generate,
                ProcessingLocation::Remote,
                "p2",
                "m",
                &parts
            )
        );
        assert_ne!(
            base,
            payload_hash_of(
                "t",
                "a",
                "s",
                ModelPurpose::Generate,
                ProcessingLocation::Remote,
                "p",
                "m2",
                &parts
            )
        );
        // 载荷文本或来源篡改 → 哈希变化。
        let tampered = vec![EgressPayloadPart {
            source_id: "b1".into(),
            text: "hello!".into(),
            attachment_digest: None,
        }];
        assert_ne!(
            base,
            payload_hash_of(
                "t",
                "a",
                "s",
                ModelPurpose::Generate,
                ProcessingLocation::Remote,
                "p",
                "m",
                &tampered
            )
        );
        let other_source = vec![EgressPayloadPart {
            source_id: "b2".into(),
            text: "hello".into(),
            attachment_digest: None,
        }];
        assert_ne!(
            base,
            payload_hash_of(
                "t",
                "a",
                "s",
                ModelPurpose::Generate,
                ProcessingLocation::Remote,
                "p",
                "m",
                &other_source
            )
        );
    }

    #[test]
    fn payload_hash_binds_attachment_digest() {
        let part = |digest: Option<&str>| {
            vec![EgressPayloadPart {
                source_id: "b1".into(),
                text: "看图".into(),
                attachment_digest: digest.map(str::to_string),
            }]
        };
        let hash = |digest: Option<&str>| {
            payload_hash_of(
                "t",
                "a",
                "s",
                ModelPurpose::Generate,
                ProcessingLocation::Remote,
                "p",
                "m",
                &part(digest),
            )
        };
        let digest_a = "a".repeat(64);
        let digest_b = "b".repeat(64);
        // 同一文本 + 同一图片摘要 → 稳定。
        assert_eq!(hash(Some(&digest_a)), hash(Some(&digest_a)));
        // 同一文本 + 不同图片 → 哈希不同。
        assert_ne!(hash(Some(&digest_a)), hash(Some(&digest_b)));
        // 无图与有图不能碰撞。
        assert_ne!(hash(None), hash(Some(&digest_a)));
    }
}
