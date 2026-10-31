//! M08 对外 DTO(《02》§3.3 模型/隐私/设置命令 + 《M08》§2–§9 视图)。
//!
//! 约定:
//! - 密钥永远不出现在任何 DTO;`SecretState` 只含 configured 与更新时间。
//! - `EgressPermit` 是进程内对象,不进 IPC,定义在 `permit.rs`。
//! - 清除类别对齐《M08》§8 清除矩阵;未注册贡献器的模块在预览中显式
//!   标记不可用,不静默假装已清理。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::evidence::models::ProtectedBlockView;

// —— 枚举 ——

/// 提供方协议形态(《M08》§2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ProviderKind {
    Ollama,
    #[serde(rename = "openai_compatible")]
    OpenaiCompatible,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenaiCompatible => "openai_compatible",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "ollama" => Self::Ollama,
            "openai_compatible" => Self::OpenaiCompatible,
            _ => return None,
        })
    }

    /// 协议形态的默认处理位置:Ollama 走本机,OpenAI 兼容端点属远程。
    pub fn default_location(&self) -> ProcessingLocation {
        match self {
            Self::Ollama => ProcessingLocation::Local,
            Self::OpenaiCompatible => ProcessingLocation::Remote,
        }
    }
}

/// 处理位置:本地与远程授权独立(《M08》§5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ProcessingLocation {
    Local,
    Remote,
}

impl ProcessingLocation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "local" => Self::Local,
            "remote" => Self::Remote,
            _ => return None,
        })
    }
}

/// 用途:生成/嵌入授权独立(《M08》§5);Vision 仅用于图片外发授权,
/// 不是可由用户指定的模型参考角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ModelPurpose {
    Generate,
    Embed,
    /// 图片阅读授权:含图片的请求除 Generate 外还必须具备该用途授权。
    /// 不是模型参考角色(不能设为生成/嵌入参考模型)。
    Vision,
}

impl ModelPurpose {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::Embed => "embed",
            Self::Vision => "vision",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "generate" => Self::Generate,
            "embed" => Self::Embed,
            "vision" => Self::Vision,
            _ => return None,
        })
    }

    /// 是否可作为生成/嵌入参考模型角色(图片授权不是角色)。
    pub fn is_reference_role(&self) -> bool {
        matches!(self, Self::Generate | Self::Embed)
    }
}

/// 生成模型角色(《方案》FR-13):快速/质量/翻译分别可选不同模型;
/// 未配置时按回退链选择,不要求用户重复配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GenerationRole {
    Fast,
    Quality,
    Translation,
}

impl GenerationRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
            Self::Translation => "translation",
        }
    }

    pub fn parse_db(value: &str) -> Option<Self> {
        Some(match value {
            "fast" => Self::Fast,
            "quality" => Self::Quality,
            "translation" => Self::Translation,
            _ => return None,
        })
    }
}

/// 提供方能力(《M08》§2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ProviderCapability {
    Generate,
    Embed,
    StructuredOutput,
    Streaming,
    Cancel,
    Vision,
    /// 工具调用(P2 Agent 只读循环;模型/提供方必须显式声明)。
    ToolCalling,
}

impl ProviderCapability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::Embed => "embed",
            Self::StructuredOutput => "structured_output",
            Self::Streaming => "streaming",
            Self::Cancel => "cancel",
            Self::Vision => "vision",
            Self::ToolCalling => "tool_calling",
        }
    }
}

/// 整体能力状态(《M08》§3)。模型不可用不阻塞启动,只降级依赖任务。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ModelCapability {
    Ready,
    GenerateOnly,
    EmbedOnly,
    Unavailable,
    Misconfigured,
}

/// 降级原因(稳定枚举,前端按 code 映射文案;不含路径与正文)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ModelDegradationReason {
    /// 没有任何已启用的提供方。
    NoProvider,
    /// 提供方探测不可达(未安装/端口不可达/断网)。
    ProviderUnreachable,
    /// 远程提供方缺少密钥。
    SecretMissing,
    /// 远程提供方缺少 baseUrl。
    BaseUrlMissing,
    /// 生成角色未设置参考模型。
    GenerateReferenceMissing,
    /// 嵌入角色未设置参考模型。
    EmbedReferenceMissing,
    /// 参考模型不在提供方可用列表中。
    ReferenceModelMissing,
    /// 参考的提供方不存在或未启用。
    ReferenceProviderMissing,
}

impl ModelDegradationReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoProvider => "no_provider",
            Self::ProviderUnreachable => "provider_unreachable",
            Self::SecretMissing => "secret_missing",
            Self::BaseUrlMissing => "base_url_missing",
            Self::GenerateReferenceMissing => "generate_reference_missing",
            Self::EmbedReferenceMissing => "embed_reference_missing",
            Self::ReferenceModelMissing => "reference_model_missing",
            Self::ReferenceProviderMissing => "reference_provider_missing",
        }
    }
}

