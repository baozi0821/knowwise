//! AppError 与错误码基线(M00,契约见《02》第 1、7 节)。
//! `details` 只允许非敏感结构化字段;正文、提示词、密钥、完整路径、
//! SQLite 原文与供应商原始异常一律不得进入 IPC。

use std::collections::HashMap;
use ts_rs::TS;

/// 稳定用户动作。前端按此驱动恢复入口,不解析 messageKey 之外的自由文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum UserAction {
    Retry,
    ChooseScope,
    ReconnectFile,
    OpenSettings,
    GrantPermission,
    WaitForIndex,
    ChooseOtherFile,
    None,
}

/// details 字段的受控值域:只有标量,杜绝嵌套正文。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, TS)]
#[serde(untagged)]
#[ts(export, export_to = "src/shared/generated/")]
pub enum DetailValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
}

impl From<i64> for DetailValue {
    fn from(v: i64) -> Self {
        DetailValue::Int(v)
    }
}
impl From<bool> for DetailValue {
    fn from(v: bool) -> Self {
        DetailValue::Bool(v)
    }
}
impl From<&str> for DetailValue {
    fn from(v: &str) -> Self {
        DetailValue::Str(v.to_string())
    }
}
impl From<String> for DetailValue {
    fn from(v: String) -> Self {
        DetailValue::Str(v)
    }
}

/// 统一 IPC 错误信封。`schemaVersion` 恒为 "error.v1"。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AppError {
    pub schema_version: &'static str,
    pub code: String,
    pub message_key: String,
    pub retryable: bool,
    pub user_action: UserAction,
    pub details: Option<HashMap<String, DetailValue>>,
    pub trace_id: String,
}

impl AppError {
    pub fn new(code: CoreErrorCode, trace_id: impl Into<String>) -> Self {
        Self {
            schema_version: "error.v1",
            code: code.code().to_string(),
            message_key: format!("error.{}", code.code().to_lowercase()),
            retryable: code.retryable(),
            user_action: code.user_action(),
            details: None,
            trace_id: trace_id.into(),
        }
    }

    /// 追加非敏感 detail(页码、状态、数量、版本号等)。
    pub fn with_detail(mut self, key: &str, value: DetailValue) -> Self {
        debug_assert!(
            !matches!(
                key,
                "path" | "prompt" | "text" | "content" | "secret" | "body" | "quote"
            ),
            "敏感字段禁止进入 AppError.details: {key}"
        );
        self.details
            .get_or_insert_with(HashMap::new)
            .insert(key.to_string(), value);
        self
    }

    /// 未知/未映射错误的兜底(仅用于防御;业务路径必须显式映射)。
    pub fn internal(trace_id: impl Into<String>) -> Self {
        Self::new(CoreErrorCode::Internal, trace_id)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {} (trace {})",
            self.code, self.message_key, self.trace_id
        )
    }
}

impl std::error::Error for AppError {}

/// M00–M05 范围内的核心错误码。错误码只增不改语义(《02》§7)。
/// M07+ 模块的错误码由其自有模块定义,不并入此枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreErrorCode {
    // 通用
    Internal,
    InvalidRequest,
    ConflictRevisionMismatch,
    NotFound,
    // M01
    TaskCanceled,
    TaskInterrupted,
    TaskNotRunning,
    DbCorrupt,
    DbMigrationFailed,
    // M02
    DocNotFound,
    DocFileMissing,
    DocUnsupportedFormat,
    DocEmpty,
    DocCorrupt,
    DocEncrypted,
    DocTooLarge,
    DocVersionUnconfirmed,
    FileAccessExpired,
    // M03/M04
    ReaderFormatUnsupportedRegion,
    ParseNoTextLayer,
    ParsePartial,
    ParseFailed,
    IndexFtsOnly,
    IndexGenerationUnavailable,
    // M05
    CitationTargetMissing,
    CitationQuoteMismatch,
    AnchorOrphaned,
    AnchorRelocationNotConfirmed,
    // M10(《02》§7:版本不可变/Schema 无效/导出目标三类错误)
    ArtifactVersionImmutable,
    ArtifactSchemaInvalid,
    ExportTargetExists,
    ExportPermissionDenied,
    ExportDiskFull,
    // M11(《M11》§9:无效题不计错;不可重试,无用户动作)
    QuizQuestionInvalid,
}

