//! 跨模块共享 DTO(M00,契约见《02》第 6 节)。
//! 具体业务对象(AnchorView、ParseStatusView 等)留在各模块目录。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum TaskType {
    ParseDocument,
    BuildEmbedding,
    AskQuestion,
    GenerateArtifact,
    /// P2 Agent 只读循环(深度研究);默认问答仍是 AskQuestion。
    AgentRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum TaskPhase {
    Waiting,
    Parsing,
    Indexing,
    Retrieving,
    Generating,
    Validating,
    Committing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum TaskOutcome {
    None,
    Answered,
    InsufficientEvidence,
    Conflict,
    OutOfScope,
    Partial,
}

/// 任务视图:任务状态的唯一真相投影(《02》§6)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct TaskView {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub document_id: Option<String>,
    pub task_type: TaskType,
    pub status: TaskStatus,
    pub phase: TaskPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub outcome: Option<TaskOutcome>,
    pub cancel_requested: bool,
    #[ts(type = "number")]
    pub state_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub result_ref: Option<TaskResultRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<AppError>,
    /// 任务结果消息所属会话(仅问答/研究任务;无消息归属时为 None)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub conversation_title: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct TaskResultRef {
    pub kind: TaskResultRefKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum TaskResultRefKind {
    Message,
    ArtifactVersion,
}

/// 阅读简报导出结果(单文件;只报告真实字节数与条目数)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ReadingBriefExportResult {
    #[ts(type = "number")]
    pub bytes_written: u64,
    pub format: String,
    #[ts(type = "number")]
    pub annotation_count: i64,
    #[ts(type = "number")]
    pub qa_count: i64,
}

/// 带乐观并发修订号的包装。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct Revisioned<T> {
    pub value: T,
    #[ts(type = "number")]
    pub revision: i64,
}

/// 短期访问令牌描述:不携带路径等敏感信息。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AccessTokenDescriptor {
    pub token: String,
    #[ts(type = "number")]
    pub expires_at: i64,
}

/// 二进制分块(M03 `reader_read_range` 与 M04 `parser_read_input` 共用)。
/// data 为 base64;offset/length 为字节区间,eof 表示已到达文件末尾。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct BinaryChunk {
    #[ts(type = "number")]
    pub offset: i64,
    #[ts(type = "number")]
    pub length: i64,
    pub data_base64: String,
    pub eof: bool,
}

/// base64 标准编码(无换行);仅用于 IPC 二进制传输。
pub fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct DeleteResult {
    pub deleted: bool,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelIdentifier {
    pub provider_id: String,
    pub model_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_view_serializes_camel_case() {
        let v = TaskView {
            id: "t1".into(),
            document_id: Some("d1".into()),
            task_type: TaskType::ParseDocument,
            status: TaskStatus::Running,
            phase: TaskPhase::Indexing,
            outcome: None,
            cancel_requested: false,
            state_version: 2,
            result_ref: None,
            error: None,
            conversation_id: None,
            conversation_title: None,
            created_at: 1,
            updated_at: 2,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["taskType"], "parse_document");
        assert_eq!(json["status"], "running");
        assert_eq!(json["stateVersion"], 2);
        assert!(json.get("outcome").is_none());
    }
}