// —— 模型状态视图 ——

/// 单个模型的非敏感描述(不含提示词或密钥)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelDescriptor {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub capabilities: Vec<ProviderCapability>,
}

/// 提供方描述(《M08》§2 的 ModelProviderDescriptor)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelProviderDescriptor {
    pub id: String,
    pub kind: ProviderKind,
    pub location: ProcessingLocation,
    pub display_name: String,
    pub capabilities: Vec<ProviderCapability>,
    pub configured: bool,
    pub reachable: bool,
    pub available_models: Vec<ModelDescriptor>,
}

/// 生成/嵌入角色的参考配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelReference {
    pub provider_id: String,
    pub model_id: String,
}

/// 密钥状态:前端只可读 configured 与更新时间,不存在读取秘密的命令。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SecretState {
    pub provider_id: String,
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    pub updated_at: Option<i64>,
}

/// `model_get_status` 返回(《02》§3.3)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelStatusView {
    pub schema_version: &'static str,
    pub capability: ModelCapability,
    pub reasons: Vec<ModelDegradationReason>,
    pub providers: Vec<ModelProviderDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate_reference: Option<ModelReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fast_reference: Option<ModelReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub quality_reference: Option<ModelReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub translation_reference: Option<ModelReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_reference: Option<ModelReference>,
    /// 质量模式的真实能力(工具调用/推理窗口);用于如实提示“仅单步”。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub quality_capability: Option<ModelCapabilityView>,
    pub secrets: Vec<SecretState>,
    /// 设置修订号(model_set_reference/settings_update 的乐观并发依据)。
    #[ts(type = "number")]
    pub settings_revision: i64,
}

/// 2F 模型能力视图(设置页展示与手动修正)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelCapabilityView {
    pub provider_id: String,
    pub model_id: String,
    #[ts(type = "number")]
    pub context_window: i64,
    #[ts(type = "number")]
    pub max_output: i64,
    pub streaming: bool,
    pub reasoning: bool,
    pub tool_calling: bool,
    pub vision: bool,
    pub json_schema: bool,
    /// auto = 自动探测/内置默认;manual = 用户手动修正。
    pub source: String,
}

impl ModelCapabilityView {
    pub fn from_capability(capability: &super::capability::ModelCapability) -> Self {
        Self {
            provider_id: capability.provider_id.clone(),
            model_id: capability.model_id.clone(),
            context_window: capability.context_window,
            max_output: capability.max_output,
            streaming: capability.streaming,
            reasoning: capability.reasoning,
            tool_calling: capability.tool_calling,
            vision: capability.vision,
            json_schema: capability.json_schema,
            source: capability.source.as_str().to_string(),
        }
    }
}

/// 图片发送被阻止的稳定原因(前端映射文案;不含正文/路径)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum VisionBlockedReason {
    /// 未配置生成参考模型。
    GenerateReferenceMissing,
    /// 参考提供方未注册或不可用。
    ProviderUnavailable,
    /// 远程图片外发总开关关闭。
    RemoteVisionDisabled,
    /// 当前文档尚未授权图片发送给该提供方。
    VisionGrantRequired,
}

impl VisionBlockedReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GenerateReferenceMissing => "generate_reference_missing",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::RemoteVisionDisabled => "remote_vision_disabled",
            Self::VisionGrantRequired => "vision_grant_required",
        }
    }
}

/// 当前文档的图片发送就绪状态(本地提供方按既有本地规则恒可用)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct VisionStatusView {
    pub schema_version: &'static str,
    pub document_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_location: Option<ProcessingLocation>,
    /// 远程图片外发总开关(本地链路不需要)。
    pub remote_vision_allowed: bool,
    /// 当前文档已具备 Vision 授权(本地恒为 true)。
    pub vision_granted: bool,
    /// 当前是否允许把图片随问题发送。
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<VisionBlockedReason>,
}

// —— 隐私授权视图 ——

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum PrivacyGrantStatus {
    Granted,
    Revoked,
}

impl PrivacyGrantStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Revoked => "revoked",
        }
    }
}

/// 单条授权记录(历史保留,撤回不删行)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct PrivacyGrantView {
    pub id: String,
    pub document_id: String,
    pub processing_location: ProcessingLocation,
    pub provider_id: String,
    pub purpose: ModelPurpose,
    pub status: PrivacyGrantStatus,
    #[ts(type = "number")]
    pub granted_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    pub revoked_at: Option<i64>,
    pub policy_version: String,
}

