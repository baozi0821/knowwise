//! EgressGateway:唯一模型网络出口(《M08》§4 / 《00》§3.4 红线)。
//!
//! 技术强制层:M04/M09 的前置授权检查只是用户体验层;即便绕过 UI 直接
//! 调用内部命令,未经本网关的许可签发与消费,任何模型请求都不会发出。
//! 结构保证:适配器没有独立 HTTP 客户端与密钥,全部由网关注入。
//!
//! 每次发送前的完整校验链(签发与消费各执行一遍):
//! 1. 任务处于合法状态(queued/running、当前 Attempt、未请求取消);
//! 2. 隐私授权仍有效(按 Document × location × provider × purpose);
//! 3. 远程提供方密钥已配置(取自系统凭据,发送前注入,不落盘);
//! 4. 载荷哈希与允许来源集合一致(消费时重算,防篡改/防重放);
//! 5. 目的地唯一且为配置值(客户端禁重定向/禁代理继承)。
//!
//! 全部通过后才计数并放行——未授权时远程请求计数恒为零(M08-A2)。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::shared::{AppError, Clock, IdGenerator};

use super::errors::{model_err, ModelErrorCode, ProviderError};
use super::models::{ModelPurpose, ProcessingLocation, EGRESS_PERMIT_TTL_MS};
use super::permit::{payload_hash_of, EgressPayloadPart, EgressPermit, PermitRegistry};
use super::provider::{
    DeltaSink, EmbedRequest, EmbedSuccess, GenerateRequest, GenerateSuccess,
    GenerateWithToolsRequest, GenerateWithToolsSuccess, ModelProvider, ProbeResult,
    ProviderMessage, ProviderTransport, ToolSpec,
};
use super::secrets::SecretStore;

/// 授权查询端口(由 grants.rs 的 GrantStore 实现)。
pub trait GrantSource: Send + Sync {
    fn is_granted(
        &self,
        document_id: &str,
        location: ProcessingLocation,
        provider_id: &str,
        purpose: ModelPurpose,
    ) -> Result<bool, AppError>;
}

/// 任务合法性端口(由 TaskService 适配器实现)。
pub trait EgressTaskGate: Send + Sync {
    /// 任务存在、状态为 queued/running、Attempt 为当前 Attempt 且未请求取消。
    fn egress_allowed(&self, task_id: &str, attempt_id: &str) -> Result<bool, AppError>;
}

/// 出口计数(A2 断言与诊断用;均为非敏感计数)。
#[derive(Default)]
pub struct EgressStats {
    /// 实际放行的远程模型请求(不含探测;探测无内容,单独计)。
    pub remote_requests: AtomicU64,
    /// 实际放行的本地模型请求。
    pub local_requests: AtomicU64,
    pub generate_requests: AtomicU64,
    pub embed_requests: AtomicU64,
    /// 远程可达性/模型列表探测(无用户内容)。
    pub remote_probes: AtomicU64,
    /// 在任何发送之前被拒绝的次数(授权/许可/哈希/任务)。
    pub denied: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressStatsSnapshot {
    pub remote_requests: u64,
    pub local_requests: u64,
    pub generate_requests: u64,
    pub embed_requests: u64,
    pub remote_probes: u64,
    pub denied: u64,
}

struct ProviderSlot {
    provider: Arc<dyn ModelProvider>,
    /// 无 bearer 的传输模板;远程密钥在每次发送/探测前注入。
    transport: ProviderTransport,
}

/// 外发请求的作用域。
///
/// 绝大多数请求属于某个 Task(问答/研究/产物生成),必须通过任务门控
/// (状态、当前 Attempt、未取消)。少数请求天然没有 Task:
/// 动态卡片的单次生成、检索查询向量、会话摘要刷新等——它们由 UI 直接
/// 触发或属于子步骤。这类请求使用 `Document` 作用域:只跳过任务门控,
/// **授权、载荷哈希、来源白名单、密钥与审计仍然全部执行**,不绕过网关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressScope {
    /// 必须绑定活动 Task 与当前 Attempt。
    Task,
    /// 文档级请求:不绑定 Task,但其余校验不变。
    Document,
}

/// 签发/发送共用的外发请求规格。
#[derive(Debug, Clone)]
pub struct EgressRequestSpec {
    pub task_id: String,
    pub attempt_id: String,
    pub document_id: String,
    pub snapshot_id: String,
    pub purpose: ModelPurpose,
    pub provider_id: String,
    pub model_id: String,
    pub parts: Vec<EgressPayloadPart>,
    /// 作用域;默认 Task(构造方显式声明 Document 才豁免任务门控)。
    pub scope: EgressScope,
}

