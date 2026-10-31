//! M05 领域对象与对外 DTO(《01》§4.3/§5.3、《M05》§2–§8)。
//!
//! 契约:结构校验与语义支持分离。`structuralStatus=valid` 只证明目标存在、
//! 版本一致、引文匹配且定位可解析;`supportStatus=supported` 仍必须记录
//! 支持来源(none/model/rule/user)。任何模块不得把模型引用直接标为已核实。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::locator::SourceLocator;

// —— 枚举(封闭联合,数据库小写 snake_case)——

/// 选区种类,与 M03 RawSelection.selectionKind 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum AnchorKind {
    TextSelection,
    Page,
    Section,
}

impl AnchorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TextSelection => "text_selection",
            Self::Page => "page",
            Self::Section => "section",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "text_selection" => Self::TextSelection,
            "page" => Self::Page,
            "section" => Self::Section,
            _ => return None,
        })
    }
}

/// Anchor 存储状态。有效状态在读取时结合文档当前版本派生:
/// 版本落后于当前版本 ⇒ orphaned(用户确认前不自动绑定新版本)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum AnchorStatus {
    Active,
    Orphaned,
    Relocated,
    Invalid,
}

impl AnchorStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Orphaned => "orphaned",
            Self::Relocated => "relocated",
            Self::Invalid => "invalid",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "active" => Self::Active,
            "orphaned" => Self::Orphaned,
            "relocated" => Self::Relocated,
            "invalid" => Self::Invalid,
            _ => return None,
        })
    }
}

/// 结构校验状态(《01》§4.3、《M05》§4 算法输出)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum StructuralStatus {
    Valid,
    TargetMissing,
    QuoteMismatch,
    VersionMismatch,
    LocatorInvalid,
}

impl StructuralStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::TargetMissing => "target_missing",
            Self::QuoteMismatch => "quote_mismatch",
            Self::VersionMismatch => "version_mismatch",
            Self::LocatorInvalid => "locator_invalid",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "valid" => Self::Valid,
            "target_missing" => Self::TargetMissing,
            "quote_mismatch" => Self::QuoteMismatch,
            "version_mismatch" => Self::VersionMismatch,
            "locator_invalid" => Self::LocatorInvalid,
            _ => return None,
        })
    }

    /// 对应稳定错误码(命令层拒绝时使用)。
    pub fn error_code(&self) -> crate::shared::CoreErrorCode {
        match self {
            Self::Valid => crate::shared::CoreErrorCode::Internal,
            Self::TargetMissing => crate::shared::CoreErrorCode::CitationTargetMissing,
            Self::QuoteMismatch => crate::shared::CoreErrorCode::CitationQuoteMismatch,
            Self::VersionMismatch => crate::shared::CoreErrorCode::CitationTargetMissing,
            Self::LocatorInvalid => crate::shared::CoreErrorCode::CitationQuoteMismatch,
        }
    }
}

/// 语义支持状态:结构有效不等于语义支持(《00》§3.2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SupportStatus {
    Unverified,
    Supported,
    Insufficient,
    Conflict,
}

impl SupportStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Supported => "supported",
            Self::Insufficient => "insufficient",
            Self::Conflict => "conflict",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "unverified" => Self::Unverified,
            "supported" => Self::Supported,
            "insufficient" => Self::Insufficient,
            "conflict" => Self::Conflict,
            _ => return None,
        })
    }
}

/// 支持判断来源:模型自检只能写 model;用户确认才可写 user(《M05》§4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SupportSource {
    None,
    Model,
    Rule,
    User,
}

impl SupportSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Model => "model",
            Self::Rule => "rule",
            Self::User => "user",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Self::None,
            "model" => Self::Model,
            "rule" => Self::Rule,
            "user" => Self::User,
            _ => return None,
        })
    }
}

/// Citation 属主:M05 不创建 owner,只校验存在与合法(《M05》§3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum CitationOwnerType {
    Message,
    Note,
    ArtifactVersion,
    Clarification,
}

