//! M06 领域对象与对外 DTO(《01》§5.3/§6、《M06》§2–§8)。
//!
//! 契约:笔记、标注和待澄清是用户资产,优先于后台任务保存;模块只使用
//! M05 创建的 Anchor(经 evidence::anchor_summary_tx),不自行计算页码或块身份。
//! `origin` 区分用户笔记与 AI 草稿,参与 AI 上下文时始终保留来源层级。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::locator::SourceLocator;
use crate::shared::paging::Page;

// —— 枚举 ——

/// 高亮颜色:显示分类,不隐含掌握程度(《M06》§2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum AnnotationColor {
    Yellow,
    Green,
    Blue,
    Pink,
    Purple,
}

impl AnnotationColor {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Pink => "pink",
            Self::Purple => "purple",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "yellow" => Self::Yellow,
            "green" => Self::Green,
            "blue" => Self::Blue,
            "pink" => Self::Pink,
            "purple" => Self::Purple,
            _ => return None,
        })
    }
}

/// 笔记来源:AI 回答保存先建 ai_draft;用户编辑后转 user_edited_ai,
/// 仍显示 AI 来源;普通手动笔记为 user(《M06》§3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum NoteOrigin {
    User,
    AiDraft,
    UserEditedAi,
}

impl NoteOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::AiDraft => "ai_draft",
            Self::UserEditedAi => "user_edited_ai",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "user" => Self::User,
            "ai_draft" => Self::AiDraft,
            "user_edited_ai" => Self::UserEditedAi,
            _ => return None,
        })
    }

    /// 保存请求允许携带的 origin:创建只允许 user/ai_draft;
    /// user_edited_ai 只能由编辑 ai_draft 笔记产生(服务端推导)。
    pub fn allowed_on_save(&self) -> bool {
        matches!(self, Self::User | Self::AiDraft)
    }
}

/// 笔记状态:用于筛选与上下文材料选择,不代表掌握度(《M06》§5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum NoteStatus {
    Open,
    Confirmed,
    NeedsClarification,
}

impl NoteStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Confirmed => "confirmed",
            Self::NeedsClarification => "needs_clarification",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "confirmed" => Self::Confirmed,
            "needs_clarification" => Self::NeedsClarification,
            _ => return None,
        })
    }
}

/// 待澄清状态(《M06》§6):resolved 必须携带 resolutionRef。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ClarificationStatus {
    Pending,
    Resolved,
    Deferred,
}

impl ClarificationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::Deferred => "deferred",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "resolved" => Self::Resolved,
            "deferred" => Self::Deferred,
            _ => return None,
        })
    }
}

/// 待澄清来源:用户问题、助手回答、标注、笔记或错题
/// (question/answer/quiz 为 M07/M11 前向引用,创建时携带快照)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ClarificationSourceType {
    Question,
    Answer,
    Annotation,
    Note,
    Quiz,
}

impl ClarificationSourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Answer => "answer",
            Self::Annotation => "annotation",
            Self::Note => "note",
            Self::Quiz => "quiz",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "question" => Self::Question,
            "answer" => Self::Answer,
            "annotation" => Self::Annotation,
            "note" => Self::Note,
            "quiz" => Self::Quiz,
            _ => return None,
        })
    }

    /// 创建时必须在数据库中存在的来源类型。
    pub fn requires_existing_source(&self) -> bool {
        matches!(self, Self::Annotation | Self::Note)
    }
}

// —— 来源引用(《01》§5.3)——

/// SourceReference:笔记、待澄清与产物来源摘要的统一形态。
/// 不替代 Citation:文档事实的支持关系仍以 Citation 和 ContentBlock 校验为准。
/// 来源被删除后保留引用 ID 与快照,界面显示"来源不可用"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SourceReference {
    pub source_type: SourceRefType,
    pub source_id: String,
    pub source_role: SourceRole,
    #[serde(default)]
    #[ts(optional)]
    pub display_name: Option<String>,
    pub citation_ids: Vec<String>,
    /// 快照是否随引用保存(保存时恒为 true;读取时按快照存在性)。
    pub snapshot_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SourceRefType {
    ContentBlock,
    Note,
    Message,
    ArtifactVersion,
    Selection,
    /// 标注作为来源:笔记由高亮/涂鸦/文本框转写时保留原标注身份。
    Annotation,
}

