//! M09 命令层(薄封装,`desktop` feature,契约见《02》§3.3)。
//!
//! `ai_ask` / `ai_get_task` / `ai_cancel` / `ai_retry`。业务逻辑在服务层;
//! 取消转发到 TaskService 令牌,由编排传播到检索与模型流。

use serde::Deserialize;

use super::service::AskRequest;
use crate::shared::dto::TaskView;
use crate::shared::error::AppError;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAskRequest {
    pub conversation_id: String,
    pub snapshot_id: String,
    /// 模型输入:纯文本问题,**不含**图片 base64。
    pub question: String,
    /// 展示文本(纯文本)。服务端会剥离其中任何图片 Markdown,再按已校验的
    /// `attachmentIds` 追加 `attachment:<id>` 引用;模型仍只读 `question`。
    /// 缺省回退 `question`,兼容未升级的前端。
    #[serde(default)]
    pub display_text: Option<String>,
    /// 已上传并压缩的图片附件 ID(最多 3 个)。服务端校验归属后绑定到用户消息。
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    #[serde(default = "default_locale")]
    pub locale: String,
    pub idempotency_key: String,
}

fn default_locale() -> String {
    "zh-CN".to_string()
}

/// 服务级直连入口(不经 M07 消息编排;注册表使用 app_shell::ask_flow::ai_ask)。
#[tauri::command]
pub async fn ai_ask_direct(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AiAskRequest,
) -> Result<TaskView, AppError> {
    if !request.attachment_ids.is_empty() {
        return Err(AppError::new(
            crate::shared::CoreErrorCode::InvalidRequest,
            crate::shared::new_trace_id(),
        )
        .with_detail("reason", "direct_attachments_not_supported".into()));
    }
    let document_id = state
        .retrieval
        .snapshot_document_id(&request.snapshot_id)?
        .ok_or_else(|| {
            AppError::new(
                crate::shared::CoreErrorCode::NotFound,
                crate::shared::new_trace_id(),
            )
        })?;
    let task = state.retrieval.ask(AskRequest {
        document_id,
        conversation_id: request.conversation_id,
        assistant_message_id: None,
        snapshot_id: request.snapshot_id,
        question: request.question,
        locale: request.locale,
        idempotency_key: request.idempotency_key,
        history: Vec::new(),
        attachments: Vec::new(),
    })?;
    Ok(task.to_view())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTaskRequest {
    pub task_id: String,
}

#[tauri::command]
pub async fn ai_get_task(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AiTaskRequest,
) -> Result<TaskView, AppError> {
    let task = state.tasks.get_task(&request.task_id)?.ok_or_else(|| {
        AppError::new(
            crate::shared::CoreErrorCode::NotFound,
            crate::shared::new_trace_id(),
        )
    })?;
    Ok(task.to_view())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCancelRequest {
    pub task_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn ai_cancel(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AiCancelRequest,
) -> Result<TaskView, AppError> {
    let task = state
        .tasks
        .request_cancel(&request.task_id, request.expected_revision)?;
    Ok(task.to_view())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiRetryRequest {
    pub task_id: String,
    /// 原问题(前端从同对话上一轮用户消息提取;截图 Markdown 已剥离)。
    /// 缺省时服务层拒绝重试,避免用空问题重新计费。
    #[serde(default)]
    pub question: Option<String>,
    pub idempotency_key: String,
}

#[tauri::command]
pub fn ai_retry(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AiRetryRequest,
) -> Result<TaskView, AppError> {
    let _ = request.idempotency_key; // 重试复用 Task,幂等键不新建任务
    let task = state.retrieval.retry(&request.task_id, request.question)?;
    Ok(task.to_view())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_deserialize_camel_case() {
        let req: AiAskRequest = serde_json::from_str(
            r#"{"conversationId":"c1","snapshotId":"s1","question":"q?",
               "locale":"zh-CN","idempotencyKey":"k1"}"#,
        )
        .unwrap();
        assert_eq!(req.snapshot_id, "s1");
        assert_eq!(req.idempotency_key, "k1");
        // 旧客户端不带 displayText/attachmentIds:安全缺省。
        assert!(req.display_text.is_none());
        assert!(req.attachment_ids.is_empty());
    }

    #[test]
    fn ask_request_accepts_attachment_ids_but_not_base64_contract() {
        let req: AiAskRequest = serde_json::from_str(
            r#"{"conversationId":"c1","snapshotId":"s1","question":"预算怎么算?",
               "displayText":"预算怎么算?",
               "attachmentIds":["att-1","att-2"],
               "idempotencyKey":"k1"}"#,
        )
        .unwrap();
        assert_eq!(req.question, "预算怎么算?");
        assert_eq!(req.display_text.as_deref(), Some("预算怎么算?"));
        assert_eq!(
            req.attachment_ids,
            vec!["att-1".to_string(), "att-2".to_string()]
        );
    }
}