impl CitationOwnerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Note => "note",
            Self::ArtifactVersion => "artifact_version",
            Self::Clarification => "clarification",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "message" => Self::Message,
            "note" => Self::Note,
            "artifact_version" => Self::ArtifactVersion,
            "clarification" => Self::Clarification,
            _ => return None,
        })
    }
}

/// Citation 证据来源类型。文档事实只能以 content_block / anchor 为权威;
/// note / user_view 为用户观点层,message / artifact_version / ai_draft 为
/// AI 草稿层,展示时不得使用与原文相同的视觉标签(《M05》§3/§5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum CitationSourceType {
    ContentBlock,
    Anchor,
    Note,
    Message,
    ArtifactVersion,
    Selection,
    UserView,
    AiDraft,
}

impl CitationSourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ContentBlock => "content_block",
            Self::Anchor => "anchor",
            Self::Note => "note",
            Self::Message => "message",
            Self::ArtifactVersion => "artifact_version",
            Self::Selection => "selection",
            Self::UserView => "user_view",
            Self::AiDraft => "ai_draft",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "content_block" => Self::ContentBlock,
            "anchor" => Self::Anchor,
            "note" => Self::Note,
            "message" => Self::Message,
            "artifact_version" => Self::ArtifactVersion,
            "selection" => Self::Selection,
            "user_view" => Self::UserView,
            "ai_draft" => Self::AiDraft,
            _ => return None,
        })
    }

    /// 来源层级(《01》§5.3 sourceRole 语义)。
    pub fn source_tier(&self) -> SourceTier {
        match self {
            Self::ContentBlock | Self::Anchor | Self::Selection => SourceTier::Original,
            Self::Note | Self::UserView => SourceTier::UserNote,
            Self::Message | Self::ArtifactVersion | Self::AiDraft => SourceTier::AiDraft,
        }
    }

    /// 是否为文档事实的合法权威证据(《M05》§3)。
    pub fn is_document_fact_source(&self) -> bool {
        matches!(self, Self::ContentBlock | Self::Anchor)
    }
}

/// 来源层级:界面必须区分展示,不得混同原文(《00》§3.2、《M05》§5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SourceTier {
    Original,
    UserNote,
    AiDraft,
}

/// 结论段类型(与 AnswerContent segments.kind 对齐,《01》§5.2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ClaimKind {
    DocumentFact,
    Inference,
    Example,
    UserView,
    GeneralNote,
}

impl ClaimKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DocumentFact => "document_fact",
            Self::Inference => "inference",
            Self::Example => "example",
            Self::UserView => "user_view",
            Self::GeneralNote => "general_note",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "document_fact" => Self::DocumentFact,
            "inference" => Self::Inference,
            "example" => Self::Example,
            "user_view" => Self::UserView,
            "general_note" => Self::GeneralNote,
            _ => return None,
        })
    }
}

/// 证据反馈类型(《M05》§5):append-only,保留原结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum EvidenceFeedbackType {
    QuoteWrong,
    InsufficientEvidence,
    ScopeWrong,
    ConflictWithSource,
}

impl EvidenceFeedbackType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::QuoteWrong => "quote_wrong",
            Self::InsufficientEvidence => "insufficient_evidence",
            Self::ScopeWrong => "scope_wrong",
            Self::ConflictWithSource => "conflict_with_source",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "quote_wrong" => Self::QuoteWrong,
            "insufficient_evidence" => Self::InsufficientEvidence,
            "scope_wrong" => Self::ScopeWrong,
            "conflict_with_source" => Self::ConflictWithSource,
            _ => return None,
        })
    }
}

/// 重定位状态:候选不自动升级,只有 confirmed 建立新关联(《M05》§7)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum RelocationStatus {
    Proposed,
    Confirmed,
    Rejected,
}