impl EgressRequestSpec {
    /// 任务作用域请求(既有行为,默认)。
    pub fn for_task(mut self) -> Self {
        self.scope = EgressScope::Task;
        self
    }

    /// 文档作用域请求:无 Task 的子步骤(卡片/查询向量/摘要)。
    pub fn for_document(mut self) -> Self {
        self.scope = EgressScope::Document;
        self
    }

    /// 该请求是否需要通过任务门控。
    fn requires_active_task(&self) -> bool {
        self.scope == EgressScope::Task
    }
}

pub struct EgressGateway {
    slots: RwLock<HashMap<String, ProviderSlot>>,
    grants: Arc<dyn GrantSource>,
    task_gate: Arc<dyn EgressTaskGate>,
    secrets: Arc<dyn SecretStore>,
    permits: PermitRegistry,
    concurrency: RwLock<Arc<Semaphore>>,
    /// 全局开关:允许把图片发送到远程模型。默认关闭,宁可拒绝也不静默外发。
    remote_vision_allowed: RwLock<bool>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    pub stats: EgressStats,
}

/// 出口拒绝:静态原因码进入日志(非敏感),错误信封保持原契约。
fn egress_denied(reason: &'static str) -> AppError {
    tracing::warn!(
        event = "egress_denied",
        phase = reason,
        error_code = "PRIVACY_EGRESS_DENIED",
        "egress request denied"
    );
    model_err(ModelErrorCode::PrivacyEgressDenied).with_detail("reason", reason.into())
}

/// 提供方不可用(未注册/密钥缺失):任务层只看到 MODEL_UNAVAILABLE,
/// 日志必须留下可区分的原因,便于定位"模型不可用"的真实来源。
fn provider_unavailable(reason: &'static str) -> AppError {
    tracing::warn!(
        event = "egress_provider_unavailable",
        phase = reason,
        error_code = "MODEL_UNAVAILABLE",
        "egress provider unavailable"
    );
    model_err(ModelErrorCode::ModelUnavailable).with_detail("reason", reason.into())
}

impl EgressGateway {
    pub fn new(
        grants: Arc<dyn GrantSource>,
        task_gate: Arc<dyn EgressTaskGate>,
        secrets: Arc<dyn SecretStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        max_concurrent: usize,
    ) -> Self {
        Self {
            slots: RwLock::new(HashMap::new()),
            grants,
            task_gate,
            secrets,
            permits: PermitRegistry::new(clock.clone()),
            concurrency: RwLock::new(Arc::new(Semaphore::new(max_concurrent.max(1)))),
            remote_vision_allowed: RwLock::new(false),
            clock,
            ids,
            stats: EgressStats::default(),
        }
    }

    /// 远程图片外发开关(由模型设置驱动;关闭后既有许可也会在消费时被拒)。
    pub fn set_remote_vision_allowed(&self, allowed: bool) {
        *self.remote_vision_allowed.write().unwrap() = allowed;
    }

    fn remote_vision_allowed(&self) -> bool {
        *self.remote_vision_allowed.read().unwrap()
    }

    /// 含图片的载荷:本地按既有本地规则放行;远程必须同时具备全局开关
    /// 与 Vision 用途授权。纯文本载荷不受影响。
    fn ensure_vision_allowed(
        &self,
        document_id: &str,
        location: ProcessingLocation,
        provider_id: &str,
        parts: &[EgressPayloadPart],
    ) -> Result<(), AppError> {
        if !parts.iter().any(|part| part.attachment_digest.is_some()) {
            return Ok(());
        }
        if location == ProcessingLocation::Local {
            return Ok(());
        }
        if !self.remote_vision_allowed() {
            return Err(egress_denied("remote_vision_not_allowed"));
        }
        if !self
            .grants
            .is_granted(document_id, location, provider_id, ModelPurpose::Vision)?
        {
            return Err(model_err(ModelErrorCode::PrivacyGrantRequired)
                .with_detail("reason", "vision_grant_required".into()));
        }
        Ok(())
    }

    /// 服务层在设置变化后重建提供方槽位(唯一注册入口)。
    /// 槽位重建即吊销全部未消费许可:目的地/模型可能已变,重试须重新签发。
    pub fn refresh_providers(
        &self,
        slots: Vec<(String, Arc<dyn ModelProvider>, ProviderTransport)>,
        max_concurrent: usize,
    ) {
        {
            let mut map = self.slots.write().unwrap();
            map.clear();
            for (id, provider, transport) in slots {
                map.insert(
                    id,
                    ProviderSlot {
                        provider,
                        transport,
                    },
                );
            }
        }
        self.permits.revoke_all();
        *self.concurrency.write().unwrap() = Arc::new(Semaphore::new(max_concurrent.max(1)));
    }

