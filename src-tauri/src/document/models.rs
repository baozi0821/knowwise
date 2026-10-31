//! M02 领域对象与对外 DTO。
//! 契约:路径不作为身份;details 不携带完整路径与正文(API-06)。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::paging::Page;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum DocumentFormat {
    Pdf,
    Markdown,
    Docx,
}

impl DocumentFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Markdown => "markdown",
            Self::Docx => "docx",
        }
    }

    /// 解析数据库存储形态;未知值返回 None(不静默回退)。
    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "pdf" => Self::Pdf,
            "markdown" => Self::Markdown,
            "docx" => Self::Docx,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum FingerprintStatus {
    Computing,
    Ready,
    Unreadable,
}

/// 文件当前位置状态(派生,不落库;由 check_file/stat 计算)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum FileState {
    Present,
    Missing,
    /// 同一路径出现新内容,已创建待确认候选版本。
    ChangedPendingConfirm,
    Unreadable,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct DocumentSummary {
    pub id: String,
    pub display_name: String,
    pub format: DocumentFormat,
    pub current_version_id: Option<String>,
    pub revision: i64,
    pub file_state: FileState,
    /// 当前版本指纹状态;无版本时为 None。
    pub fingerprint_status: Option<FingerprintStatus>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_opened_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct VersionSummary {
    pub id: String,
    pub document_id: String,
    pub supersedes_version_id: Option<String>,
    pub content_hash: Option<String>,
    pub file_size: Option<i64>,
    pub fingerprint_status: FingerprintStatus,
    pub confirmed: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct DocumentDetail {
    #[serde(flatten)]
    #[ts(flatten)]
    pub summary: DocumentSummary,
    pub versions: Vec<VersionSummary>,
}

/// 导入结果的形态:新文档 / 重连到已有版本 / 其他文档的重复副本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ImportKind {
    /// 新文档,首个版本已就绪(或指纹计算中)。
    NewDocument,
    /// 同一路径同内容重复导入:连接到已有文档与版本,未产生新解析对象。
    ConnectedExisting,
    /// 不同路径出现相同内容:默认建议连接,由用户决定是否合并。
    DuplicateOfOtherDocument,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ImportResult {
    pub document_id: String,
    pub version_id: String,
    pub import_kind: ImportKind,
    /// 与 DuplicateOfOtherDocument 搭配:内容相同的原文档。
    pub duplicate_of_document_id: Option<String>,
    pub fingerprint_status: FingerprintStatus,
    pub access_token: Option<crate::shared::dto::AccessTokenDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum FileCheckStatus {
    Unchanged,
    Missing,
    ChangedPendingConfirm,
    Unreadable,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct FileCheckResult {
    pub document_id: String,
    pub status: FileCheckStatus,
    pub current_version_id: Option<String>,
    /// 变化时新建的待确认候选版本。
    pub candidate_version_id: Option<String>,
}

/// 打开令牌 DTO:不含路径;路径只存在于服务端令牌绑定中。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct DocumentOpenTokenDto {
    pub token: String,
    pub expires_at: i64,
    pub document_id: String,
    pub version_id: String,
    pub file_size: i64,
    pub format: DocumentFormat,
}

pub type DocumentSummaryPage = Page<DocumentSummary>;

/// 数据库行结构(内部)。
#[derive(Debug, Clone)]
pub struct DocumentRow {
    pub id: String,
    pub display_name: String,
    pub format: DocumentFormat,
    pub current_version_id: Option<String>,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_opened_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct VersionRow {
    pub id: String,
    pub document_id: String,
    pub supersedes_version_id: Option<String>,
    pub content_hash: Option<String>,
    pub file_size: Option<i64>,
    pub fingerprint_status: FingerprintStatus,
    pub confirmed: bool,
    pub created_at: i64,
}

impl VersionRow {
    pub fn to_summary(&self) -> VersionSummary {
        VersionSummary {
            id: self.id.clone(),
            document_id: self.document_id.clone(),
            supersedes_version_id: self.supersedes_version_id.clone(),
            content_hash: self.content_hash.clone(),
            file_size: self.file_size,
            fingerprint_status: self.fingerprint_status,
            confirmed: self.confirmed,
            created_at: self.created_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileLocationRow {
    pub id: String,
    pub document_id: String,
    pub observed_version_id: Option<String>,
    pub path: String,
    pub is_primary: bool,
    pub last_seen_size: Option<i64>,
    pub last_seen_mtime_ns: Option<i64>,
    pub last_verified_at: Option<i64>,
}

fn fp_from_str(s: &str) -> Option<FingerprintStatus> {
    Some(match s {
        "computing" => FingerprintStatus::Computing,
        "ready" => FingerprintStatus::Ready,
        "unreadable" => FingerprintStatus::Unreadable,
        _ => return None,
    })
}

impl FingerprintStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Computing => "computing",
            Self::Ready => "ready",
            Self::Unreadable => "unreadable",
        }
    }
    /// 解析数据库存储形态;命名避开与 `FromStr` 混淆。
    pub fn parse_db(s: &str) -> Option<Self> {
        fp_from_str(s)
    }
}

impl<'r> TryFrom<&rusqlite::Row<'r>> for DocumentRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            display_name: r.get("display_name")?,
            format: match r.get::<_, String>("format")?.as_str() {
                "pdf" => DocumentFormat::Pdf,
                "docx" => DocumentFormat::Docx,
                _ => DocumentFormat::Markdown,
            },
            current_version_id: r.get("current_version_id")?,
            revision: r.get("revision")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
            last_opened_at: r.get("last_opened_at")?,
        })
    }
}

impl<'r> TryFrom<&rusqlite::Row<'r>> for VersionRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_id: r.get("document_id")?,
            supersedes_version_id: r.get("supersedes_version_id")?,
            content_hash: r.get("content_hash")?,
            file_size: r.get("file_size")?,
            fingerprint_status: fp_from_str(&r.get::<_, String>("fingerprint_status")?)
                .unwrap_or(FingerprintStatus::Unreadable),
            confirmed: r.get::<_, i64>("confirmed")? != 0,
            created_at: r.get("created_at")?,
        })
    }
}

impl<'r> TryFrom<&rusqlite::Row<'r>> for FileLocationRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_id: r.get("document_id")?,
            observed_version_id: r.get("observed_version_id")?,
            path: r.get("path")?,
            is_primary: r.get::<_, i64>("is_primary")? != 0,
            last_seen_size: r.get("last_seen_size")?,
            last_seen_mtime_ns: r.get("last_seen_mtime_ns")?,
            last_verified_at: r.get("last_verified_at")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedPromptItem {
    pub id: String,
    pub icon: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSuggestedPromptsResponse {
    pub domain_label: Option<String>,
    pub items: Vec<SuggestedPromptItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSuggestedPromptsRequest {
    pub document_id: String,
}