impl RelocationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "proposed" => Self::Proposed,
            "confirmed" => Self::Confirmed,
            "rejected" => Self::Rejected,
            _ => return None,
        })
    }
}

/// 重定位匹配方法(差异说明的机器可读部分)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum RelocationMethod {
    /// 文本哈希完全一致。
    Exact,
    /// 规范化后一致(空白/软连字符/兼容差异)。
    Normalized,
    /// 目标块包含原引文。
    Contains,
    /// 规范化包含。
    NormalizedContains,
}

impl RelocationMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Normalized => "normalized",
            Self::Contains => "contains",
            Self::NormalizedContains => "normalized_contains",
        }
    }

    /// 人类可读差异说明(候选展示用)。
    pub fn diff_note(&self) -> &'static str {
        match self {
            Self::Exact => "文本完全一致",
            Self::Normalized => "规范化后一致(空白或软连字符差异)",
            Self::Contains => "目标块包含原文引文",
            Self::NormalizedContains => "规范化后包含原文引文",
        }
    }
}

// —— 选区输入(M03 RawSelection 的最小投影,《M05》§2)——

/// M03 RawSelection.selectionKind 的对齐枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SelectionKind {
    Text,
    Page,
    Section,
}

impl SelectionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Page => "page",
            Self::Section => "section",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "text" => Self::Text,
            "page" => Self::Page,
            "section" => Self::Section,
            _ => return None,
        })
    }

    pub fn to_anchor_kind(&self) -> AnchorKind {
        match self {
            Self::Text => AnchorKind::TextSelection,
            Self::Page => AnchorKind::Page,
            Self::Section => AnchorKind::Section,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct RawSelectionBlockInput {
    pub render_key: String,
    pub text: String,
    #[ts(type = "number")]
    pub block_order: i64,
    pub locator: SourceLocator,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct RawSelectionInput {
    pub document_id: String,
    pub document_version_id: String,
    pub format: crate::document::models::DocumentFormat,
    pub text: String,
    pub blocks: Vec<RawSelectionBlockInput>,
    pub primary_locator: SourceLocator,
    #[serde(default)]
    pub selection_kind: Option<SelectionKind>,
}

// —— 视图 ——

/// AnchorBlock 投影:按原文顺序,每块引文可独立校验。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnchorBlockView {
    pub block_id: String,
    #[ts(type = "number")]
    pub position: i64,
    pub quote_text: String,
    pub locator: SourceLocator,
}

/// AnchorView:含 M03 可显示的 overlay 投影(块定位 + 引文)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnchorView {
    pub id: String,
    pub document_id: String,
    pub document_version_id: String,
    pub kind: AnchorKind,
    /// 有效状态(结合文档当前版本派生;孤立 ≠ 删除)。
    pub status: AnchorStatus,
    pub exact_text: String,
    pub snapshot_text: String,
    pub primary_locator: SourceLocator,
    pub blocks: Vec<AnchorBlockView>,
    /// overlay:PDF 主页面号列表(Markdown/DOCX 为空)。
    pub overlay_pages: Vec<i64>,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// 锚点摘要:M06 等下游模块的只读投影(不携带正文以外的字段)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnchorSummary {
    pub id: String,
    pub document_id: String,
    pub document_version_id: String,
    pub kind: AnchorKind,
    pub status: AnchorStatus,
}

// —— Citation 输入与视图 ——

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct CitationInput {
    /// 结论段标识(如 AnswerContent segment id)。
    pub claim_key: String,
    pub claim_kind: ClaimKind,
    pub source_type: CitationSourceType,
    /// sourceType 对应实体 ID(content_block/anchor/note/message/artifact_version)。
    #[serde(default)]
    #[ts(optional)]
    pub source_id: Option<String>,
    /// sourceType=anchor 时的锚点(也可由 sourceId 提供)。
    #[serde(default)]
    #[ts(optional)]
    pub anchor_id: Option<String>,
    /// sourceType=content_block 时必填;其他类型可空。
    #[serde(default)]
    #[ts(optional)]
    pub content_block_id: Option<String>,
    /// 模型给出的引文;规范化匹配命中时保存原文切片。
    pub quoted_text: String,
    /// 可选显式定位;与文档格式不匹配 ⇒ locator_invalid。
    #[serde(default)]
    #[ts(optional)]
    pub locator: Option<SourceLocator>,
    /// 模型自检输出(仅允许 supportSource=model/rule;user 由确认命令写入)。
    #[serde(default)]
    #[ts(optional)]
    pub support_status: Option<SupportStatus>,
    #[serde(default)]
    #[ts(optional)]
    pub support_source: Option<SupportSource>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct CitationView {
    pub id: String,
    pub document_id: String,
    pub document_version_id: String,
    pub owner_type: CitationOwnerType,
    pub owner_id: String,
    pub claim_key: String,
    pub claim_kind: ClaimKind,
    pub source_type: CitationSourceType,
    pub source_tier: SourceTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content_block_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub anchor_id: Option<String>,
    pub quoted_text: String,
    /// 原文快照(引文命中时与 quotedText 一致;块缺失时保留输入)。
    pub source_snapshot: String,
    pub structural_status: StructuralStatus,
    pub support_status: SupportStatus,
    pub support_source: SupportSource,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// 批量保存请求(M09 在任务提交事务内调用;批量超限拒绝整段结果)。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SaveCitationsRequest {
    pub owner_type: CitationOwnerType,
    pub owner_id: String,
    pub document_id: String,
    pub document_version_id: String,
    pub citations: Vec<CitationInput>,
    pub idempotency_key: String,
}

// —— 回跳解析(《M05》§6)——

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ResolutionMode {
    /// 结构有效且版本为当前版本:可回跳(等待 M03 页面渲染)。
    Navigate,
    /// 打开原文快照或只读面板,显示原因;不打开错误页。
    Snapshot,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct EvidenceResolution {
    pub citation_id: String,
    pub structural_status: StructuralStatus,
    pub mode: ResolutionMode,
    /// navigate 模式下的定位(M03 ReaderNavigationPort 消费)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub locator: Option<SourceLocator>,
    /// PDF 主页面号(便于等待页面 ready)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub block_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub block_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub heading_path: Option<Vec<String>>,
    /// 引文在块内 UTF-16 起止(前端高亮)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub quote_start_utf16: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub quote_end_utf16: Option<i64>,
    pub quote_text: String,
    /// snapshot 模式下的原文快照。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub snapshot_text: Option<String>,
    pub document_version_id: String,
    /// snapshot 模式原因(结构失效 / 版本已被取代 / 锚点孤立)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
}

// —— 证据透镜(《M05》§5)——

/// 透镜中的单条证据:结论片段 + 引文 + 状态 + 来源层级。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct LensCitationView {
    pub citation_id: String,
    pub claim_key: String,
    pub claim_kind: ClaimKind,
    pub source_type: CitationSourceType,
    pub source_tier: SourceTier,
    pub quoted_text: String,
    pub structural_status: StructuralStatus,
    pub support_status: SupportStatus,
    pub support_source: SupportSource,
    pub document_version_id: String,
    /// 原文顺序(块 ordinal;无块引用时按创建顺序)。
    #[ts(type = "number")]
    pub order: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub heading_path: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub anchor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub anchor_status: Option<AnchorStatus>,
}

/// 缺失/冲突证据条目(透镜第三区域输入)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct LensIssueView {
    pub citation_id: String,
    pub claim_key: String,
    /// missing_evidence | conflict | structural_invalid | feedback_conflict。
    pub issue_kind: String,
    pub description: String,
}

/// 证据透镜:结论段引文按原文顺序 + 来源层级 + 缺失/冲突证据。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct EvidenceLensView {
    pub owner_type: CitationOwnerType,
    pub owner_id: String,
    pub document_id: String,
    pub citations: Vec<LensCitationView>,
    pub missing_evidence: Vec<LensIssueView>,
    pub conflicts: Vec<LensIssueView>,
    #[ts(type = "number")]
    pub feedback_count: i64,
}