impl SourceRefType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ContentBlock => "content_block",
            Self::Note => "note",
            Self::Message => "message",
            Self::ArtifactVersion => "artifact_version",
            Self::Selection => "selection",
            Self::Annotation => "annotation",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "content_block" => Self::ContentBlock,
            "note" => Self::Note,
            "message" => Self::Message,
            "artifact_version" => Self::ArtifactVersion,
            "selection" => Self::Selection,
            "annotation" => Self::Annotation,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SourceRole {
    Original,
    UserNote,
    AiDraft,
}

impl SourceRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::UserNote => "user_note",
            Self::AiDraft => "ai_draft",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "original" => Self::Original,
            "user_note" => Self::UserNote,
            "ai_draft" => Self::AiDraft,
            _ => return None,
        })
    }
}

/// 解决结果引用:resolved 状态必填;note 需同文档,message/artifact_version
/// 为 M07/M10 前向引用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ResolutionRef {
    pub ref_type: ResolutionRefType,
    pub ref_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ResolutionRefType {
    Note,
    Message,
    ArtifactVersion,
}

impl ResolutionRefType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Message => "message",
            Self::ArtifactVersion => "artifact_version",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "note" => Self::Note,
            "message" => Self::Message,
            "artifact_version" => Self::ArtifactVersion,
            _ => return None,
        })
    }
}

// —— 视图 ——

/// 标注视图:状态由 Anchor 有效状态派生(《M06》§2)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnnotationView {
    pub id: String,
    pub document_id: String,
    pub anchor_id: String,
    pub color: AnnotationColor,
    pub note_text: String,
    /// 可读备注(与 note_text 的样式/笔迹/文本框载荷分开;编辑备注不改内容)。
    #[serde(default)]
    pub remark: String,
    pub anchor_status: crate::evidence::models::AnchorStatus,
    /// 派生展示提示:active 可回跳;orphaned 需重新确认;invalid 显示快照。
    pub status_hint: String,
    pub quote_text: String,
    pub locator: Option<SourceLocator>,
    pub page_number: Option<i64>,
    #[ts(type = "number")]
    pub revision: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct BookmarkView {
    pub id: String,
    pub document_id: String,
    pub document_version_id: Option<String>,
    pub anchor_id: Option<String>,
    pub label: String,
    /// 书签备注(与名称分开;可为空)。
    #[serde(default)]
    pub note: String,
    pub locator: SourceLocator,
    pub page_number: Option<i64>,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// 书签切换结果:期望状态语义,双击不产生两个相反结果。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct BookmarkResult {
    pub document_id: String,
    /// 切换后的书签存在状态。
    pub state: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bookmark: Option<BookmarkView>,
}

/// 笔记视图(《M06》§3 逐字段对齐)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct NoteView {
    pub id: String,
    pub document_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub document_version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub anchor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub anchor_status: Option<crate::evidence::models::AnchorStatus>,
    pub origin: NoteOrigin,
    pub body: String,
    /// 显示名称(保留大小写),按名称排序。
    pub tags: Vec<String>,
    pub status: NoteStatus,
    pub source_refs: Vec<SourceReference>,
    /// 来源仍存在(读取时校验;删除后保留快照并提示来源不可用)。
    pub source_refs_available: Vec<bool>,
    #[ts(type = "number")]
    pub revision: i64,
    /// 锚点定位(Markdown/DOCX/PDF):笔记详情可回跳来源;无锚点时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub locator: Option<SourceLocator>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

pub type NotePage = Page<NoteView>;

