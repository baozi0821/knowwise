//! M07 领域对象与对外 DTO(《01》§4.4/§4.6/§5.2、《M07》§2–§8)。
//!
//! 契约:用户看到的阅读范围与实际冻结的上下文分离。建议只预填,
//! 发送前可见可改;快照创建后不可变,滚动、标注或切换章节只影响下一次建议。
//!
//! - 局部范围存 SnapshotBlock 精确列表;全文只在代次完整且 blockSetHash
//!   已封存时绑定 parseGenerationId + blockSetHash。
//! - 笔记与 AI 草稿以文本快照(textSnapshot)进入,不因后续编辑变化;
//!   AI 草稿保持 ai_draft 层级,不得作为唯一原文事实。
//! - 消息不保存模型原始流与未校验 JSON:contentJson 只存通过校验的
//!   user_message.v1 / assistant_placeholder.v1 / answer.v1。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::evidence::models::RawSelectionInput;
use crate::notes::models::SourceRole;
use crate::shared::locator::SourceLocator;

/// ContextSource.sourceType 与《01》§5.3 SourceReference.sourceType 同域,
/// 复用 M06 已导出的封闭联合,避免契约漂移。
pub type ContextSourceType = crate::notes::models::SourceRefType;

// —— 模块自有错误码(《02》§7:CONTEXT_* 系列,不进 shared/error.rs)——

/// M07 稳定错误码。错误码只增不改语义;重试与用户动作按《02》§7 固定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextErrorCode {
    /// 部分解析时请求全文:不得自动全文回答(用户动作 wait_for_index)。
    CoverageIncomplete,
    /// 预算超限:不静默截断,要求缩小范围或改分块任务(choose_scope)。
    BudgetExceeded,
    /// 快照不可变:修改范围必须创建新快照(none)。
    SnapshotImmutable,
}

impl ContextErrorCode {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CoverageIncomplete => "CONTEXT_COVERAGE_INCOMPLETE",
            Self::BudgetExceeded => "CONTEXT_BUDGET_EXCEEDED",
            Self::SnapshotImmutable => "CONTEXT_SNAPSHOT_IMMUTABLE",
        }
    }

    pub fn retryable(&self) -> bool {
        false
    }

    pub fn user_action(&self) -> crate::shared::UserAction {
        match self {
            Self::CoverageIncomplete => crate::shared::UserAction::WaitForIndex,
            Self::BudgetExceeded => crate::shared::UserAction::ChooseScope,
            Self::SnapshotImmutable => crate::shared::UserAction::None,
        }
    }

    /// 产出统一 AppError 信封(模块自有构造,不并入 CoreErrorCode)。
    pub fn to_app_error(self) -> crate::shared::AppError {
        crate::shared::AppError {
            schema_version: "error.v1",
            code: self.code().to_string(),
            message_key: format!("error.{}", self.code().to_lowercase()),
            retryable: self.retryable(),
            user_action: self.user_action(),
            details: None,
            trace_id: crate::shared::new_trace_id(),
        }
    }
}

// —— 范围模型(《M07》§2)——

/// 范围种类,与 scope_json 的判别式一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ScopeKind {
    Selection,
    CurrentPosition,
    Section,
    Pages,
    FullDocument,
    Sources,
}

impl ScopeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Selection => "selection",
            Self::CurrentPosition => "current_position",
            Self::Section => "section",
            Self::Pages => "pages",
            Self::FullDocument => "full_document",
            Self::Sources => "sources",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "selection" => Self::Selection,
            "current_position" => Self::CurrentPosition,
            "section" => Self::Section,
            "pages" => Self::Pages,
            "full_document" => Self::FullDocument,
            "sources" => Self::Sources,
            _ => return None,
        })
    }
}

/// 范围草案:建议只预填,发送前可见可改;冻结时按 kind 精确解析。
/// 相等性以规范 JSON 序列化为准(canonicalHash),不做结构化 PartialEq
/// (RawSelectionInput 为 M05 DTO,未派生 PartialEq)。
/// 字段按《01》§2 使用 camelCase;判别值(kind)保持 snake_case 枚举串。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ScopeDraft {
    /// 当前选区(M03 RawSelection 的最小投影,复用 M05 DTO)。
    #[serde(rename_all = "camelCase")]
    Selection {
        raw_selection: Box<RawSelectionInput>,
    },
    /// 当前位置:视口中心语义块(建议阶段由 viewportBlockId 解析)。
    #[serde(rename_all = "camelCase")]
    CurrentPosition {
        block_id: String,
        locator: SourceLocator,
    },
    /// 当前节:标题路径前缀;可显式携带块集合。
    #[serde(rename_all = "camelCase")]
    Section {
        heading_path: Vec<String>,
        #[serde(default)]
        #[ts(optional)]
        block_ids: Option<Vec<String>>,
    },
    /// 指定页面(PDF;startPage <= endPage,从 1 起)。
    #[serde(rename_all = "camelCase")]
    Pages {
        #[ts(type = "number")]
        start_page: i64,
        #[ts(type = "number")]
        end_page: i64,
    },
    /// 全文:必须用户确认;只在代次完整且 blockSetHash 已封存时可冻结。
    #[serde(rename_all = "camelCase")]
    FullDocument {
        #[serde(default)]
        confirmed: bool,
    },
    /// 多来源:块引用 + 选区 + 笔记 + AI 草稿(逐项可移除)。
    #[serde(rename_all = "camelCase")]
    Sources { sources: Vec<ContextSourceDraft> },
}