// —— 反馈 ——

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct FeedbackResult {
    pub feedback_id: String,
    pub citation_id: String,
    pub feedback_type: EvidenceFeedbackType,
    /// 该引文累计反馈数(append-only)。
    #[ts(type = "number")]
    pub total_count: i64,
    #[ts(type = "number")]
    pub created_at: i64,
}

// —— 重定位(《M05》§7)——

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct RelocationBlockMapping {
    pub from_block_id: String,
    pub to_block_id: String,
    pub to_block_key: String,
    #[ts(type = "number")]
    pub to_ordinal: i64,
    pub method: RelocationMethod,
    /// 新版本中的匹配片段(原文切片)。
    pub matched_text: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct RelocationCandidate {
    pub id: String,
    pub anchor_id: String,
    pub target_version_id: String,
    pub score: f64,
    pub status: RelocationStatus,
    /// 单一候选的映射说明(每块证据)。
    pub mappings: Vec<RelocationBlockMapping>,
    /// 人类可读差异说明。
    pub diff_note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confirmed_anchor_id: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
}

// —— 证据保留裁决(《M05》§8)——

/// 清理类别(M08 清理计划的输入子集;仅 M05 裁决块级证据)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum RetentionCategory {
    /// FTS 行:可重建缓存,清除不影响引用。
    FtsIndex,
    /// 向量行:可重建缓存,清除不影响引用。
    VectorIndex,
    /// 未被引用的 ContentBlock(解析缓存)。
    ParseCache,
}

