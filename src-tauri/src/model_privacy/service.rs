//! M08 应用服务门面(《M08》§3–§9 的编排层)。
//!
//! 组装:设置(原子 JSON)→ 提供方工厂 → EgressGateway(唯一出口);
//! 授权/撤回联动许可吊销与队列任务取消;清除协调与诊断在此挂接。
//! 模型不可用只降级依赖任务,任何失败都不是应用启动失败原因(§1)。

use std::path::PathBuf;
use std::sync::Arc;

use crate::persistence::task::TaskService;
use crate::persistence::{db_app_error, Db};
use crate::shared::dto::TaskStatus;
use crate::shared::events::{DomainEvent, DomainEventType, EventSink};
use crate::shared::{AppError, Clock, IdGenerator};

use super::clear::{ClearCoordinator, TaskCancelPort};
use super::diagnostics::DiagnosticsCollector;
use super::gateway::{EgressGateway, EgressRequestSpec, EgressTaskGate, GrantSource};
use super::grants::{GrantInput, GrantStore};
use super::models::*;
use super::ollama::OllamaProvider;
use super::openai_compatible::OpenAiCompatibleProvider;
use super::provider::{ModelProvider, ProviderTransport};
use super::secrets::SecretStore;
use super::settings::{
    settings_invalid_err, settings_io_err, settings_revision_conflict, ModelSettingsPatch,
    ModelSettingsStore, ModelSettingsV1, ModelSettingsView, ProviderConfigRecord,
};

/// 提供方工厂端口:设置记录 → (适配器, 传输句柄)。
/// 生产用 `DefaultProviderFactory`;测试注入假工厂。
pub trait ProviderFactory: Send + Sync {
    fn build(
        &self,
        config: &ProviderConfigRecord,
        settings: &ModelSettingsV1,
    ) -> Option<(Arc<dyn ModelProvider>, ProviderTransport)>;
}

/// 生产工厂:Ollama(本地回环)与 OpenAI 兼容(远程)适配器。
/// HTTP 客户端统一在此构建:禁自动重定向、禁代理环境变量继承。
pub struct DefaultProviderFactory {
    client: reqwest::Client,
}

impl DefaultProviderFactory {
    pub fn new() -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|_| AppError::internal("http-client-build"))?;
        Ok(Self { client })
    }
}

impl Default for DefaultProviderFactory {
    fn default() -> Self {
        Self::new().expect("reqwest client build cannot fail with these options")
    }
}

impl ProviderFactory for DefaultProviderFactory {
    fn build(
        &self,
        config: &ProviderConfigRecord,
        settings: &ModelSettingsV1,
    ) -> Option<(Arc<dyn ModelProvider>, ProviderTransport)> {
        let (provider, base_url): (Arc<dyn ModelProvider>, String) = match config.kind {
            ProviderKind::Ollama => {
                let url = config
                    .base_url
                    .clone()
                    .unwrap_or_else(|| settings.ollama_base_url.clone());
                (
                    Arc::new(OllamaProvider::new(&config.id, &config.display_name)),
                    url,
                )
            }
            ProviderKind::OpenaiCompatible => {
                let url = config.base_url.clone()?;
                (
                    Arc::new(OpenAiCompatibleProvider::new(
                        &config.id,
                        &config.display_name,
                    )),
                    url,
                )
            }
        };
        let transport = ProviderTransport {
            http: self.client.clone(),
            base_url,
            bearer: None,
            max_response_bytes: settings.max_response_bytes.clamp(1024, 64 * 1024 * 1024) as usize,
            request_timeout_ms: settings.request_timeout_ms.clamp(1_000, 600_000) as u64,
            embed_dimensions: config.embed_dimensions,
        };
        Some((provider, transport))
    }
}

// —— M01 任务端口适配 ——

pub struct TaskServiceEgressGate {
    tasks: Arc<TaskService>,
}

impl EgressTaskGate for TaskServiceEgressGate {
    fn egress_allowed(&self, task_id: &str, attempt_id: &str) -> Result<bool, AppError> {
        let Some(task) = self.tasks.get_task(task_id)? else {
            return Ok(false);
        };
        Ok(
            matches!(task.status, TaskStatus::Queued | TaskStatus::Running)
                && task.current_attempt_id.as_deref() == Some(attempt_id)
                && !task.cancel_requested,
        )
    }
}

pub struct TaskServiceCancelPort {
    tasks: Arc<TaskService>,
    db: Arc<Db>,
}

impl TaskCancelPort for TaskServiceCancelPort {
    fn cancel_active_for_document(&self, document_id: &str) -> Result<i64, AppError> {
        let page = self
            .tasks
            .list_tasks(Some(document_id.to_string()), true, 500, None)?;
        let mut n = 0;
        for task in page.items {
            if self
                .tasks
                .request_cancel(&task.id, task.state_version)
                .is_ok()
            {
                n += 1;
            }
        }
        Ok(n)
    }
}