/// 一条数据链路能否执行:授权 × 能力 的合成裁决(《M08》§5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GrantCheckReason {
    /// 需要授权(未授权或已撤回)。
    GrantRequired,
    /// 提供方当前不可用(不可达/未配置/模型缺失)。
    ProviderUnavailable,
}

/// `privacy_get_status` 中每条链路的检查结果。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GrantCheckView {
    pub processing_location: ProcessingLocation,
    pub provider_id: String,
    pub purpose: ModelPurpose,
    pub granted: bool,
    pub capability_ready: bool,
    /// granted && capability_ready。
    pub executable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<GrantCheckReason>,
}

/// `privacy_get_status` 返回:每类能力+授权能否执行、数据链路与降级原因。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct PrivacyStatusView {
    pub schema_version: &'static str,
    pub document_id: String,
    pub policy_version: String,
    /// 本地/远程 × 生成/嵌入的检查矩阵(数据链路即此矩阵的行)。
    pub checks: Vec<GrantCheckView>,
    /// 当前文档全部授权历史(含已撤回)。
    pub grants: Vec<PrivacyGrantView>,
}

// —— 派生数据清除(《M08》§8)——

/// 清除类别(对齐《M08》§8 清除矩阵行)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ClearCategory {
    /// FTS/向量/临时缓存(可重建)。
    FtsVectorCache,
    /// 未被引用的解析缓存 ContentBlock(裁决归 M05,执行归 M04 接线)。
    ParseCache,
    /// 会话与消息(M07 接线后注册)。
    Conversations,
    /// 产物与版本(M10 接线后注册)。
    Artifacts,
    /// 可选保存的思考记录(默认不写;开启后按文档清除)。
    AgentReasoning,
    /// 动态卡片与文档画像缓存(可重建)。
    InsightCards,
    /// 翻译记忆(跨文档缓存;清除后不删除已生成译文产物)。
    TranslationMemory,
    // —— 用户资产类:默认不选,必须明确勾选 ——
    Annotations,
    Bookmarks,
    Notes,
    Clarifications,
    Tags,
}

impl ClearCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FtsVectorCache => "fts_vector_cache",
            Self::ParseCache => "parse_cache",
            Self::Conversations => "conversations",
            Self::Artifacts => "artifacts",
            Self::AgentReasoning => "agent_reasoning",
            Self::InsightCards => "insight_cards",
            Self::TranslationMemory => "translation_memory",
            Self::Annotations => "annotations",
            Self::Bookmarks => "bookmarks",
            Self::Notes => "notes",
            Self::Clarifications => "clarifications",
            Self::Tags => "tags",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "fts_vector_cache" => Self::FtsVectorCache,
            "parse_cache" => Self::ParseCache,
            "conversations" => Self::Conversations,
            "artifacts" => Self::Artifacts,
            "agent_reasoning" => Self::AgentReasoning,
            "insight_cards" => Self::InsightCards,
            "translation_memory" => Self::TranslationMemory,
            "annotations" => Self::Annotations,
            "bookmarks" => Self::Bookmarks,
            "notes" => Self::Notes,
            "clarifications" => Self::Clarifications,
            "tags" => Self::Tags,
            _ => return None,
        })
    }

    /// 全部类别(预览/UI 用稳定顺序:可重建数据 → 派生数据 → 用户资产)。
    pub const ALL: [ClearCategory; 12] = [
        Self::FtsVectorCache,
        Self::ParseCache,
        Self::Conversations,
        Self::Artifacts,
        Self::AgentReasoning,
        Self::InsightCards,
        Self::TranslationMemory,
        Self::Annotations,
        Self::Bookmarks,
        Self::Notes,
        Self::Clarifications,
        Self::Tags,
    ];

    /// 用户资产类:默认不选,必须明确勾选(《M08》§8)。
    pub fn is_user_asset(&self) -> bool {
        matches!(
            self,
            Self::Annotations | Self::Bookmarks | Self::Notes | Self::Clarifications | Self::Tags
        )
    }
}

/// 类别不可执行原因(未注册贡献模块;如实展示,不静默假装已清理)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ClearUnavailableReason {
    /// 拥有该类别的模块尚未注册清除贡献器。
    ContributorNotRegistered,
}

/// 单类别预览。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClearCategoryPreview {
    pub category: ClearCategory,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<ClearUnavailableReason>,
    /// 待清除数量(不可用时缺省)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub count: Option<i64>,
    /// 会受影响的引用数量(如笔记拥有的 Citation)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub affected_citations: Option<i64>,
    /// 受证据保留裁决保护、不会被清除的数量。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub protected_count: Option<i64>,
}