impl RetentionCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FtsIndex => "fts_index",
            Self::VectorIndex => "vector_index",
            Self::ParseCache => "parse_cache",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "fts_index" => Self::FtsIndex,
            "vector_index" => Self::VectorIndex,
            "parse_cache" => Self::ParseCache,
            _ => return None,
        })
    }
}

/// 受保护块与原因。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ProtectedBlockView {
    pub block_id: String,
    /// anchor_reference | citation_reference | current_generation | on_demand_active
    pub reason: String,
}

/// 每类别的裁决摘要。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct RetentionCategoryRuling {
    pub category: RetentionCategory,
    #[ts(type = "number")]
    pub protected_count: i64,
    #[ts(type = "number")]
    pub cleanable_count: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct RetentionRuling {
    pub document_id: String,
    pub categories: Vec<RetentionCategoryRuling>,
    pub protected_blocks: Vec<ProtectedBlockView>,
}

// —— 数据库行(内部)——

#[derive(Debug, Clone)]
pub struct AnchorRow {
    pub id: String,
    pub document_id: String,
    pub document_version_id: String,
    pub kind: AnchorKind,
    pub status: AnchorStatus,
    pub exact_text: String,
    pub prefix_text: String,
    pub suffix_text: String,
    pub snapshot_text: String,
    pub text_hash: String,
    pub primary_locator_json: String,
    pub created_at: i64,
}

pub const ANCHOR_COLS: &str = "id, document_id, document_version_id, kind, status, exact_text, \
     prefix_text, suffix_text, snapshot_text, text_hash, primary_locator_json, created_at";

impl<'r> TryFrom<&rusqlite::Row<'r>> for AnchorRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_id: r.get("document_id")?,
            document_version_id: r.get("document_version_id")?,
            kind: AnchorKind::parse_db(&r.get::<_, String>("kind")?)
                .unwrap_or(AnchorKind::TextSelection),
            status: AnchorStatus::parse_db(&r.get::<_, String>("status")?)
                .unwrap_or(AnchorStatus::Invalid),
            exact_text: r.get("exact_text")?,
            prefix_text: r.get("prefix_text")?,
            suffix_text: r.get("suffix_text")?,
            snapshot_text: r.get("snapshot_text")?,
            text_hash: r.get("text_hash")?,
            primary_locator_json: r.get("primary_locator_json")?,
            created_at: r.get("created_at")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct AnchorBlockRow {
    pub anchor_id: String,
    pub block_id: String,
    pub position: i64,
    pub block_quote_text: String,
}