impl ScopeDraft {
    pub fn kind(&self) -> ScopeKind {
        match self {
            Self::Selection { .. } => ScopeKind::Selection,
            Self::CurrentPosition { .. } => ScopeKind::CurrentPosition,
            Self::Section { .. } => ScopeKind::Section,
            Self::Pages { .. } => ScopeKind::Pages,
            Self::FullDocument { .. } => ScopeKind::FullDocument,
            Self::Sources { .. } => ScopeKind::Sources,
        }
    }
}

/// 来源草案:sourceRole 由服务端按来源实体推导,客户端不得声明层级。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ContextSourceDraft {
    pub source_type: ContextSourceType,
    pub source_id: String,
    /// selection 来源的选区文本(冻结时经 M04 生成按需块)。
    #[serde(default)]
    #[ts(optional)]
    pub text: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub locator: Option<SourceLocator>,
}

// —— 建议与预览(《M07》§2/§3)——

/// 建议置信度:低置信度不强制选择,要求用户确认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SuggestionConfidence {
    High,
    Medium,
    Low,
}

/// 单条范围建议:固定顺序 明确选区 > 正在编辑位置/视口块 > 当前节 > 全文,
/// 界面显示依据(basis)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ScopeSuggestion {
    #[ts(type = "number")]
    pub rank: i64,
    pub scope: ScopeDraft,
    pub confidence: SuggestionConfidence,
    pub requires_confirmation: bool,
    /// 机器可读依据:explicit_selection / viewport_block / viewport_section /
    /// fallback_full / no_signal 等。
    pub basis: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ContextSuggestion {
    pub document_id: String,
    pub version_id: String,
    /// 按 rank 升序;至少包含全文兜底(始终要求确认)。
    pub candidates: Vec<ScopeSuggestion>,
}

/// 覆盖摘要(coverage_json 的结构,schemaVersion=context_coverage.v1)。
/// 部分解析时 scope_covered=false;不得显示"全文已读取"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct CoverageSummary {
    pub schema_version: String,
    /// 解析代次状态(not_started/in_progress/complete/partial/failed)。
    pub parse_state: String,
    /// 范围内内容是否完整可得。
    pub scope_covered: bool,
    pub completed_regions: Vec<String>,
    pub partial_regions: Vec<String>,
    /// 范围内缺失区域(如未完成解析的页)。
    pub missing_regions: Vec<String>,
    /// 附加说明(fts_only / no_text_layer / generation_unsealed 等)。
    pub notes: Vec<String>,
}

impl CoverageSummary {
    pub fn empty(parse_state: &str) -> Self {
        Self {
            schema_version: "context_coverage.v1".to_string(),
            parse_state: parse_state.to_string(),
            scope_covered: false,
            completed_regions: Vec::new(),
            partial_regions: Vec::new(),
            missing_regions: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// 预览中的来源条目。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ContextSourcePreview {
    #[ts(type = "number")]
    pub position: i64,
    pub source_type: ContextSourceType,
    pub source_id: String,
    pub source_role: SourceRole,
    pub display_name: String,
    #[ts(type = "number")]
    pub char_count: i64,
    /// 来源实体当前仍存在(读取时派生;删除后快照仍可读)。
    pub source_available: bool,
}

/// `context_preview` 返回:名称、块数量、字符/Token 估算、来源清单、
/// 覆盖与可执行能力。预览不冻结;预算超限不静默截断。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ContextPreview {
    pub document_id: String,
    pub document_version_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parse_generation_id: Option<String>,
    pub name: String,
    #[ts(type = "number")]
    pub block_count: i64,
    #[ts(type = "number")]
    pub char_count: i64,
    #[ts(type = "number")]
    pub token_estimate: i64,
    #[ts(type = "number")]
    pub token_budget: i64,
    pub within_budget: bool,
    pub sources: Vec<ContextSourcePreview>,
    pub coverage: CoverageSummary,
    /// explicit_blocks / fts_only / hybrid / unavailable(《01》§4.6 capabilityMode)。
    pub capability: String,
    /// 覆盖不足、FTS-only、无文本层等用户可见提示(非正文)。
    pub warnings: Vec<String>,
}

// —— 快照视图(《M07》§4)——

/// 已冻结来源:文本快照与层级随快照固定。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ContextSourceView {
    #[ts(type = "number")]
    pub position: i64,
    pub source_type: ContextSourceType,
    pub source_id: String,
    pub source_role: SourceRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_version: Option<String>,
    pub display_name: String,
    /// 冻结时的正文副本(来源被删除后仍可查看,不受后续编辑影响)。
    pub text_snapshot: String,
    #[ts(type = "number")]
    pub char_count: i64,
    pub content_hash: String,
    /// 来源实体当前仍存在(读取时派生)。
    pub source_available: bool,
}

