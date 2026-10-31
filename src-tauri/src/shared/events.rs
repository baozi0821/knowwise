//! 事件信封(M00,契约见《02》第 4 节)。
//! 事件只在数据库 COMMIT 成功后发出;事件是通知,不是事实来源。

use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, TS)]
#[ts(export, export_to = "src/shared/generated/")]
pub enum DomainEventType {
    /// 事件类型名使用契约的点分形式(《02》§4),非 snake_case。
    #[serde(rename = "document.changed")]
    DocumentChanged,
    #[serde(rename = "parse.progress")]
    ParseProgress,
    /// 2D 页级 OCR:页状态 COMMIT 后通知,载荷不包含页图/正文。
    #[serde(rename = "ocr.progress")]
    OcrProgress,
    #[serde(rename = "task.changed")]
    TaskChanged,
    #[serde(rename = "ai.delta")]
    AiDelta,
    #[serde(rename = "conversation.changed")]
    ConversationChanged,
    #[serde(rename = "userdata.changed")]
    UserdataChanged,
    #[serde(rename = "artifact.changed")]
    ArtifactChanged,
    #[serde(rename = "privacy.changed")]
    PrivacyChanged,
    #[serde(rename = "model.changed")]
    ModelChanged,
    #[serde(rename = "parser.request")]
    ParserRequest,
    /// P3A:写工具等待用户确认(通知,不是事实来源)。
    #[serde(rename = "agent.confirmation")]
    AgentConfirmation,
}

impl DomainEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DocumentChanged => "document.changed",
            Self::ParseProgress => "parse.progress",
            Self::OcrProgress => "ocr.progress",
            Self::TaskChanged => "task.changed",
            Self::AiDelta => "ai.delta",
            Self::ConversationChanged => "conversation.changed",
            Self::UserdataChanged => "userdata.changed",
            Self::ArtifactChanged => "artifact.changed",
            Self::PrivacyChanged => "privacy.changed",
            Self::ModelChanged => "model.changed",
            Self::ParserRequest => "parser.request",
            Self::AgentConfirmation => "agent.confirmation",
        }
    }
}

/// 事件信封。`payload` 在 TS 侧为 unknown,具体载荷由消费模块用其自有 Schema 解析。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct DomainEvent {
    pub schema_version: &'static str,
    pub event_id: String,
    #[serde(rename = "type")]
    pub event_type: DomainEventType,
    #[ts(type = "number")]
    pub occurred_at: i64,
    pub aggregate_id: String,
    #[ts(type = "number")]
    pub aggregate_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
}

impl DomainEvent {
    pub fn new(
        event_type: DomainEventType,
        aggregate_id: impl Into<String>,
        aggregate_version: i64,
        occurred_at: i64,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            schema_version: "event.v1",
            event_id: crate::shared::new_trace_id(),
            event_type,
            occurred_at,
            aggregate_id: aggregate_id.into(),
            aggregate_version,
            document_id: None,
            document_version_id: None,
            task_id: None,
            payload,
        }
    }

    pub fn with_document(
        mut self,
        document_id: Option<String>,
        version_id: Option<String>,
    ) -> Self {
        self.document_id = document_id;
        self.document_version_id = version_id;
        self
    }

    pub fn with_task(mut self, task_id: Option<String>) -> Self {
        self.task_id = task_id;
        self
    }
}

/// 事件出口。业务服务依赖此 trait;Tauri 实现只在 COMMIT 后转发到 WebView。
pub trait EventSink: Send + Sync {
    fn emit(&self, event: DomainEvent);
}

/// 空事件出口(测试与无窗口场景)。
pub struct NoopEventSink;

impl EventSink for NoopEventSink {
    fn emit(&self, _event: DomainEvent) {}
}

/// 收集事件用于断言(仅测试)。
#[derive(Default)]
pub struct CollectingEventSink {
    pub events: std::sync::Mutex<Vec<DomainEvent>>,
}

impl EventSink for CollectingEventSink {
    fn emit(&self, event: DomainEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_serializes_camel_case_with_type() {
        let e = DomainEvent::new(
            DomainEventType::TaskChanged,
            "task-1",
            3,
            1000,
            serde_json::json!({"status": "succeeded"}),
        )
        .with_document(Some("doc-1".into()), None);
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["schemaVersion"], "event.v1");
        assert_eq!(json["type"], "task.changed");
        assert_eq!(json["aggregateVersion"], 3);
        assert_eq!(json["documentId"], "doc-1");
        assert!(json.get("taskId").is_none(), "可选字段缺省时省略");
    }

    #[test]
    fn event_names_match_contract() {
        assert_eq!(DomainEventType::ParserRequest.as_str(), "parser.request");
        assert_eq!(DomainEventType::AiDelta.as_str(), "ai.delta");
    }
}