/// `privacy_preview_clear` 返回:带 planId/planHash/过期时间的清计划。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClearPreview {
    pub schema_version: &'static str,
    pub document_id: String,
    pub plan_id: String,
    pub plan_hash: String,
    /// 计划过期时间(epoch ms);过期或数据漂移后拒绝执行。
    #[ts(type = "number")]
    pub expires_at: i64,
    pub categories: Vec<ClearCategoryPreview>,
    /// M05 保留裁决指出的受保护块(清理解析缓存时不会被删除)。
    pub protected_blocks: Vec<ProtectedBlockView>,
    /// 原文件永不受影响(常量 false,仅作显式契约)。
    pub original_file_affected: bool,
    /// 含用户资产类时必须二次确认。
    pub requires_explicit_confirmation: bool,
}

/// 单类别执行结果(只含计数,不含被删正文)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClearCategoryExecution {
    pub category: ClearCategory,
    #[ts(type = "number")]
    pub deleted: i64,
}

/// `privacy_execute_clear` 返回。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClearResult {
    /// 审计幂等重放需要反序列化,schemaVersion 用 String(非 &'static str)。
    pub schema_version: String,
    pub document_id: String,
    pub plan_id: String,
    pub executed: Vec<ClearCategoryExecution>,
    /// 执行前请求取消的活动任务数(queued 直接取消/running 请求取消)。
    #[ts(type = "number")]
    pub canceled_tasks: i64,
    pub audit_id: String,
}

// —— 诊断(《M08》§9)——

/// 已应用迁移版本(非敏感 schema 摘要)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SchemaModuleVersion {
    pub module: String,
    #[ts(type = "number")]
    pub version: i64,
}

/// 非敏感计数条目(任务类型/状态/错误码等)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct CountEntry {
    pub key: String,
    #[ts(type = "number")]
    pub count: i64,
}

/// 诊断预览:默认不含正文、路径、提示词和密钥;路径与模型细节
/// 仅在用户显式勾选后加入,并需再次确认后才可导出(A9)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct DiagnosticsPreview {
    pub schema_version: &'static str,
    pub preview_hash: String,
    pub app_version: String,
    pub os: String,
    pub db_schema: Vec<SchemaModuleVersion>,
    pub model_capability: ModelCapability,
    pub model_reasons: Vec<ModelDegradationReason>,
    pub provider_kinds: Vec<ProviderKind>,
    pub task_counts: Vec<CountEntry>,
    pub task_error_counts: Vec<CountEntry>,
    /// 用户主动勾选后才加入的字段:
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_dir_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_details: Option<Vec<ModelProviderDescriptor>>,
    pub include_paths: bool,
    pub include_model_details: bool,
    #[ts(type = "number")]
    pub generated_at: i64,
}

/// `diagnostics_export` 返回(显式路径,不覆盖已有文件)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct DiagnosticsExportResult {
    pub schema_version: &'static str,
    pub path: String,
    #[ts(type = "number")]
    pub bytes: i64,
}

// —— 常量 ——

/// 隐私说明版本:展示内容(处理位置/范围/提供方/用途/外发内容)变化时递增。
pub const PRIVACY_POLICY_VERSION: &str = "privacy.policy.v1";

/// 派生数据清除计划有效期(预览后数据可能漂移,过期必须重新预览)。
pub const CLEAR_PLAN_TTL_MS: i64 = 10 * 60 * 1000;

/// 外发许可有效期(短期;重试需新许可)。
pub const EGRESS_PERMIT_TTL_MS: i64 = 60 * 1000;

/// 授权确认对话框必须展示的当前隐私说明版本对应文案键(前端)。
pub const PRIVACY_GRANT_DIALOG_MESSAGE_KEY: &str = "privacy.grant.dialog.v1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_categories_cover_matrix_and_flag_user_assets() {
        assert_eq!(ClearCategory::ALL.len(), 12);
        assert!(ClearCategory::Notes.is_user_asset());
        assert!(!ClearCategory::FtsVectorCache.is_user_asset());
        for c in ClearCategory::ALL {
            assert_eq!(ClearCategory::parse_db(c.as_str()), Some(c));
        }
    }

    #[test]
    fn provider_kind_location_defaults() {
        assert_eq!(
            ProviderKind::Ollama.default_location(),
            ProcessingLocation::Local
        );
        assert_eq!(
            ProviderKind::OpenaiCompatible.default_location(),
            ProcessingLocation::Remote
        );
        assert_eq!(
            ProviderKind::parse_db("openai_compatible"),
            Some(ProviderKind::OpenaiCompatible)
        );
    }
}