/// 显示摘要:提问区固定显示范围、来源数与估算。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SnapshotSummary {
    pub name: String,
    #[ts(type = "number")]
    pub block_count: i64,
    #[ts(type = "number")]
    pub source_count: i64,
    #[ts(type = "number")]
    pub char_count: i64,
    #[ts(type = "number")]
    pub token_estimate: i64,
}

/// 快照简要(会话/消息列表内嵌,不含来源明细)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SnapshotBrief {
    pub snapshot_id: String,
    pub scope_kind: ScopeKind,
    pub name: String,
    /// 冻结时的覆盖真相(消息头显示“已读页/总页、完整/部分”)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coverage: Option<CoverageSummary>,
}

/// `context_freeze` / `context_get` 返回:冻结后的范围与来源,不可修改。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ContextSnapshotView {
    pub id: String,
    pub document_id: String,
    pub document_version_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parse_generation_id: Option<String>,
    pub scope_kind: ScopeKind,
    pub scope: ScopeDraft,
    pub summary: SnapshotSummary,
    pub coverage: CoverageSummary,
    /// 局部范围的显式块集合(按原文顺序);全文快照为空(以 blockSetHash 固定)。
    pub block_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub block_set_hash: Option<String>,
    pub canonical_hash: String,
    pub sources: Vec<ContextSourceView>,
    #[ts(type = "number")]
    pub created_at: i64,
}

// —— 会话与消息(《M07》§6)——

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ConversationView {
    pub id: String,
    pub document_id: String,
    pub title: String,
    /// 下一轮追问默认继承的快照;只影响建议,不改变历史消息。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_snapshot: Option<SnapshotBrief>,
    #[ts(type = "number")]
    pub message_count: i64,
    /// 用户置顶(仅排序;消息与快照不变)。
    pub pinned: bool,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

pub type ConversationPage = crate::shared::Page<ConversationView>;

/// `conversation_export` 结果(不含保存路径;仅返回字节数供 UI 反馈)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ConversationExportResult {
    pub conversation_id: String,
    pub format: String,
    #[ts(type = "number")]
    pub bytes_written: i64,
}

/// 消息生命周期(《M07》§6):流式文本仅临时显示,不落库。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            _ => return None,
        })
    }
}

/// 助手消息生命周期:占位 → completed/failed/canceled;失败保留问题与原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum AnswerLifecycle {
    Pending,
    Completed,
    Failed,
    Canceled,
}

impl AnswerLifecycle {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            _ => return None,
        })
    }
}

/// answer.v1 规范类型归 M09 所有(《00》§3.2:结构化输出契约单一事实源);
/// M07 消息存储只保存通过 M09 校验后的 AnswerContent,此处再导出复用,
/// 不复制第二份形状(避免双定义漂移与 ts-rs 契约文件冲突)。
pub use crate::retrieval_ai::models::{
    AnswerCitation, AnswerConflict, AnswerContent, AnswerSegment, AnswerStatus, SegmentKind,
};

/// 写工具确认框投影:只含工具名/目标摘要/是否需要用户选择路径;
/// 绝不含正文全文、密钥或完整路径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AgentConfirmationView {
    pub tool: String,
    /// 脱敏的人读摘要(标题/数量/格式等,不含正文或路径)。
    pub summary: String,
    /// export_file 需要用户在保存对话框中选定路径。
    pub requires_path: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub suggested_filename: Option<String>,
}

