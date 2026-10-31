//! M07 命令层(薄封装,`desktop` feature,契约见《02》§3.2)。
//! 业务逻辑全部在服务层;命令只做请求反序列化与视图返回。

use serde::Deserialize;

use super::models::*;
use super::service::{NewAssistantMessage, NewUserMessage};
use crate::shared::{AppError, CoreErrorCode, PageRequest};

fn invalid(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
        .with_detail("reason", reason.into())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSuggestRequest {
    pub document_id: String,
    pub version_id: String,
    #[serde(default)]
    pub raw_selection: Option<crate::evidence::models::RawSelectionInput>,
    #[serde(default)]
    pub viewport_block_id: Option<String>,
    /// 当前页(PDF);用于生成“当前页 ±N”候选与页范围快照。
    #[serde(default)]
    pub page_number: Option<i64>,
    /// 附近页数(0/1/2;缺省 1)。
    #[serde(default)]
    pub nearby_pages: Option<i64>,
    /// 总页数(用于裁剪边界;缺省不裁剪)。
    #[serde(default)]
    pub page_count: Option<i64>,
}

#[tauri::command]
pub async fn context_suggest(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ContextSuggestRequest,
) -> Result<ContextSuggestion, AppError> {
    state.context.suggest_with_page(
        &request.document_id,
        &request.version_id,
        request.raw_selection.as_ref(),
        request.viewport_block_id.as_deref(),
        request.page_number.map(|page| PageViewport {
            page_number: page,
            nearby_pages: request.nearby_pages.unwrap_or(1).clamp(0, 5),
            page_count: request.page_count,
        }),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPreviewRequest {
    pub document_id: String,
    pub scope_draft: ScopeDraft,
}

#[tauri::command]
pub async fn context_preview(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ContextPreviewRequest,
) -> Result<ContextPreview, AppError> {
    state
        .context
        .preview(&request.document_id, &request.scope_draft)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextFreezeRequest {
    pub document_id: String,
    pub scope_draft: ScopeDraft,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn context_freeze(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ContextFreezeRequest,
) -> Result<ContextSnapshotView, AppError> {
    // 默认全部允许:真正开始用 AI 之前确保文档级许可存在(幂等)。
    // 早期导入的文档没有许可,会让问答/卡片以“未授权”失败。
    // 这里只补许可,不跳过网关的任何校验(载荷哈希/来源白名单/审计)。
    let grant_key = format!("freeze-grant:{}", request.document_id);
    if let Err(error) = state
        .model_privacy
        .grant_document_defaults(&request.document_id, &grant_key)
    {
        tracing::warn!(
            error_code = error.code,
            "default grants on freeze failed (freeze continues)"
        );
    }
    state.context.freeze(
        &request.document_id,
        &request.scope_draft,
        &request.idempotency_key,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextGetRequest {
    pub snapshot_id: String,
}

#[tauri::command]
pub async fn context_get(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ContextGetRequest,
) -> Result<ContextSnapshotView, AppError> {
    state
        .context
        .get_snapshot(&request.snapshot_id)?
        .ok_or_else(|| invalid("snapshot_not_found"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCreateRequest {
    pub document_id: String,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub title: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn conversation_create(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ConversationCreateRequest,
) -> Result<ConversationView, AppError> {
    state.context.conversation_create(
        &request.document_id,
        request.snapshot_id.as_deref(),
        &request.title,
        &request.idempotency_key,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListRequest {
    pub document_id: String,
    #[serde(default = "default_page_limit")]
    pub limit: i64,
    #[serde(default)]
    pub cursor: Option<String>,
}

fn default_page_limit() -> i64 {
    50
}

#[tauri::command]
pub async fn conversation_list(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ConversationListRequest,
) -> Result<ConversationPage, AppError> {
    let page = PageRequest {
        limit: request.limit,
        cursor: request.cursor,
    };
    state.context.conversation_list(&request.document_id, page)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessagesRequest {
    pub conversation_id: String,
    #[serde(default = "default_page_limit")]
    pub limit: i64,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[tauri::command]
pub async fn conversation_get_messages(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ConversationMessagesRequest,
) -> Result<MessagePage, AppError> {
    let page = PageRequest {
        limit: request.limit,
        cursor: request.cursor,
    };
    state
        .context
        .conversation_messages(&request.conversation_id, page)
}

// —— 消息级命令(会话面板直连;M09 编排经 tx 接口)——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageGetRequest {
    pub message_id: String,
}

/// 单条消息定位(任务中心「查看结果」):返回会话、文档与快照摘要。
#[tauri::command]
pub async fn message_get(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: MessageGetRequest,
) -> Result<MessageView, AppError> {
    state
        .context
        .message_view(&request.message_id)?
        .ok_or_else(|| AppError::new(CoreErrorCode::NotFound, crate::shared::new_trace_id()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageAppendRequest {
    pub conversation_id: String,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    pub text: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn message_append_user(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: MessageAppendRequest,
) -> Result<MessageView, AppError> {
    state.context.append_user_message(NewUserMessage {
        conversation_id: &request.conversation_id,
        snapshot_id: request.snapshot_id.as_deref(),
        task_id: request.task_id.as_deref(),
        text: &request.text,
        idempotency_key: &request.idempotency_key,
    })
}

#[tauri::command]
pub async fn message_append_assistant(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: MessageAppendRequest,
) -> Result<MessageView, AppError> {
    state
        .context
        .append_assistant_placeholder(NewAssistantMessage {
            conversation_id: &request.conversation_id,
            snapshot_id: request.snapshot_id.as_deref(),
            task_id: request.task_id.as_deref(),
            idempotency_key: &request.idempotency_key,
        })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationActiveSnapshotRequest {
    pub conversation_id: String,
    pub snapshot_id: String,
}

#[tauri::command]
pub async fn conversation_set_active_snapshot(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ConversationActiveSnapshotRequest,
) -> Result<ConversationView, AppError> {
    state
        .context
        .set_active_snapshot(&request.conversation_id, &request.snapshot_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationDeleteRequest {
    pub conversation_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPinRequest {
    pub conversation_id: String,
    pub pinned: bool,
}

#[tauri::command]
pub async fn conversation_pin(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ConversationPinRequest,
) -> Result<ConversationView, AppError> {
    state
        .context
        .set_conversation_pinned(&request.conversation_id, request.pinned)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationBranchRequest {
    pub conversation_id: String,
    #[serde(default)]
    pub up_to_message_id: Option<String>,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn conversation_branch(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ConversationBranchRequest,
) -> Result<ConversationView, AppError> {
    state.context.branch_conversation(
        &request.conversation_id,
        request.up_to_message_id.as_deref(),
        &request.idempotency_key,
    )
}

#[tauri::command]
pub async fn conversation_delete(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ConversationDeleteRequest,
) -> Result<crate::shared::dto::DeleteResult, AppError> {
    let context = std::sync::Arc::clone(&state.context);
    let attachments = std::sync::Arc::clone(&state.attachments);
    let conversation_id = request.conversation_id;
    tauri::async_runtime::spawn_blocking(move || {
        let keys = attachments.storage_keys_for_conversation(&conversation_id)?;
        let deleted = context.delete_conversation(&conversation_id)?;
        if let Err(error) = attachments.remove_files(&keys) {
            tracing::warn!(
                error_code = error.code,
                "attachment cleanup after conversation delete failed"
            );
        }
        Ok(deleted)
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationExportRequest {
    pub conversation_id: String,
    /// markdown / json
    pub format: String,
    /// 用户经保存对话框选择的显式目标路径;不进入导出内容或错误。
    pub path: String,
}

#[tauri::command]
pub async fn conversation_export(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ConversationExportRequest,
) -> Result<ConversationExportResult, AppError> {
    let format = crate::context::export::ConversationExportFormat::parse(&request.format)?;
    let mut data = state
        .context
        .conversation_export_data(&request.conversation_id)?;
    let attachments = state
        .attachments
        .list_for_conversation(&request.conversation_id)?;
    data.attachments = attachments
        .into_iter()
        .map(|attachment| crate::context::export::ExportAttachment {
            id: attachment.id.clone(),
            message_id: attachment.message_id.clone(),
            mime: attachment.mime.clone(),
            width: attachment.width,
            height: attachment.height,
            sha256: attachment.sha256.clone(),
            byte_size: attachment.byte_size,
            name: attachment.name.clone(),
            // 校验失败不阻断导出;data_url=None 会在正文/JSON 明确标注。
            data_url: state
                .attachments
                .thumbnail(&attachment.id)
                .ok()
                .map(|thumbnail| thumbnail.data_url),
        })
        .collect();
    let content = crate::context::export::render(&data, format)?;
    let bytes_written = crate::context::export::write_export_file(
        std::path::Path::new(&request.path),
        content.as_bytes(),
    )?;
    Ok(ConversationExportResult {
        conversation_id: request.conversation_id,
        format: format.as_str().to_string(),
        bytes_written,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRenameRequest {
    pub conversation_id: String,
    pub title: String,
}

#[tauri::command]
pub async fn conversation_rename(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ConversationRenameRequest,
) -> Result<ConversationView, AppError> {
    state
        .context
        .update_conversation_title(&request.conversation_id, &request.title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_deserialize_camel_case() {
        let req: ContextFreezeRequest = serde_json::from_str(
            r#"{"documentId":"d1","scopeDraft":{"kind":"pages","startPage":1,"endPage":3},
                "idempotencyKey":"k1"}"#,
        )
        .unwrap();
        assert_eq!(req.document_id, "d1");
        assert_eq!(req.idempotency_key, "k1");
        assert!(matches!(
            req.scope_draft,
            ScopeDraft::Pages {
                start_page: 1,
                end_page: 3
            }
        ));
    }

    #[test]
    fn conversation_export_request_deserializes_camel_case() {
        let req: ConversationExportRequest = serde_json::from_str(
            r#"{"conversationId":"c1","format":"json","path":"C:\\tmp\\会话.json"}"#,
        )
        .unwrap();
        assert_eq!(req.conversation_id, "c1");
        assert_eq!(req.format, "json");
        assert!(req.path.ends_with("会话.json"));
    }

    #[test]
    fn suggest_request_accepts_missing_optional_signals() {
        let req: ContextSuggestRequest =
            serde_json::from_str(r#"{"documentId":"d1","versionId":"v1"}"#).unwrap();
        assert!(req.raw_selection.is_none());
        assert!(req.viewport_block_id.is_none());
    }
}