    pub fn provider_ids(&self) -> Vec<String> {
        self.slots.read().unwrap().keys().cloned().collect()
    }

    pub fn provider(&self, id: &str) -> Option<Arc<dyn ModelProvider>> {
        self.slots
            .read()
            .unwrap()
            .get(id)
            .map(|s| s.provider.clone())
    }

    /// 能力探测:不携带任何用户内容,不需要许可;远程探测单独计数。
    pub async fn probe(&self, provider_id: &str) -> Result<ProbeResult, AppError> {
        let slot = {
            let map = self.slots.read().unwrap();
            map.get(provider_id)
                .map(|s| (s.provider.clone(), s.transport.clone()))
        };
        let Some((provider, mut transport)) = slot else {
            self.stats.denied.fetch_add(1, Ordering::SeqCst);
            return Err(provider_unavailable("provider_not_registered"));
        };
        if let ProcessingLocation::Remote = provider.location() {
            match self.secrets.get_secret(provider_id) {
                Ok(Some(secret)) => transport.bearer = Some(secret),
                Ok(None) => {
                    self.stats.denied.fetch_add(1, Ordering::SeqCst);
                    return Err(provider_unavailable("secret_missing"));
                }
                Err(e) => return Err(e),
            }
            self.stats.remote_probes.fetch_add(1, Ordering::SeqCst);
        }
        provider
            .probe(transport)
            .await
            .map_err(|e| e.to_app_error())
    }