#[derive(Debug, Clone)]
pub struct CitationRow {
    pub id: String,
    pub document_id: String,
    pub document_version_id: String,
    pub owner_type: CitationOwnerType,
    pub owner_id: String,
    pub claim_key: String,
    pub claim_kind: ClaimKind,
    pub source_type: CitationSourceType,
    pub source_id: Option<String>,
    pub content_block_id: Option<String>,
    pub anchor_id: Option<String>,
    pub quoted_text: String,
    pub source_snapshot: String,
    pub structural_status: StructuralStatus,
    pub support_status: SupportStatus,
    pub support_source: SupportSource,
    pub created_at: i64,
}

pub const CITATION_COLS: &str = "id, document_id, document_version_id, owner_type, owner_id, \
     claim_key, claim_kind, source_type, source_id, content_block_id, anchor_id, quoted_text, \
     source_snapshot, structural_status, support_status, support_source, created_at";

impl CitationRow {
    pub fn to_view(&self) -> CitationView {
        CitationView {
            id: self.id.clone(),
            document_id: self.document_id.clone(),
            document_version_id: self.document_version_id.clone(),
            owner_type: self.owner_type,
            owner_id: self.owner_id.clone(),
            claim_key: self.claim_key.clone(),
            claim_kind: self.claim_kind,
            source_type: self.source_type,
            source_tier: self.source_type.source_tier(),
            source_id: self.source_id.clone(),
            content_block_id: self.content_block_id.clone(),
            anchor_id: self.anchor_id.clone(),
            quoted_text: self.quoted_text.clone(),
            source_snapshot: self.source_snapshot.clone(),
            structural_status: self.structural_status,
            support_status: self.support_status,
            support_source: self.support_source,
            created_at: self.created_at,
        }
    }
}

impl<'r> TryFrom<&rusqlite::Row<'r>> for CitationRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_id: r.get("document_id")?,
            document_version_id: r.get("document_version_id")?,
            owner_type: CitationOwnerType::parse_db(&r.get::<_, String>("owner_type")?)
                .unwrap_or(CitationOwnerType::Message),
            owner_id: r.get("owner_id")?,
            claim_key: r.get("claim_key")?,
            claim_kind: ClaimKind::parse_db(&r.get::<_, String>("claim_kind")?)
                .unwrap_or(ClaimKind::Inference),
            source_type: CitationSourceType::parse_db(&r.get::<_, String>("source_type")?)
                .unwrap_or(CitationSourceType::ContentBlock),
            source_id: r.get("source_id")?,
            content_block_id: r.get("content_block_id")?,
            anchor_id: r.get("anchor_id")?,
            quoted_text: r.get("quoted_text")?,
            source_snapshot: r.get("source_snapshot")?,
            structural_status: StructuralStatus::parse_db(
                &r.get::<_, String>("structural_status")?,
            )
            .unwrap_or(StructuralStatus::TargetMissing),
            support_status: SupportStatus::parse_db(&r.get::<_, String>("support_status")?)
                .unwrap_or(SupportStatus::Unverified),
            support_source: SupportSource::parse_db(&r.get::<_, String>("support_source")?)
                .unwrap_or(SupportSource::None),
            created_at: r.get("created_at")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RelocationRow {
    pub id: String,
    pub anchor_id: String,
    pub target_version_id: String,
    pub candidate_json: String,
    pub score: f64,
    pub status: RelocationStatus,
    pub created_at: i64,
    pub confirmed_at: Option<i64>,
}