impl CoreErrorCode {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Internal => "INTERNAL",
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::ConflictRevisionMismatch => "CONFLICT_REVISION_MISMATCH",
            Self::NotFound => "NOT_FOUND",
            Self::TaskCanceled => "TASK_CANCELED",
            Self::TaskInterrupted => "TASK_INTERRUPTED",
            Self::TaskNotRunning => "TASK_NOT_RUNNING",
            Self::DbCorrupt => "DB_CORRUPT",
            Self::DbMigrationFailed => "DB_MIGRATION_FAILED",
            Self::DocNotFound => "DOC_NOT_FOUND",
            Self::DocFileMissing => "DOC_FILE_MISSING",
            Self::DocUnsupportedFormat => "DOC_UNSUPPORTED_FORMAT",
            Self::DocEmpty => "DOC_EMPTY",
            Self::DocCorrupt => "DOC_CORRUPT",
            Self::DocEncrypted => "DOC_ENCRYPTED",
            Self::DocTooLarge => "DOC_TOO_LARGE",
            Self::DocVersionUnconfirmed => "DOC_VERSION_UNCONFIRMED",
            Self::FileAccessExpired => "FILE_ACCESS_EXPIRED",
            Self::ReaderFormatUnsupportedRegion => "READER_FORMAT_UNSUPPORTED_REGION",
            Self::ParseNoTextLayer => "PARSE_NO_TEXT_LAYER",
            Self::ParsePartial => "PARSE_PARTIAL",
            Self::ParseFailed => "PARSE_FAILED",
            Self::IndexFtsOnly => "INDEX_FTS_ONLY",
            Self::IndexGenerationUnavailable => "INDEX_GENERATION_UNAVAILABLE",
            Self::CitationTargetMissing => "CITATION_TARGET_MISSING",
            Self::CitationQuoteMismatch => "CITATION_QUOTE_MISMATCH",
            Self::AnchorOrphaned => "ANCHOR_ORPHANED",
            Self::AnchorRelocationNotConfirmed => "ANCHOR_RELOCATION_NOT_CONFIRMED",
            Self::ArtifactVersionImmutable => "ARTIFACT_VERSION_IMMUTABLE",
            Self::ArtifactSchemaInvalid => "ARTIFACT_SCHEMA_INVALID",
            Self::ExportTargetExists => "EXPORT_TARGET_EXISTS",
            Self::ExportPermissionDenied => "EXPORT_PERMISSION_DENIED",
            Self::ExportDiskFull => "EXPORT_DISK_FULL",
            Self::QuizQuestionInvalid => "QUIZ_QUESTION_INVALID",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Internal
                | Self::FileAccessExpired
                | Self::ParseFailed
                | Self::IndexGenerationUnavailable
                | Self::ConflictRevisionMismatch
                | Self::TaskCanceled
                | Self::TaskInterrupted
                | Self::ArtifactSchemaInvalid
        )
    }

    pub fn user_action(&self) -> UserAction {
        match self {
            Self::DocNotFound | Self::NotFound => UserAction::None,
            Self::DocFileMissing => UserAction::ReconnectFile,
            Self::DocUnsupportedFormat | Self::DocEmpty | Self::DocCorrupt | Self::DocEncrypted => {
                UserAction::ChooseOtherFile
            }
            Self::DocTooLarge => UserAction::OpenSettings,
            Self::FileAccessExpired
            | Self::ParseFailed
            | Self::TaskCanceled
            | Self::TaskInterrupted
            | Self::ConflictRevisionMismatch
            | Self::Internal => UserAction::Retry,
            Self::DocVersionUnconfirmed
            | Self::TaskNotRunning
            | Self::ReaderFormatUnsupportedRegion
            | Self::ParseNoTextLayer
            | Self::ParsePartial
            | Self::AnchorOrphaned
            | Self::AnchorRelocationNotConfirmed
            | Self::CitationTargetMissing
            | Self::CitationQuoteMismatch => UserAction::None,
            Self::DbCorrupt | Self::DbMigrationFailed | Self::InvalidRequest => UserAction::None,
            Self::IndexFtsOnly => UserAction::OpenSettings,
            Self::IndexGenerationUnavailable => UserAction::WaitForIndex,
            Self::ArtifactSchemaInvalid => UserAction::Retry,
            Self::ArtifactVersionImmutable | Self::ExportPermissionDenied => UserAction::None,
            Self::ExportTargetExists => UserAction::ChooseOtherFile,
            Self::ExportDiskFull => UserAction::Retry,
            Self::QuizQuestionInvalid => UserAction::None,
        }
    }
}

/// 生成短随机 traceId(诊断关联用,不含任何内容)。
pub fn new_trace_id() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_error_shape_matches_contract() {
        let e = AppError::new(CoreErrorCode::DocFileMissing, "trace-1")
            .with_detail("documentId", "d1".into());
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["schemaVersion"], "error.v1");
        assert_eq!(json["code"], "DOC_FILE_MISSING");
        assert_eq!(json["userAction"], "reconnect_file");
        assert_eq!(json["retryable"], false);
        assert_eq!(json["details"]["documentId"], "d1");
    }

    #[test]
    fn codes_are_stable_snake_upper() {
        assert_eq!(
            CoreErrorCode::DocUnsupportedFormat.code(),
            "DOC_UNSUPPORTED_FORMAT"
        );
        assert_eq!(
            CoreErrorCode::AnchorRelocationNotConfirmed.code(),
            "ANCHOR_RELOCATION_NOT_CONFIRMED"
        );
    }
}