/// 列表筛选(《M06》§7 辅助抽屉)。
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct NoteListFilter {
    /// 标签(规范化名称,大小写不敏感)。
    #[serde(default)]
    #[ts(optional)]
    pub tag: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub status: Option<NoteStatus>,
    #[serde(default)]
    #[ts(optional)]
    pub origin: Option<NoteOrigin>,
    #[serde(default)]
    #[ts(optional)]
    pub anchor_id: Option<String>,
}

/// 待澄清视图:来源删除后保留条目并标记不可用(《M06》§6)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClarificationView {
    pub id: String,
    pub document_id: String,
    pub question_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_type: Option<ClarificationSourceType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_id: Option<String>,
    /// 来源快照(创建时保存,来源删除后仍可查看)。
    pub source_snapshot: ClarificationSourceSnapshot,
    /// 来源当前是否仍存在(读取时校验)。
    pub source_available: bool,
    /// 可读备注(与问题正文分开;不改变问题/状态)。
    #[serde(default)]
    pub remark: String,
    pub status: ClarificationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resolution_ref: Option<ResolutionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resolved_at: Option<i64>,
    #[ts(type = "number")]
    pub revision: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// 来源快照:可回跳引用与文本。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClarificationSourceSnapshot {
    /// 来源文本快照。
    #[serde(default)]
    pub text: String,
    /// 可回跳引用(M05 Anchor)。
    #[serde(default)]
    #[ts(optional)]
    pub anchor_id: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub locator: Option<SourceLocator>,
}

pub type ClarificationPage = Page<ClarificationView>;

// —— 数据清理预览(M06 §8;M08 DataClearContributor 的输入)——

/// 用户数据清除类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum UserdataClearCategory {
    Annotations,
    Bookmarks,
    Notes,
    Clarifications,
    Tags,
}