impl TaskServiceCancelPort {
    /// 撤回联动:取消本文档中"当前 Attempt 属于该提供方"的活动任务,
    /// 以及尚未分配 Attempt、但按用途参考会走该提供方的排队任务。
    pub fn cancel_active_for_provider(
        &self,
        document_id: &str,
        provider_id: &str,
        purpose: ModelPurpose,
        reference_provider: Option<&str>,
    ) -> Result<i64, AppError> {
        let did = document_id.to_string();
        let pid = provider_id.to_string();
        // 有 Attempt:按 provider_id 匹配;无 Attempt:按任务类型与参考匹配。
        let remote_types: Vec<String> = match purpose {
            ModelPurpose::Generate | ModelPurpose::Vision => {
                vec!["ask_question".into(), "generate_artifact".into()]
            }
            ModelPurpose::Embed => vec!["build_embedding".into()],
        };
        let rows: Vec<(String, i64)> = self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT t.id, t.state_version, a.provider_id, t.task_type FROM task t \
                     LEFT JOIN task_attempt a ON a.id = t.current_attempt_id \
                     WHERE t.document_id=?1 AND t.status IN ('queued','running')",
                )
                .map_err(|e| db_app_error(&e))?;
            let out = stmt
                .query_map([&did], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            Ok(out
                .into_iter()
                .filter(
                    |(_, _, attempt_provider, task_type)| match attempt_provider {
                        Some(p) => p == &pid,
                        None => {
                            remote_types.contains(task_type)
                                && reference_provider == Some(pid.as_str())
                        }
                    },
                )
                .map(|(id, sv, _, _)| (id, sv))
                .collect())
        })?;
        let mut n = 0;
        for (task_id, state_version) in rows {
            if self.tasks.request_cancel(&task_id, state_version).is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }
}

// —— 服务 ——

/// 日志用角色名(静态、非敏感)。
fn generation_role_phase(role: GenerationRole) -> &'static str {
    match role {
        GenerationRole::Fast => "fast",
        GenerationRole::Quality => "quality",
        GenerationRole::Translation => "translation",
    }
}

pub struct ModelPrivacyService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    events: Arc<dyn EventSink>,
    cancel_port: Arc<TaskServiceCancelPort>,
    settings: Arc<ModelSettingsStore>,
    secrets: Arc<dyn SecretStore>,
    grants: Arc<GrantStore>,
    gateway: Arc<EgressGateway>,
    clear: Arc<ClearCoordinator>,
    diagnostics: Arc<DiagnosticsCollector>,
    provider_factory: Arc<dyn ProviderFactory>,
    data_dir: PathBuf,
    status_cache: std::sync::Mutex<Option<ModelStatusView>>,
    /// remote_secret_set 幂等缓冲(key → 结果)。
    secret_idempotency: std::sync::Mutex<std::collections::HashMap<String, SecretState>>,
}