/// Agent 步骤时间线投影(P2/P3A):只含工具名/状态/摘要,不含正文全文、密钥或路径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AgentStepView {
    /// 确认/回放使用的稳定步骤 ID(P2 旧消息可能缺省,反序列化为空)。
    #[serde(default)]
    pub step_id: String,
    #[ts(type = "number")]
    pub step_no: i64,
    /// model / tool
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_name: Option<String>,
    /// running / awaiting_confirmation / ok / failed / canceled
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub args_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub result_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error_code: Option<String>,
    #[ts(type = "number")]
    pub duration_ms: i64,
    /// 仅写工具确认步骤存在;UI 据此弹确认框。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confirmation: Option<AgentConfirmationView>,
}

/// agent.v1 消息内容:最终答复 + 可回放步骤;verified=false 时 UI 必须标注“未校验”。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AgentAnswerV1 {
    pub schema_version: String,
    pub answer: AnswerContent,
    pub steps: Vec<AgentStepView>,
    /// 最终答复是否通过 answer.v1 引用校验(否则为无引用自由文本降级)。
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stopped_reason: Option<String>,
}

/// 阅读器视口页码(PDF);用于“当前页 ±N”范围建议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageViewport {
    pub page_number: i64,
    pub nearby_pages: i64,
    pub page_count: Option<i64>,
}

/// 5 秒速答卡:从当前范围原文抽取,不经过模型、不冒充完整回答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuickAnswerView {
    pub schema_version: String,
    pub scope_label: String,
    pub sentences: Vec<QuickAnswerSentence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuickAnswerSentence {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub page: Option<i64>,
    pub block_id: String,
}

/// 非答案但必须可见的助手通知(权限/模型不可用/OCR/取消等)。
/// 只含稳定错误码、可见说明与动作,不含正文、密钥或路径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AssistantNoticeV1 {
    pub schema_version: String,
    pub message: String,
    pub reason_code: String,
    #[serde(default)]
    pub actions: Vec<AssistantNoticeAction>,
    /// 是否为兜底(未得到模型答案);不冒充已验证回答。
    pub fallback_used: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AssistantNoticeAction {
    /// open_settings / grant / retry / switch_mode
    pub id: String,
    pub label: String,
}

impl AssistantNoticeV1 {
    pub fn new(message: String, reason_code: String, actions: Vec<AssistantNoticeAction>) -> Self {
        Self {
            schema_version: "assistant_notice.v1".into(),
            message,
            reason_code,
            actions,
            fallback_used: true,
        }
    }
}

/// contentJson 封闭联合:按 schemaVersion 判别,不把流式碎片当已验证内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "schemaVersion", rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum MessageContent {
    #[serde(rename = "user_message.v1")]
    UserMessage { text: String },
    #[serde(rename = "assistant_placeholder.v1")]
    AssistantPlaceholder {},
    #[serde(rename = "answer.v1")]
    Answer { content: AnswerContent },
    /// P2 Agent 只读循环:最终答复 + 步骤时间线(与 answer.v1 并列,不改旧契约)。
    #[serde(rename = "agent.v1")]
    AgentAnswer { content: AgentAnswerV1 },
    /// 失败/权限/OCR 等场景的可见回复(不冒充答案)。
    #[serde(rename = "assistant_notice.v1")]
    AssistantNotice { notice: AssistantNoticeV1 },
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct MessageView {
    pub id: String,
    pub conversation_id: String,
    pub document_id: String,
    pub role: MessageRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub snapshot_id: Option<String>,
    /// 快照摘要(会话历史显示"使用哪一版本、哪个范围")。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub snapshot: Option<SnapshotBrief>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_id: Option<String>,
    /// 任务当前状态(读取时关联 task 表;AI-06 状态真相来源)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_status: Option<String>,
    pub content: MessageContent,
    pub answer_status: AnswerLifecycle,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model_provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model_id: Option<String>,
    /// failed/canceled 的稳定错误码(不保存伪答案)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error_code: Option<String>,
    /// 用户显式开启“保存思考记录”时保存的 provider 原始 reasoning。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning: Option<String>,
    /// 本轮上下文说明:历史/摘要裁剪、摘要更新失败等(来自任务结果,
    /// 不含正文、密钥或完整路径)。未发生时缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_note: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

pub type MessagePage = crate::shared::Page<MessageView>;

/// 任务重试所需的原始上下文(任务中心「重试」真实重发)。
///
/// 只从已落库的消息恢复:原问题来自该任务助手消息之前的最近用户消息,
/// 快照优先取助手消息(发送时冻结),缺失时回退用户消息;均不重新检索。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRetrySource {
    pub document_id: String,
    pub conversation_id: String,
    pub snapshot_id: String,
    pub question: String,
}