    /// 签发短期许可:授权/任务/密钥前置校验通过后注册内存许可。
    pub fn issue_permit(&self, spec: &EgressRequestSpec) -> Result<EgressPermit, AppError> {
        let location = {
            let map = self.slots.read().unwrap();
            map.get(&spec.provider_id)
                .map(|s| s.provider.location())
                .ok_or_else(|| {
                    self.stats.denied.fetch_add(1, Ordering::SeqCst);
                    provider_unavailable("provider_not_registered")
                })?
        };
        if spec.requires_active_task()
            && !self
                .task_gate
                .egress_allowed(&spec.task_id, &spec.attempt_id)?
        {
            self.stats.denied.fetch_add(1, Ordering::SeqCst);
            return Err(egress_denied("task_not_active"));
        }
        if !self
            .grants
            .is_granted(&spec.document_id, location, &spec.provider_id, spec.purpose)?
        {
            self.stats.denied.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(
                event = "egress_grant_required",
                phase = "issue_permit",
                error_code = "PRIVACY_GRANT_REQUIRED",
                "egress grant missing"
            );
            return Err(model_err(ModelErrorCode::PrivacyGrantRequired));
        }
        if let Err(e) =
            self.ensure_vision_allowed(&spec.document_id, location, &spec.provider_id, &spec.parts)
        {
            self.stats.denied.fetch_add(1, Ordering::SeqCst);
            return Err(e);
        }
        if let ProcessingLocation::Remote = location {
            match self.secrets.get_secret(&spec.provider_id) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.stats.denied.fetch_add(1, Ordering::SeqCst);
                    return Err(provider_unavailable("secret_missing"));
                }
                Err(e) => return Err(e),
            }
        }
        let mut allowed: Vec<String> = spec.parts.iter().map(|p| p.source_id.clone()).collect();
        allowed.sort();
        allowed.dedup();
        let permit = EgressPermit {
            permit_id: self.ids.new_token(),
            task_id: spec.task_id.clone(),
            attempt_id: spec.attempt_id.clone(),
            document_id: spec.document_id.clone(),
            snapshot_id: spec.snapshot_id.clone(),
            purpose: spec.purpose,
            location,
            provider_id: spec.provider_id.clone(),
            model_id: spec.model_id.clone(),
            allowed_source_ids: allowed,
            scope: spec.scope,
            payload_hash: payload_hash_of(
                &spec.task_id,
                &spec.attempt_id,
                &spec.snapshot_id,
                spec.purpose,
                location,
                &spec.provider_id,
                &spec.model_id,
                &spec.parts,
            ),
            expires_at: self.clock.now_ms() + EGRESS_PERMIT_TTL_MS,
        };
        let permit_id = permit.permit_id.clone();
        self.permits.issue(&permit_id, permit.clone());
        Ok(permit)
    }

    /// 消费许可并发送生成请求(单次消费;重试需要新许可)。
    pub async fn send_generation(
        &self,
        permit_id: &str,
        spec: &EgressRequestSpec,
        messages: Vec<ProviderMessage>,
        json_schema: Option<serde_json::Value>,
        cancel: CancellationToken,
        on_delta: DeltaSink,
    ) -> Result<GenerateSuccess, AppError> {
        let (permit, provider, transport) = match self.consume_and_verify(permit_id, spec) {
            Ok(v) => v,
            Err(e) => {
                self.stats.denied.fetch_add(1, Ordering::SeqCst);
                return Err(e);
            }
        };
        self.count_request(&permit);
        let request = GenerateRequest {
            model_id: permit.model_id.clone(),
            messages,
            json_schema,
        };
        self.dispatch(move || provider.generate(transport, request, cancel, on_delta))
            .await
    }

    /// 消费许可并发送带工具的单步生成(P2 Agent)。
    /// 与 `send_generation` 共用 `consume_and_verify`/计数/并发闸;
    /// 每步独立许可,不得复用上一次 permit。
    #[allow(clippy::too_many_arguments)]
    pub async fn send_generation_with_tools(
        &self,
        permit_id: &str,
        spec: &EgressRequestSpec,
        messages: Vec<ProviderMessage>,
        tools: Vec<ToolSpec>,
        json_schema: Option<serde_json::Value>,
        cancel: CancellationToken,
        on_delta: DeltaSink,
    ) -> Result<GenerateWithToolsSuccess, AppError> {
        let (permit, provider, transport) = match self.consume_and_verify(permit_id, spec) {
            Ok(v) => v,
            Err(e) => {
                self.stats.denied.fetch_add(1, Ordering::SeqCst);
                return Err(e);
            }
        };
        self.count_request(&permit);
        let request = GenerateWithToolsRequest {
            model_id: permit.model_id.clone(),
            messages,
            tools,
            json_schema,
        };
        self.dispatch(move || provider.generate_with_tools(transport, request, cancel, on_delta))
            .await
    }

    /// 消费许可并发送嵌入批次。
    pub async fn send_embedding(
        &self,
        permit_id: &str,
        spec: &EgressRequestSpec,
        cancel: CancellationToken,
    ) -> Result<EmbedSuccess, AppError> {
        let (permit, provider, transport) = match self.consume_and_verify(permit_id, spec) {
            Ok(v) => v,
            Err(e) => {
                self.stats.denied.fetch_add(1, Ordering::SeqCst);
                return Err(e);
            }
        };
        self.count_request(&permit);
        let request = EmbedRequest {
            model_id: permit.model_id.clone(),
            texts: spec.parts.iter().map(|p| p.text.clone()).collect(),
            expected_dimensions: transport.embed_dimensions,
        };
        self.dispatch(move || provider.embed(transport, request, cancel))
            .await
    }

    fn count_request(&self, permit: &EgressPermit) {
        match permit.location {
            ProcessingLocation::Remote => {
                self.stats.remote_requests.fetch_add(1, Ordering::SeqCst);
            }
            ProcessingLocation::Local => {
                self.stats.local_requests.fetch_add(1, Ordering::SeqCst);
            }
        }
        match permit.purpose {
            ModelPurpose::Generate | ModelPurpose::Vision => {
                self.stats.generate_requests.fetch_add(1, Ordering::SeqCst);
            }
            ModelPurpose::Embed => {
                self.stats.embed_requests.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// 消费许可 + 复核(哈希/来源/授权/任务/目的地)。任何失败:零发送。
    fn consume_and_verify(
        &self,
        permit_id: &str,
        spec: &EgressRequestSpec,
    ) -> Result<(EgressPermit, Arc<dyn ModelProvider>, ProviderTransport), AppError> {
        let permit = match self.permits.consume(permit_id) {
            Ok(permit) => permit,
            Err(error) => {
                tracing::warn!(
                    event = "egress_permit_rejected",
                    phase = "consume",
                    error_code = error.code.as_str(),
                    "egress permit rejected"
                );
                return Err(error);
            }
        };
        let slot = {
            let map = self.slots.read().unwrap();
            map.get(&permit.provider_id)
                .map(|s| (s.provider.clone(), s.transport.clone()))
        };
        let Some((provider, mut transport)) = slot else {
            return Err(provider_unavailable("provider_not_registered"));
        };
        // 载荷哈希重算比对:任何字段/文本/模型替换都会失配。
        let recomputed = payload_hash_of(
            &permit.task_id,
            &permit.attempt_id,
            &permit.snapshot_id,
            permit.purpose,
            permit.location,
            &permit.provider_id,
            &spec.model_id,
            &spec.parts,
        );
        if recomputed != permit.payload_hash {
            return Err(egress_denied("payload_mismatch"));
        }
        // 允许来源集合:请求来源必须是许可允许集合的子集(最小来源)。
        for part in &spec.parts {
            if !permit.allowed_source_ids.contains(&part.source_id) {
                return Err(egress_denied("source_not_allowed"));
            }
        }
        // 授权与任务状态复核(签发后可能被撤回/取消)。
        if !self.grants.is_granted(
            &permit.document_id,
            permit.location,
            &permit.provider_id,
            permit.purpose,
        )? {
            tracing::warn!(
                event = "egress_grant_required",
                phase = "consume",
                error_code = "PRIVACY_GRANT_REQUIRED",
                "egress grant revoked before send"
            );
            return Err(model_err(ModelErrorCode::PrivacyGrantRequired)
                .with_detail("reason", "grant_revoked".into()));
        }
        self.ensure_vision_allowed(
            &permit.document_id,
            permit.location,
            &permit.provider_id,
            &spec.parts,
        )?;
        // 文档作用域的许可在消费时不复核任务(签发时已按同一作用域判定)。
        if permit.scope == EgressScope::Task
            && !self
                .task_gate
                .egress_allowed(&permit.task_id, &permit.attempt_id)?
        {
            return Err(egress_denied("task_not_active"));
        }
        // 目的地与许可一致;远程在发送前取密钥注入。
        if provider.location() != permit.location {
            return Err(egress_denied("provider_location_changed"));
        }
        if let ProcessingLocation::Remote = permit.location {
            match self.secrets.get_secret(&permit.provider_id) {
                Ok(Some(secret)) => transport.bearer = Some(secret),
                Ok(None) => {
                    return Err(provider_unavailable("secret_missing"));
                }
                Err(e) => return Err(e),
            }
        }
        Ok((permit, provider, transport))
    }

    /// 并发受限地执行适配器调用(远程请求并发上限来自设置)。
    async fn dispatch<T, F, Fut>(&self, make: F) -> Result<T, AppError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, ProviderError>>,
    {
        let semaphore = self.concurrency.read().unwrap().clone();
        let _guard = semaphore
            .acquire()
            .await
            .map_err(|_| AppError::internal("egress-semaphore-closed"))?;
        make().await.map_err(|e| e.to_app_error())
    }

    /// 撤回授权时按文档+提供方+用途吊销未消费许可(立即阻断在途重试)。
    pub fn revoke_permits_for(
        &self,
        document_id: &str,
        provider_id: &str,
        purpose: ModelPurpose,
    ) -> usize {
        self.permits.revoke_where(|p| {
            p.document_id == document_id && p.provider_id == provider_id && p.purpose == purpose
        })
    }

    pub fn revoke_all_permits(&self) -> usize {
        self.permits.revoke_all()
    }

    /// 密钥删除等提供方级失效:吊销该提供方全部未消费许可。
    pub fn revoke_permits_for_provider(&self, provider_id: &str) -> usize {
        self.permits.revoke_where(|p| p.provider_id == provider_id)
    }

    pub fn active_permits(&self) -> usize {
        self.permits.len_active()
    }

    /// 未授权请求计数必须为零(A2/E2E-13 的断言入口)。
    pub fn remote_request_count(&self) -> u64 {
        self.stats.remote_requests.load(Ordering::SeqCst)
    }

    pub fn snapshot_stats(&self) -> EgressStatsSnapshot {
        EgressStatsSnapshot {
            remote_requests: self.stats.remote_requests.load(Ordering::SeqCst),
            local_requests: self.stats.local_requests.load(Ordering::SeqCst),
            generate_requests: self.stats.generate_requests.load(Ordering::SeqCst),
            embed_requests: self.stats.embed_requests.load(Ordering::SeqCst),
            remote_probes: self.stats.remote_probes.load(Ordering::SeqCst),
            denied: self.stats.denied.load(Ordering::SeqCst),
        }
    }
}

/// 便捷构造:把载荷来源转为用户消息(协议角色由调用方按需覆写)。
pub fn messages_from_parts(parts: &[EgressPayloadPart]) -> Vec<ProviderMessage> {
    parts
        .iter()
        .map(|p| ProviderMessage::text(super::provider::ProviderMessageRole::User, p.text.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_snapshot_shape() {
        let stats = EgressStats::default();
        stats.remote_requests.fetch_add(3, Ordering::SeqCst);
        stats.denied.fetch_add(2, Ordering::SeqCst);
        assert_eq!(stats.remote_requests.load(Ordering::SeqCst), 3);
        assert_eq!(stats.denied.load(Ordering::SeqCst), 2);
    }
}
