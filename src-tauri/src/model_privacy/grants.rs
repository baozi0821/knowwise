//! 隐私授权存取(《M08》§5 / 《01》§4.8)。
//!
//! 按 Document × processingLocation × providerId × purpose 管理;本地与远程
//! 分别授权,生成与嵌入分别授权。撤回不删行(历史保留);"单一当前语义"
//! 由本模块保证:同一元组至多一行 status='granted'。
//! 模型名称变化不扩大提供方范围——授权绑定 providerId,与具体模型无关。

use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};

use crate::persistence::{db_app_error, Db};
use crate::shared::{AppError, Clock, CoreErrorCode, IdGenerator};

use super::gateway::GrantSource;
use super::models::{
    ModelPurpose, PrivacyGrantStatus, PrivacyGrantView, ProcessingLocation, PRIVACY_POLICY_VERSION,
};

pub struct GrantStore {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

#[derive(Debug, Clone)]
pub struct GrantRow {
    pub id: String,
    pub document_id: String,
    pub location: ProcessingLocation,
    pub provider_id: String,
    pub purpose: ModelPurpose,
    pub status: PrivacyGrantStatus,
    pub granted_at: i64,
    pub revoked_at: Option<i64>,
    pub policy_version: String,
}

impl GrantRow {
    pub fn to_view(&self) -> PrivacyGrantView {
        PrivacyGrantView {
            id: self.id.clone(),
            document_id: self.document_id.clone(),
            processing_location: self.location,
            provider_id: self.provider_id.clone(),
            purpose: self.purpose,
            status: self.status,
            granted_at: self.granted_at,
            revoked_at: self.revoked_at,
            policy_version: self.policy_version.clone(),
        }
    }
}

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

pub fn document_exists_tx(conn: &Connection, document_id: &str) -> Result<bool, AppError> {
    conn.query_row("SELECT 1 FROM document WHERE id=?1", [document_id], |_| {
        Ok(())
    })
    .optional()
    .map(|o| o.is_some())
    .map_err(|e| db_app_error(&e))
}

fn row_to_grant(r: &rusqlite::Row<'_>) -> rusqlite::Result<GrantRow> {
    let location: String = r.get("processing_location")?;
    let purpose: String = r.get("purpose")?;
    let status: String = r.get("status")?;
    Ok(GrantRow {
        id: r.get("id")?,
        document_id: r.get("document_id")?,
        location: ProcessingLocation::parse_db(&location).unwrap_or(ProcessingLocation::Remote),
        provider_id: r.get("provider_id")?,
        purpose: ModelPurpose::parse_db(&purpose).unwrap_or(ModelPurpose::Generate),
        status: match status.as_str() {
            "granted" => PrivacyGrantStatus::Granted,
            _ => PrivacyGrantStatus::Revoked,
        },
        granted_at: r.get("granted_at")?,
        revoked_at: r.get("revoked_at")?,
        policy_version: r.get("policy_version")?,
    })
}

fn active_grant_tx(
    conn: &Connection,
    document_id: &str,
    location: ProcessingLocation,
    provider_id: &str,
    purpose: ModelPurpose,
) -> Result<Option<GrantRow>, AppError> {
    conn.query_row(
        "SELECT id, document_id, processing_location, provider_id, purpose, status, granted_at, \
         revoked_at, policy_version FROM privacy_grant \
         WHERE document_id=?1 AND processing_location=?2 AND provider_id=?3 AND purpose=?4 \
         AND status='granted' ORDER BY granted_at DESC LIMIT 1",
        params![
            document_id,
            location.as_str(),
            provider_id,
            purpose.as_str()
        ],
        row_to_grant,
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

impl GrantSource for GrantStore {
    fn is_granted(
        &self,
        document_id: &str,
        location: ProcessingLocation,
        provider_id: &str,
        purpose: ModelPurpose,
    ) -> Result<bool, AppError> {
        let (d, l, p, u) = (
            document_id.to_string(),
            location,
            provider_id.to_string(),
            purpose,
        );
        self.db
            .read(move |conn| Ok(active_grant_tx(conn, &d, l, &p, u)?.is_some()))
    }
}

pub struct GrantInput {
    pub document_id: String,
    pub location: ProcessingLocation,
    pub provider_id: String,
    pub purpose: ModelPurpose,
    pub policy_version: String,
    pub idempotency_key: String,
}

impl GrantStore {
    pub fn new(db: Arc<Db>, clock: Arc<dyn Clock>, ids: Arc<dyn IdGenerator>) -> Self {
        Self { db, clock, ids }
    }

    /// 授权:已有有效授权时幂等返回;policyVersion 必须匹配当前策略,
    /// 过期策略版本要求用户重新确认(不静默沿用)。
    pub fn grant(&self, input: &GrantInput) -> Result<GrantRow, AppError> {
        if input.policy_version != PRIVACY_POLICY_VERSION {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "policy_version_mismatch".into()));
        }
        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let inp = GrantInput {
            document_id: input.document_id.clone(),
            location: input.location,
            provider_id: input.provider_id.clone(),
            purpose: input.purpose,
            policy_version: input.policy_version.clone(),
            idempotency_key: input.idempotency_key.clone(),
        };
        self.db.write(move |conn| {
            if !document_exists_tx(conn, &inp.document_id)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            // 幂等键:重复请求返回已有授权。
            let existing_by_key: Option<GrantRow> = conn
                .query_row(
                    "SELECT id, document_id, processing_location, provider_id, purpose, status, \
                     granted_at, revoked_at, policy_version FROM privacy_grant \
                     WHERE idempotency_key=?1",
                    [&inp.idempotency_key],
                    row_to_grant,
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if let Some(row) = existing_by_key {
                return Ok(row);
            }
            if let Some(active) = active_grant_tx(
                conn,
                &inp.document_id,
                inp.location,
                &inp.provider_id,
                inp.purpose,
            )? {
                return Ok(active);
            }
            conn.execute(
                "INSERT INTO privacy_grant(id, document_id, processing_location, provider_id, \
                 purpose, status, granted_at, revoked_at, policy_version, idempotency_key, \
                 created_at) VALUES (?1,?2,?3,?4,?5,'granted',?6,NULL,?7,?8,?6)",
                params![
                    id,
                    inp.document_id,
                    inp.location.as_str(),
                    inp.provider_id,
                    inp.purpose.as_str(),
                    now,
                    inp.policy_version,
                    inp.idempotency_key
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            conn.query_row(
                "SELECT id, document_id, processing_location, provider_id, purpose, status, \
                 granted_at, revoked_at, policy_version FROM privacy_grant WHERE id=?1",
                [&id],
                row_to_grant,
            )
            .map_err(|e| db_app_error(&e))
        })
    }

    /// 撤回:有效授权置 revoked(保留历史);无有效授权时幂等成功。
    pub fn revoke(
        &self,
        document_id: &str,
        location: ProcessingLocation,
        provider_id: &str,
        purpose: ModelPurpose,
    ) -> Result<Option<GrantRow>, AppError> {
        let now = self.clock.now_ms();
        let (d, l, p, u) = (
            document_id.to_string(),
            location,
            provider_id.to_string(),
            purpose,
        );
        self.db.write(move |conn| {
            if !document_exists_tx(conn, &d)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            let active = active_grant_tx(conn, &d, l, &p, u)?;
            let Some(row) = active else {
                return Ok(None);
            };
            conn.execute(
                "UPDATE privacy_grant SET status='revoked', revoked_at=?2 WHERE id=?1 \
                 AND status='granted'",
                params![row.id, now],
            )
            .map_err(|e| db_app_error(&e))?;
            conn.query_row(
                "SELECT id, document_id, processing_location, provider_id, purpose, status, \
                 granted_at, revoked_at, policy_version FROM privacy_grant WHERE id=?1",
                [&row.id],
                row_to_grant,
            )
            .optional()
            .map_err(|e| db_app_error(&e))
        })
    }

    /// 文档全部授权历史(含已撤回),按授予时间倒序。
    pub fn list_for_document(&self, document_id: &str) -> Result<Vec<GrantRow>, AppError> {
        let d = document_id.to_string();
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, document_id, processing_location, provider_id, purpose, status, \
                     granted_at, revoked_at, policy_version FROM privacy_grant \
                     WHERE document_id=?1 ORDER BY granted_at DESC, id",
                )
                .map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map([&d], row_to_grant)
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(rows)
        })
    }

    /// 有效授权(状态视图构建用)。
    pub fn active_grant(
        &self,
        document_id: &str,
        location: ProcessingLocation,
        provider_id: &str,
        purpose: ModelPurpose,
    ) -> Result<Option<GrantRow>, AppError> {
        let (d, l, p, u) = (
            document_id.to_string(),
            location,
            provider_id.to_string(),
            purpose,
        );
        self.db
            .read(move |conn| active_grant_tx(conn, &d, l, &p, u))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_helpers_roundtrip() {
        assert_eq!(
            ProcessingLocation::parse_db(ProcessingLocation::Remote.as_str()),
            Some(ProcessingLocation::Remote)
        );
        assert_eq!(
            ModelPurpose::parse_db(ModelPurpose::Embed.as_str()),
            Some(ModelPurpose::Embed)
        );
        assert_eq!(
            ModelPurpose::parse_db(ModelPurpose::Vision.as_str()),
            Some(ModelPurpose::Vision)
        );
    }
}