impl ModelPrivacyService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        events: Arc<dyn EventSink>,
        tasks: Arc<TaskService>,
        config_dir: &std::path::Path,
        data_dir: &std::path::Path,
        secrets: Arc<dyn SecretStore>,
        provider_factory: Arc<dyn ProviderFactory>,
    ) -> Result<Self, AppError> {
        let settings = Arc::new(ModelSettingsStore::open(config_dir));
        let grants = Arc::new(GrantStore::new(Arc::clone(&db), clock.clone(), ids.clone()));
        let task_gate = Arc::new(TaskServiceEgressGate {
            tasks: Arc::clone(&tasks),
        });
        let initial = settings.load().map_err(settings_io_err)?;
        let gateway = Arc::new(EgressGateway::new(
            grants.clone() as Arc<dyn GrantSource>,
            task_gate,
            secrets.clone(),
            clock.clone(),
            ids.clone(),
            initial.max_concurrent_requests.max(1) as usize,
        ));
        let clear = Arc::new(ClearCoordinator::new(
            Arc::clone(&db),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let cancel_port = Arc::new(TaskServiceCancelPort {
            tasks: Arc::clone(&tasks),
            db: Arc::clone(&db),
        });
        clear.set_task_canceller(cancel_port.clone());
        let diagnostics = Arc::new(DiagnosticsCollector::new(Arc::clone(&db), clock.clone()));
        let service = Self {
            db,
            clock,
            events,
            cancel_port,
            settings,
            secrets,
            grants,
            gateway,
            clear,
            diagnostics,
            provider_factory,
            data_dir: data_dir.to_path_buf(),
            status_cache: std::sync::Mutex::new(None),
            secret_idempotency: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        service.refresh_providers()?;
        Ok(service)
    }

    /// 业务模块(M04/M09)的唯一模型通道:签发许可走 gateway。
    pub fn gateway(&self) -> Arc<EgressGateway> {
        self.gateway.clone()
    }

    // —— 组装根端口适配器(M09 capability)的同步输入 ——

    /// 角色化生成模型;未配置时按回退链选择:
    /// fast → quality → generate;quality → generate → fast;
    /// translation → quality → generate → fast。
    pub fn reference_for(&self, role: GenerationRole) -> Option<(String, String)> {
        let settings = match self.settings.load() {
            Ok(settings) => settings,
            Err(_) => {
                // 设置读取失败会让上层看到"模型不可用";日志留下真实原因。
                tracing::warn!(
                    event = "model_reference_unavailable",
                    phase = "settings_load_failed",
                    error_code = "MODEL_UNAVAILABLE",
                    "model reference lookup failed"
                );
                return None;
            }
        };
        let to_pair = |r: &Option<super::settings::ModelReferenceRecord>| {
            r.as_ref()
                .map(|r| (r.provider_id.clone(), r.model_id.clone()))
        };
        let generate = to_pair(&settings.generate_reference);
        let fast = to_pair(&settings.fast_reference);
        let quality = to_pair(&settings.quality_reference);
        let translation = to_pair(&settings.translation_reference);
        let picked = match role {
            GenerationRole::Fast => fast.or_else(|| quality.clone()).or(generate),
            GenerationRole::Quality => quality.or_else(|| generate.clone()).or(fast),
            GenerationRole::Translation => translation
                .or_else(|| quality.clone())
                .or_else(|| generate.clone())
                .or(fast),
        };
        if picked.is_none() {
            tracing::warn!(
                event = "model_reference_unavailable",
                phase = generation_role_phase(role),
                error_code = "MODEL_UNAVAILABLE",
                "model reference not configured"
            );
        }
        picked
    }

    pub fn reference_fast(&self) -> Option<(String, String)> {
        self.reference_for(GenerationRole::Fast)
    }

    pub fn reference_quality(&self) -> Option<(String, String)> {
        self.reference_for(GenerationRole::Quality)
    }

    /// 2F:当前生成模型的上下文窗口(按角色回退链取 fast/quality)。
    /// 读能力表;未记录时返回内置保守默认,并记录一次 auto 探测。
    pub fn context_window_for(&self, role: GenerationRole) -> Option<usize> {
        let (provider_id, model_id) = self.reference_for(role)?;
        let capability = self.model_capability(&provider_id, &model_id)?;
        Some(capability.prompt_window())
    }

    /// 读取(必要时记录)某模型的能力。失败返回 None(预算回退默认值)。
    pub fn model_capability(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Option<crate::model_privacy::capability::ModelCapability> {
        let service =
            crate::model_privacy::capability::ModelCapabilityService::new(Arc::clone(&self.db));
        match service.get(provider_id, model_id) {
            Ok(capability) => {
                // 表里没有记录时(来源 auto 且未持久化)顺手落库,便于设置页展示。
                let _ = service.record_auto(&capability);
                Some(capability)
            }
            Err(error) => {
                tracing::warn!(error_code = error.code, "model capability read failed");
                None
            }
        }
    }

    /// 手动修正模型能力(设置 UI);非法输入直接拒绝。
    pub fn set_model_capability(
        &self,
        input: crate::model_privacy::capability::CapabilityOverride,
    ) -> Result<crate::model_privacy::capability::ModelCapability, AppError> {
        let service =
            crate::model_privacy::capability::ModelCapabilityService::new(Arc::clone(&self.db));
        service.set_manual(input)
    }

    /// 兼容旧调用:默认生成角色等同质量角色(未配置 quality 时回退 generate)。
    pub fn reference_generation(&self) -> Option<(String, String)> {
        self.reference_quality()
    }

    /// 参考嵌入模型 (providerId, modelId);未配置返回 None。
    pub fn reference_embedding(&self) -> Option<(String, String)> {
        self.settings
            .load()
            .ok()?
            .embed_reference
            .map(|r| (r.provider_id, r.model_id))
    }

    /// 生成授权裁决:本地提供方恒为可用(SEC-02,不经网关外发);
    /// 远程提供方按有效授权记录判定,撤回后立即为 false。
    pub fn generation_authorized(&self, document_id: &str, provider_id: &str) -> bool {
        let Some(provider) = self.gateway.provider(provider_id) else {
            tracing::warn!(
                event = "generation_capability_unavailable",
                phase = "provider_not_registered",
                error_code = "MODEL_UNAVAILABLE",
                "generation provider missing from gateway"
            );
            return false;
        };
        match provider.location() {
            ProcessingLocation::Local => true,
            ProcessingLocation::Remote => self
                .grants
                .active_grant(
                    document_id,
                    ProcessingLocation::Remote,
                    provider_id,
                    ModelPurpose::Generate,
                )
                .map(|g| g.is_some())
                .unwrap_or(false),
        }
    }

    /// 当前文档的图片发送就绪状态(本地恒可用;远程需开关 + Vision 授权)。
    pub fn vision_status(&self, document_id: &str) -> Result<VisionStatusView, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            if !super::grants::document_exists_tx(conn, &did)? {
                return Err(AppError::new(
                    crate::shared::CoreErrorCode::DocNotFound,
                    crate::shared::new_trace_id(),
                ));
            }
            Ok(())
        })?;
        let settings = self.settings.load().map_err(settings_io_err)?;
        let effective_reference = settings
            .quality_reference
            .clone()
            .or_else(|| settings.generate_reference.clone())
            .or_else(|| settings.fast_reference.clone());
        let Some(reference) = effective_reference.as_ref() else {
            return Ok(VisionStatusView {
                schema_version: "vision_status.v1",
                document_id: document_id.to_string(),
                provider_id: None,
                model_id: None,
                processing_location: None,
                remote_vision_allowed: settings.allow_remote_vision,
                vision_granted: false,
                ready: false,
                reason: Some(VisionBlockedReason::GenerateReferenceMissing),
            });
        };
        let Some(provider) = self.gateway.provider(&reference.provider_id) else {
            return Ok(VisionStatusView {
                schema_version: "vision_status.v1",
                document_id: document_id.to_string(),
                provider_id: Some(reference.provider_id.clone()),
                model_id: Some(reference.model_id.clone()),
                processing_location: None,
                remote_vision_allowed: settings.allow_remote_vision,
                vision_granted: false,
                ready: false,
                reason: Some(VisionBlockedReason::ProviderUnavailable),
            });
        };
        let location = provider.location();
        let (allowed, granted, ready, reason) = match location {
            ProcessingLocation::Local => (false, true, true, None),
            ProcessingLocation::Remote => {
                let allowed = settings.allow_remote_vision;
                let granted = self
                    .grants
                    .active_grant(
                        document_id,
                        ProcessingLocation::Remote,
                        &reference.provider_id,
                        ModelPurpose::Vision,
                    )?
                    .is_some();
                let (ready, reason) = if !allowed {
                    (false, Some(VisionBlockedReason::RemoteVisionDisabled))
                } else if !granted {
                    (false, Some(VisionBlockedReason::VisionGrantRequired))
                } else {
                    (true, None)
                };
                (allowed, granted, ready, reason)
            }
        };
        Ok(VisionStatusView {
            schema_version: "vision_status.v1",
            document_id: document_id.to_string(),
            provider_id: Some(reference.provider_id.clone()),
            model_id: Some(reference.model_id.clone()),
            processing_location: Some(location),
            remote_vision_allowed: allowed,
            vision_granted: granted,
            ready,
            reason,
        })
    }

    /// 用户明确允许图片外发:打开全局开关,并为远程提供方授权本文件的 Vision 用途。
    /// 返回刷新后的状态;未配置生成模型/提供方时返回可解释的未就绪状态。
    pub fn vision_allow_remote(
        &self,
        document_id: &str,
        idempotency_key: &str,
    ) -> Result<VisionStatusView, AppError> {
        let mut settings = self.settings.load().map_err(settings_io_err)?;
        let Some(reference) = settings
            .quality_reference
            .clone()
            .or_else(|| settings.generate_reference.clone())
            .or_else(|| settings.fast_reference.clone())
        else {
            return self.vision_status(document_id);
        };
        let Some(provider) = self.gateway.provider(&reference.provider_id) else {
            return self.vision_status(document_id);
        };
        if provider.location() == ProcessingLocation::Remote {
            if !settings.allow_remote_vision {
                settings.allow_remote_vision = true;
                settings.revision += 1;
                self.save_settings(settings)?;
            }
            self.privacy_grant(
                document_id,
                ProcessingLocation::Remote,
                &reference.provider_id,
                ModelPurpose::Vision,
                PRIVACY_POLICY_VERSION,
                idempotency_key,
            )?;
        }
        self.vision_status(document_id)
    }

    pub fn clear_coordinator(&self) -> Arc<ClearCoordinator> {
        self.clear.clone()
    }

    /// 依据设置重建网关槽位(设置变化/启动时)。
    fn refresh_providers(&self) -> Result<(), AppError> {
        let settings = self.settings.load().map_err(settings_io_err)?;
        settings.validate().map_err(settings_invalid_err)?;
        let mut slots = Vec::new();
        for config in settings.providers.iter().filter(|p| p.enabled) {
            if let Some((provider, transport)) = self.provider_factory.build(config, &settings) {
                slots.push((config.id.clone(), provider, transport));
            }
        }
        self.gateway
            .refresh_providers(slots, settings.max_concurrent_requests.max(1) as usize);
        self.gateway
            .set_remote_vision_allowed(settings.allow_remote_vision);
        Ok(())
    }

    fn save_settings(&self, next: ModelSettingsV1) -> Result<ModelSettingsV1, AppError> {
        next.validate().map_err(settings_invalid_err)?;
        self.settings.save(&next).map_err(settings_io_err)?;
        self.refresh_providers()?;
        Ok(next)
    }

    // —— 模型状态(《M08》§3)——

    /// 探测全部启用提供方并合成能力状态。探测失败只影响能力显示,
    /// 不阻塞启动(A1)。
    pub async fn model_get_status(&self) -> Result<ModelStatusView, AppError> {
        self.refresh_providers()?;
        let settings = self.settings.load().map_err(settings_io_err)?;
        let mut providers: Vec<ModelProviderDescriptor> = Vec::new();
        let mut reachable_by_provider: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();
        let mut models_by_provider: std::collections::HashMap<String, Vec<ModelDescriptor>> =
            std::collections::HashMap::new();
        for config in settings.providers.iter().filter(|p| p.enabled) {
            let secret_configured = match config.kind {
                ProviderKind::Ollama => true,
                ProviderKind::OpenaiCompatible => {
                    matches!(self.secrets.get_secret(&config.id), Ok(Some(_)))
                }
            };
            let configured = secret_configured
                && match config.kind {
                    ProviderKind::Ollama => true,
                    ProviderKind::OpenaiCompatible => config.base_url.is_some(),
                };
            let (reachable, models) = match self.gateway.probe(&config.id).await {
                Ok(probe) => (probe.reachable && configured, probe.models),
                Err(_) => (false, Vec::new()),
            };
            reachable_by_provider.insert(config.id.clone(), reachable);
            models_by_provider.insert(config.id.clone(), models.clone());
            let descriptor = self
                .gateway
                .provider(&config.id)
                .map(|p| p.base_descriptor());
            let mut descriptor = descriptor.unwrap_or_else(|| ModelProviderDescriptor {
                id: config.id.clone(),
                kind: config.kind,
                location: config.kind.default_location(),
                display_name: config.display_name.clone(),
                capabilities: Vec::new(),
                configured,
                reachable: false,
                available_models: Vec::new(),
            });
            descriptor.configured = configured;
            descriptor.reachable = reachable;
            descriptor.available_models = models;
            providers.push(descriptor);
        }
        providers.sort_by(|a, b| a.id.cmp(&b.id));
        let status = Self::compute_capability(
            &settings,
            &providers,
            &reachable_by_provider,
            &models_by_provider,
        );
        let mut status = status;
        // 质量模式能力:只读能力表(手动修正优先),不额外探测网络。
        status.quality_capability = self
            .reference_quality()
            .and_then(|(provider_id, model_id)| self.model_capability(&provider_id, &model_id))
            .map(|capability| ModelCapabilityView::from_capability(&capability));
        *self.status_cache.lock().unwrap() = Some(status.clone());
        Ok(status)
    }

    fn compute_capability(
        settings: &ModelSettingsV1,
        providers: &[ModelProviderDescriptor],
        reachable: &std::collections::HashMap<String, bool>,
        models: &std::collections::HashMap<String, Vec<ModelDescriptor>>,
    ) -> ModelStatusView {
        let mut reasons: Vec<ModelDegradationReason> = Vec::new();
        let enabled_providers: Vec<&ProviderConfigRecord> =
            settings.providers.iter().filter(|p| p.enabled).collect();
        if enabled_providers.is_empty() {
            reasons.push(ModelDegradationReason::NoProvider);
        }
        // 远程密钥缺失 → Misconfigured(显式配置不完整)。
        for p in &enabled_providers {
            if matches!(p.kind, ProviderKind::OpenaiCompatible)
                && !settings
                    .secret_metadata
                    .get(&p.id)
                    .map(|m| m.configured)
                    .unwrap_or(false)
            {
                reasons.push(ModelDegradationReason::SecretMissing);
            }
        }
        let role_ok = |reference: &Option<super::settings::ModelReferenceRecord>,
                       reasons: &mut Vec<ModelDegradationReason>|
         -> bool {
            let Some(r) = reference else {
                return false;
            };
            let Some(config) = settings.providers.iter().find(|p| p.id == r.provider_id) else {
                reasons.push(ModelDegradationReason::ReferenceProviderMissing);
                return false;
            };
            if !config.enabled {
                reasons.push(ModelDegradationReason::ReferenceProviderMissing);
                return false;
            }
            if !*reachable.get(&config.id).unwrap_or(&false) {
                reasons.push(ModelDegradationReason::ProviderUnreachable);
                return false;
            }
            if let Some(list) = models.get(&config.id) {
                if !list.is_empty() && !list.iter().any(|m| m.id == r.model_id) {
                    reasons.push(ModelDegradationReason::ReferenceModelMissing);
                    return false;
                }
            }
            true
        };
        let effective_generate = settings
            .quality_reference
            .clone()
            .or_else(|| settings.generate_reference.clone())
            .or_else(|| settings.fast_reference.clone());
        let generate_ok = role_ok(&effective_generate, &mut reasons);
        let embed_ok = role_ok(&settings.embed_reference, &mut reasons);
        if effective_generate.is_none() {
            reasons.push(ModelDegradationReason::GenerateReferenceMissing);
        }
        if settings.embed_reference.is_none() {
            reasons.push(ModelDegradationReason::EmbedReferenceMissing);
        }
        let misconfigured = reasons
            .iter()
            .any(|r| matches!(r, ModelDegradationReason::SecretMissing));
        let capability = if misconfigured {
            ModelCapability::Misconfigured
        } else if generate_ok && embed_ok {
            ModelCapability::Ready
        } else if generate_ok {
            ModelCapability::GenerateOnly
        } else if embed_ok {
            ModelCapability::EmbedOnly
        } else {
            ModelCapability::Unavailable
        };
        reasons.sort_by_key(|r| r.as_str());
        reasons.dedup();
        ModelStatusView {
            schema_version: "model_status.v1",
            capability,
            reasons,
            providers: providers.to_vec(),
            generate_reference: settings
                .generate_reference
                .as_ref()
                .map(|r| ModelReference {
                    provider_id: r.provider_id.clone(),
                    model_id: r.model_id.clone(),
                }),
            fast_reference: settings.fast_reference.as_ref().map(|r| ModelReference {
                provider_id: r.provider_id.clone(),
                model_id: r.model_id.clone(),
            }),
            quality_reference: settings.quality_reference.as_ref().map(|r| ModelReference {
                provider_id: r.provider_id.clone(),
                model_id: r.model_id.clone(),
            }),
            translation_reference: settings.translation_reference.as_ref().map(|r| {
                ModelReference {
                    provider_id: r.provider_id.clone(),
                    model_id: r.model_id.clone(),
                }
            }),
            embed_reference: settings.embed_reference.as_ref().map(|r| ModelReference {
                provider_id: r.provider_id.clone(),
                model_id: r.model_id.clone(),
            }),
            quality_capability: None,
            secrets: settings
                .secret_metadata
                .iter()
                .map(|(id, m)| SecretState {
                    provider_id: id.clone(),
                    configured: m.configured,
                    updated_at: m.updated_at,
                })
                .collect(),
            settings_revision: settings.revision,
        }
    }

    /// 本地模型列表(不包含提示词或密钥;未安装 → 空列表,A1)。
    pub async fn model_list_local_models(&self) -> Result<Vec<ModelDescriptor>, AppError> {
        self.refresh_providers()?;
        let settings = self.settings.load().map_err(settings_io_err)?;
        for config in settings.providers.iter().filter(|p| p.enabled) {
            if matches!(config.kind, ProviderKind::Ollama) {
                if let Ok(probe) = self.gateway.probe(&config.id).await {
                    return Ok(probe.models);
                }
                return Ok(Vec::new());
            }
        }
        Ok(Vec::new())
    }

    /// 设置生成/嵌入参考模型;切换嵌入模型即触发新的 EmbeddingGeneration
    /// 语义(model.changed 事件,M04 消费后重建代次,不混查旧向量)。
    pub async fn model_set_reference(
        &self,
        role: ModelPurpose,
        provider_id: &str,
        model_id: &str,
        expected_revision: i64,
    ) -> Result<ModelStatusView, AppError> {
        let mut settings = self.settings.load().map_err(settings_io_err)?;
        if settings.revision != expected_revision {
            return Err(settings_revision_conflict());
        }
        if !role.is_reference_role() {
            return Err(AppError::new(
                crate::shared::CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "vision_is_not_reference_role".into()));
        }
        let reference = super::settings::ModelReferenceRecord {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
        };
        match role {
            ModelPurpose::Generate => settings.generate_reference = Some(reference),
            ModelPurpose::Embed => settings.embed_reference = Some(reference),
            ModelPurpose::Vision => unreachable!("vision rejected above"),
        }
        settings.revision += 1;
        self.save_settings(settings)?;
        self.emit_model_changed(role, provider_id, model_id);
        self.model_get_status().await
    }

    /// 设置角色化生成模型(fast/quality/translation);不改隐私授权语义。
    pub async fn model_set_generation_role(
        &self,
        role: GenerationRole,
        provider_id: &str,
        model_id: &str,
        expected_revision: i64,
    ) -> Result<ModelStatusView, AppError> {
        let mut settings = self.settings.load().map_err(settings_io_err)?;
        if settings.revision != expected_revision {
            return Err(settings_revision_conflict());
        }
        let reference = super::settings::ModelReferenceRecord {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
        };
        match role {
            GenerationRole::Fast => settings.fast_reference = Some(reference),
            GenerationRole::Quality => settings.quality_reference = Some(reference),
            GenerationRole::Translation => settings.translation_reference = Some(reference),
        }
        settings.revision += 1;
        self.save_settings(settings)?;
        self.emit_model_changed(ModelPurpose::Generate, provider_id, model_id);
        self.model_get_status().await
    }

    // —— 密钥(《M08》§6)——

    pub fn remote_secret_set(
        &self,
        provider_id: &str,
        secret: &str,
        idempotency_key: &str,
    ) -> Result<SecretState, AppError> {
        if let Some(prior) = self.secret_idempotency.lock().unwrap().get(idempotency_key) {
            return Ok(prior.clone());
        }
        // 只写系统凭据;失败即报错,不落任何明文(A4)。
        self.secrets.set_secret(provider_id, secret)?;
        let state = self.record_secret_metadata(provider_id, true)?;
        self.secret_idempotency
            .lock()
            .unwrap()
            .insert(idempotency_key.to_string(), state.clone());
        self.emit_model_changed(ModelPurpose::Generate, provider_id, "");
        Ok(state)
    }

    pub fn remote_secret_delete(
        &self,
        provider_id: &str,
        expected_revision: i64,
    ) -> Result<SecretState, AppError> {
        let settings = self.settings.load().map_err(settings_io_err)?;
        if settings.revision != expected_revision {
            return Err(settings_revision_conflict());
        }
        self.secrets.delete_secret(provider_id)?;
        let state = self.record_secret_metadata(provider_id, false)?;
        // 密钥移除:吊销该提供方全部许可(发送前复核也会因密钥缺失拒绝)。
        self.gateway.revoke_permits_for_provider(provider_id);
        self.emit_model_changed(ModelPurpose::Generate, provider_id, "");
        Ok(state)
    }

    fn record_secret_metadata(
        &self,
        provider_id: &str,
        configured: bool,
    ) -> Result<SecretState, AppError> {
        let mut settings = self.settings.load().map_err(settings_io_err)?;
        let now = self.clock.now_ms();
        settings
            .secret_metadata
            .entry(provider_id.to_string())
            .or_default()
            .configured = configured;
        if configured {
            settings
                .secret_metadata
                .get_mut(provider_id)
                .unwrap()
                .updated_at = Some(now);
        }
        settings.revision += 1;
        let updated_at = settings
            .secret_metadata
            .get(provider_id)
            .and_then(|m| m.updated_at);
        self.save_settings(settings)?;
        Ok(SecretState {
            provider_id: provider_id.to_string(),
            configured,
            updated_at,
        })
    }

    // —— 隐私授权(《M08》§5)——

    pub async fn privacy_get_status(
        &self,
        document_id: &str,
    ) -> Result<PrivacyStatusView, AppError> {
        // 文档必须存在(授权按文档粒度)。
        let did = document_id.to_string();
        self.db.read(move |conn| {
            if !super::grants::document_exists_tx(conn, &did)? {
                return Err(AppError::new(
                    crate::shared::CoreErrorCode::DocNotFound,
                    crate::shared::new_trace_id(),
                ));
            }
            Ok(())
        })?;
        let grants = self.grants.list_for_document(document_id)?;
        // 先在 await 前释放缓存锁,保证命令 future 是 Send。
        let cached = self.status_cache.lock().unwrap().clone();
        let status = match cached {
            Some(s) => s,
            None => self.model_get_status().await?,
        };
        let settings = self.settings.load().map_err(settings_io_err)?;
        let mut checks = Vec::new();
        for descriptor in &status.providers {
            for purpose in [
                ModelPurpose::Generate,
                ModelPurpose::Embed,
                ModelPurpose::Vision,
            ] {
                let granted = match (purpose, descriptor.location) {
                    // 本地链路按既有本地规则恒可用,不需要授权记录。
                    (ModelPurpose::Vision, ProcessingLocation::Local) => true,
                    _ => self
                        .grants
                        .active_grant(document_id, descriptor.location, &descriptor.id, purpose)?
                        .is_some(),
                };
                let referenced = match purpose {
                    ModelPurpose::Generate | ModelPurpose::Vision => [
                        settings.generate_reference.as_ref(),
                        settings.fast_reference.as_ref(),
                        settings.quality_reference.as_ref(),
                        settings.translation_reference.as_ref(),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|r| r.provider_id == descriptor.id),
                    ModelPurpose::Embed => settings
                        .embed_reference
                        .as_ref()
                        .map(|r| r.provider_id == descriptor.id)
                        .unwrap_or(false),
                };
                let model_ready = referenced && descriptor.reachable;
                let capability_ready = model_ready;
                checks.push(GrantCheckView {
                    processing_location: descriptor.location,
                    provider_id: descriptor.id.clone(),
                    purpose,
                    granted,
                    capability_ready,
                    executable: granted && capability_ready,
                    reason: if !granted {
                        Some(GrantCheckReason::GrantRequired)
                    } else if !capability_ready {
                        Some(GrantCheckReason::ProviderUnavailable)
                    } else {
                        None
                    },
                });
            }
        }
        Ok(PrivacyStatusView {
            schema_version: "privacy_status.v1",
            document_id: document_id.to_string(),
            policy_version: PRIVACY_POLICY_VERSION.to_string(),
            checks,
            grants: grants.iter().map(|g| g.to_view()).collect(),
        })
    }

    pub fn privacy_grant(
        &self,
        document_id: &str,
        location: ProcessingLocation,
        provider_id: &str,
        purpose: ModelPurpose,
        policy_version: &str,
        idempotency_key: &str,
    ) -> Result<PrivacyGrantView, AppError> {
        let row = self.grants.grant(&GrantInput {
            document_id: document_id.to_string(),
            location,
            provider_id: provider_id.to_string(),
            purpose,
            policy_version: policy_version.to_string(),
            idempotency_key: idempotency_key.to_string(),
        })?;
        let view = row.to_view();
        self.emit_privacy_changed("granted", document_id, location, provider_id, purpose);
        Ok(view)
    }

    /// 导入文档后自动授予“默认全部允许”的文档级许可。
    ///
    /// 产品决定(本轮):设置里不再暴露隐私分区,导入即视为同意常规用途;
    /// 但**不绕过网关**——许可、载荷哈希、来源白名单、审计仍全部保留,
    /// 撤回接口也仍在(供隐私清除与诊断使用)。只为已启用的远程提供方
    /// 授予 generate/embed/vision 三种用途;本地提供方恒可用,无需授权。
    pub fn grant_document_defaults(
        &self,
        document_id: &str,
        idempotency_key: &str,
    ) -> Result<usize, AppError> {
        let settings = self.settings.load().map_err(settings_io_err)?;
        let remote_providers: Vec<String> = settings
            .providers
            .iter()
            .filter(|provider| provider.enabled)
            .filter(|provider| {
                self.gateway
                    .provider(&provider.id)
                    .map(|p| p.location() == ProcessingLocation::Remote)
                    .unwrap_or(false)
            })
            .map(|provider| provider.id.clone())
            .collect();
        let mut granted = 0usize;
        for provider_id in remote_providers {
            for purpose in [
                ModelPurpose::Generate,
                ModelPurpose::Embed,
                ModelPurpose::Vision,
            ] {
                let key = format!("{idempotency_key}:{provider_id}:{}", purpose.as_str());
                match self.privacy_grant(
                    document_id,
                    ProcessingLocation::Remote,
                    &provider_id,
                    purpose,
                    PRIVACY_POLICY_VERSION,
                    &key,
                ) {
                    Ok(_) => granted += 1,
                    // 单个用途失败不影响导入本身;后续使用时会给出可见说明。
                    Err(error) => tracing::warn!(
                        error_code = error.code,
                        purpose = purpose.as_str(),
                        "auto grant failed (document import continues)"
                    ),
                }
            }
        }
        Ok(granted)
    }

    /// 撤回:立即吊销许可、阻止新任务,并取消已排队/运行的关联远程任务。
    pub fn privacy_revoke(
        &self,
        document_id: &str,
        location: ProcessingLocation,
        provider_id: &str,
        purpose: ModelPurpose,
        idempotency_key: &str,
    ) -> Result<Option<PrivacyGrantView>, AppError> {
        let _ = idempotency_key; // 撤回以当前状态为准;幂等体现在"无有效授权也成功"
        let row = self
            .grants
            .revoke(document_id, location, provider_id, purpose)?;
        self.gateway
            .revoke_permits_for(document_id, provider_id, purpose);
        // 取消关联活动任务:有 Attempt 按 provider 匹配;排队任务按
        // 当前用途参考是否指向该提供方。
        let reference_provider = {
            let settings = self.settings.load().map_err(settings_io_err)?;
            match purpose {
                ModelPurpose::Generate | ModelPurpose::Vision => settings
                    .generate_reference
                    .as_ref()
                    .map(|r| r.provider_id.clone()),
                ModelPurpose::Embed => settings
                    .embed_reference
                    .as_ref()
                    .map(|r| r.provider_id.clone()),
            }
        };
        let cancel_port = self.cancel_port.clone();
        let _ = cancel_port.cancel_active_for_provider(
            document_id,
            provider_id,
            purpose,
            reference_provider.as_deref(),
        );
        self.emit_privacy_changed("revoked", document_id, location, provider_id, purpose);
        Ok(row.map(|r| r.to_view()))
    }

    // —— 清除协调(《M08》§8)——

    pub fn privacy_preview_clear(
        &self,
        document_id: &str,
        categories: &[ClearCategory],
    ) -> Result<ClearPreview, AppError> {
        self.clear.preview_clear(document_id, categories)
    }

    pub fn privacy_execute_clear(
        &self,
        document_id: &str,
        clear_plan_id: &str,
        plan_hash: &str,
        confirmation: &str,
        idempotency_key: &str,
    ) -> Result<ClearResult, AppError> {
        self.clear.execute_clear(
            document_id,
            clear_plan_id,
            plan_hash,
            confirmation,
            idempotency_key,
        )
    }

    // —— 设置(《M08》§6)——

    pub fn settings_get(&self) -> Result<ModelSettingsView, AppError> {
        let settings = self.settings.load().map_err(settings_io_err)?;
        Ok(ModelSettingsView {
            schema_version: "model_settings_view.v1",
            revision: settings.revision,
            settings,
        })
    }

    /// P0 1E:思考记录是否持久化(默认 false;读取失败按 false 处理)。
    pub fn save_reasoning_enabled(&self) -> bool {
        self.settings
            .load()
            .map(|settings| settings.save_reasoning)
            .unwrap_or(false)
    }

    pub fn settings_update(
        &self,
        patch: &ModelSettingsPatch,
        expected_revision: i64,
    ) -> Result<ModelSettingsView, AppError> {
        let mut settings = self.settings.load().map_err(settings_io_err)?;
        if settings.revision != expected_revision {
            return Err(settings_revision_conflict());
        }
        settings.apply_patch(patch);
        settings.revision += 1;
        self.save_settings(settings)?;
        self.emit_model_changed(ModelPurpose::Generate, "", "");
        self.settings_get()
    }

    // —— 诊断(《M08》§9)——

    pub async fn diagnostics_preview(
        &self,
        include_paths: bool,
        include_model_details: bool,
    ) -> Result<DiagnosticsPreview, AppError> {
        let status = self.model_get_status().await?;
        self.diagnostics.preview(
            include_paths,
            include_model_details,
            &self.data_dir,
            &status,
        )
    }

    pub fn diagnostics_export(
        &self,
        preview_hash: &str,
        path: &str,
        _idempotency_key: &str,
    ) -> Result<DiagnosticsExportResult, AppError> {
        self.diagnostics.export(preview_hash, path)
    }

    // —— 内部门面(M04/M09 编排入口,不经 IPC)——

    /// 一步式嵌入编排:签发许可 → 发送(供 M04 使用;M09 走同网关)。
    pub async fn embed_with_permit(
        &self,
        spec: &EgressRequestSpec,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<super::provider::EmbedSuccess, AppError> {
        let permit = self.gateway.issue_permit(spec)?;
        self.gateway
            .send_embedding(&permit.permit_id, spec, cancel)
            .await
    }

    fn emit_privacy_changed(
        &self,
        action: &str,
        document_id: &str,
        location: ProcessingLocation,
        provider_id: &str,
        purpose: ModelPurpose,
    ) {
        let event = DomainEvent::new(
            DomainEventType::PrivacyChanged,
            document_id.to_string(),
            1,
            self.clock.now_ms(),
            serde_json::json!({
                "action": action,
                "location": location.as_str(),
                "providerId": provider_id,
                "purpose": purpose.as_str(),
            }),
        )
        .with_document(Some(document_id.to_string()), None);
        self.events.emit(event);
    }

    fn emit_model_changed(&self, purpose: ModelPurpose, provider_id: &str, model_id: &str) {
        let event = DomainEvent::new(
            DomainEventType::ModelChanged,
            format!("provider:{provider_id}"),
            1,
            self.clock.now_ms(),
            serde_json::json!({
                "purpose": purpose.as_str(),
                "providerId": provider_id,
                "modelId": model_id,
            }),
        );
        self.events.emit(event);
    }

    /// 供 M04 嵌入代次接线:期望维度来自提供方设置或探测。
    pub fn embed_dimensions_for(&self, provider_id: &str) -> Option<i64> {
        let settings = self.settings.load().ok()?;
        settings
            .providers
            .iter()
            .find(|p| p.id == provider_id)
            .and_then(|p| p.embed_dimensions)
    }

    pub fn egress_snapshot(&self) -> super::gateway::EgressStatsSnapshot {
        self.gateway.snapshot_stats()
    }
}

#[cfg(test)]
mod tests {
    use crate::model_privacy::errors::{model_err, ModelErrorCode};

    #[test]
    fn model_error_reexport() {
        // model_err 便捷函数在命令层复用。
        let e = model_err(ModelErrorCode::ModelUnavailable);
        assert_eq!(e.code, "MODEL_UNAVAILABLE");
    }
}