/// 任务中心重试结果:新任务 + 新消息定位信息(用于滚动并高亮)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct TaskRetryResult {
    pub task: crate::shared::dto::TaskView,
    pub document_id: String,
    pub conversation_id: String,
    pub assistant_message_id: String,
}

// —— 清除贡献(《M07》§8;M08 聚合,M01 执行,M07 不执行删除)——

/// 受保护快照与原因。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ProtectedSnapshotView {
    pub snapshot_id: String,
    /// artifact_reference / message_reference / active_snapshot
    pub reason: String,
}

/// 清除计划步骤(同事务按序执行;M07 只产出计划,不执行)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClearPlanStep {
    #[ts(type = "number")]
    pub step: i64,
    pub action: String,
    pub table: String,
    pub qualifier: String,
}

/// M07 清除贡献预览:会话、消息与快照;仍被产物/消息引用的快照保留。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ContextClearPreview {
    pub document_id: String,
    #[ts(type = "number")]
    pub conversation_count: i64,
    #[ts(type = "number")]
    pub message_count: i64,
    #[ts(type = "number")]
    pub snapshot_total: i64,
    /// 无属主且未被保护、可随清除删除的快照。
    pub deletable_snapshot_ids: Vec<String>,
    pub protected_snapshots: Vec<ProtectedSnapshotView>,
    /// 因删除而需清空 active_snapshot_id 的会话数。
    #[ts(type = "number")]
    pub conversations_clearing_active: i64,
    /// 同事务按序执行步骤(M01 消费;M07 不执行)。
    pub plan: Vec<ClearPlanStep>,
}

// —— 数据库行(内部)——

#[derive(Debug, Clone)]
pub struct SnapshotRow {
    pub id: String,
    pub document_id: String,
    pub document_version_id: String,
    pub parse_generation_id: Option<String>,
    pub scope_kind: ScopeKind,
    pub scope_json: String,
    pub coverage_json: String,
    pub block_set_hash: Option<String>,
    pub canonical_hash: String,
    pub idempotency_key: String,
    pub created_at: i64,
}

pub const SNAPSHOT_COLS: &str = "id, document_id, document_version_id, parse_generation_id, \
     scope_kind, scope_json, coverage_json, block_set_hash, canonical_hash, idempotency_key, \
     created_at";

impl<'r> TryFrom<&rusqlite::Row<'r>> for SnapshotRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_id: r.get("document_id")?,
            document_version_id: r.get("document_version_id")?,
            parse_generation_id: r.get("parse_generation_id")?,
            scope_kind: ScopeKind::parse_db(&r.get::<_, String>("scope_kind")?)
                .unwrap_or(ScopeKind::Sources),
            scope_json: r.get("scope_json")?,
            coverage_json: r.get("coverage_json")?,
            block_set_hash: r.get("block_set_hash")?,
            canonical_hash: r.get("canonical_hash")?,
            idempotency_key: r.get("idempotency_key")?,
            created_at: r.get("created_at")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ConversationRow {
    pub id: String,
    pub document_id: String,
    pub title: String,
    pub active_snapshot_id: Option<String>,
    pub pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

pub const CONVERSATION_COLS: &str = "id, document_id, title, active_snapshot_id, pinned, \
     created_at, updated_at";

impl<'r> TryFrom<&rusqlite::Row<'r>> for ConversationRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_id: r.get("document_id")?,
            title: r.get("title")?,
            active_snapshot_id: r.get("active_snapshot_id")?,
            pinned: r.get::<_, i64>("pinned")? != 0,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: String,
    pub conversation_id: String,
    pub role: MessageRole,
    pub snapshot_id: Option<String>,
    pub task_id: Option<String>,
    pub content_json: String,
    pub answer_status: AnswerLifecycle,
    pub model_provider_id: Option<String>,
    pub model_id: Option<String>,
    pub error_code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub const MESSAGE_COLS: &str = "id, conversation_id, role, snapshot_id, task_id, content_json, \
     answer_status, model_provider_id, model_id, error_code, created_at, updated_at";

impl<'r> TryFrom<&rusqlite::Row<'r>> for MessageRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            conversation_id: r.get("conversation_id")?,
            role: MessageRole::parse_db(&r.get::<_, String>("role")?).unwrap_or(MessageRole::User),
            snapshot_id: r.get("snapshot_id")?,
            task_id: r.get("task_id")?,
            content_json: r.get("content_json")?,
            answer_status: AnswerLifecycle::parse_db(&r.get::<_, String>("answer_status")?)
                .unwrap_or(AnswerLifecycle::Pending),
            model_provider_id: r.get("model_provider_id")?,
            model_id: r.get("model_id")?,
            error_code: r.get("error_code")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }
}