impl UserdataClearCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Annotations => "annotations",
            Self::Bookmarks => "bookmarks",
            Self::Notes => "notes",
            Self::Clarifications => "clarifications",
            Self::Tags => "tags",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "annotations" => Self::Annotations,
            "bookmarks" => Self::Bookmarks,
            "notes" => Self::Notes,
            "clarifications" => Self::Clarifications,
            "tags" => Self::Tags,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClearCategoryCount {
    pub category: UserdataClearCategory,
    #[ts(type = "number")]
    pub count: i64,
    /// 会受影响的引用数量(如笔记拥有的 Citation)。
    #[ts(type = "number")]
    pub affected_citations: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct UserdataClearPreview {
    pub document_id: String,
    pub categories: Vec<ClearCategoryCount>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct UserdataClearResult {
    pub document_id: String,
    pub deleted: Vec<ClearCategoryCount>,
}

// —— 输入 DTO ——

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnnotationCreateInput {
    pub document_id: String,
    pub anchor_id: String,
    pub color: AnnotationColor,
    #[serde(default)]
    pub note_text: String,
    #[serde(default)]
    #[ts(optional)]
    pub remark: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnnotationUpdateInput {
    pub annotation_id: String,
    pub color: AnnotationColor,
    #[serde(default)]
    pub note_text: String,
    /// None = 保留现有备注;Some("") = 清空备注。
    #[serde(default)]
    #[ts(optional)]
    pub remark: Option<String>,
    #[ts(type = "number")]
    pub expected_revision: i64,
}

/// 保存请求:noteId 为空 ⇒ 创建;origin 在创建时允许 user/ai_draft。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct NoteSaveInput {
    #[serde(default)]
    #[ts(optional)]
    pub note_id: Option<String>,
    pub document_id: String,
    #[serde(default)]
    #[ts(optional)]
    pub anchor_id: Option<String>,
    pub body: String,
    pub origin: NoteOrigin,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_note_status")]
    pub status: NoteStatus,
    #[serde(default)]
    pub source_refs: Vec<SourceReference>,
    #[ts(type = "number")]
    pub expected_revision: i64,
}

fn default_note_status() -> NoteStatus {
    NoteStatus::Open
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClarificationCreateInput {
    pub document_id: String,
    pub question_text: String,
    #[serde(default)]
    #[ts(optional)]
    pub source_type: Option<ClarificationSourceType>,
    #[serde(default)]
    #[ts(optional)]
    pub source_id: Option<String>,
    pub source_snapshot: ClarificationSourceSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClarificationUpdateInput {
    pub clarification_id: String,
    pub status: ClarificationStatus,
    #[serde(default)]
    #[ts(optional)]
    pub resolution_ref: Option<ResolutionRef>,
    #[ts(type = "number")]
    pub expected_revision: i64,
}

/// 只改待澄清备注:不改问题、来源、状态与解决引用。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ClarificationRemarkInput {
    pub clarification_id: String,
    #[serde(default)]
    pub remark: String,
    #[ts(type = "number")]
    pub expected_revision: i64,
}

// —— 数据库行(内部)——

#[derive(Debug, Clone)]
pub struct AnnotationRow {
    pub id: String,
    pub document_id: String,
    pub anchor_id: String,
    pub color: AnnotationColor,
    pub note_text: String,
    pub remark: String,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct BookmarkRow {
    pub id: String,
    pub document_id: String,
    pub document_version_id: Option<String>,
    pub anchor_id: Option<String>,
    pub toggle_key: String,
    pub label: String,
    pub note: String,
    pub locator_json: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NoteRow {
    pub id: String,
    pub document_id: String,
    pub document_version_id: Option<String>,
    pub anchor_id: Option<String>,
    pub origin: NoteOrigin,
    pub body: String,
    pub status: NoteStatus,
    pub source_refs_json: String,
    pub revision: i64,
    pub idempotency_key: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub const NOTE_COLS: &str =
    "id, document_id, document_version_id, anchor_id, origin, body, status, source_refs_json, \
     revision, idempotency_key, created_at, updated_at";

impl<'r> TryFrom<&rusqlite::Row<'r>> for NoteRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_id: r.get("document_id")?,
            document_version_id: r.get("document_version_id")?,
            anchor_id: r.get("anchor_id")?,
            origin: NoteOrigin::parse_db(&r.get::<_, String>("origin")?)
                .unwrap_or(NoteOrigin::User),
            body: r.get("body")?,
            status: NoteStatus::parse_db(&r.get::<_, String>("status")?)
                .unwrap_or(NoteStatus::Open),
            source_refs_json: r.get("source_refs_json")?,
            revision: r.get("revision")?,
            idempotency_key: r.get("idempotency_key")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ClarificationRow {
    pub id: String,
    pub document_id: String,
    pub question_text: String,
    pub remark: String,
    pub source_type: Option<ClarificationSourceType>,
    pub source_id: Option<String>,
    pub source_snapshot_json: String,
    pub status: ClarificationStatus,
    pub resolution_ref_json: Option<String>,
    pub resolved_at: Option<i64>,
    pub revision: i64,
    pub idempotency_key: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub const CLARIFICATION_COLS: &str = "id, document_id, question_text, remark, source_type, \
     source_id, source_snapshot_json, status, resolution_ref_json, resolved_at, revision, \
     idempotency_key, created_at, updated_at";

impl<'r> TryFrom<&rusqlite::Row<'r>> for ClarificationRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_id: r.get("document_id")?,
            question_text: r.get("question_text")?,
            remark: r.get("remark")?,
            source_type: r
                .get::<_, Option<String>>("source_type")?
                .and_then(|s| ClarificationSourceType::parse_db(&s)),
            source_id: r.get("source_id")?,
            source_snapshot_json: r.get("source_snapshot_json")?,
            status: ClarificationStatus::parse_db(&r.get::<_, String>("status")?)
                .unwrap_or(ClarificationStatus::Pending),
            resolution_ref_json: r.get("resolution_ref_json")?,
            resolved_at: r.get("resolved_at")?,
            revision: r.get("revision")?,
            idempotency_key: r.get("idempotency_key")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }
}
