//! M02 命令层(薄封装,`desktop` feature)。
//! 路径只接受用户选择或拖放入口(E0002);文件 I/O 经 spawn_blocking,
//! 不阻塞 Tauri 异步运行时。

use serde::Deserialize;

use crate::shared::paging::PageRequest;
use crate::shared::AppError;

use super::models::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentImportRequest {
    pub path: String,
    #[serde(default)]
    pub source: ImportSource,
    pub idempotency_key: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    #[default]
    Picker,
    Drop,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentReconnectRequest {
    pub document_id: String,
    pub new_path: String,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentConfirmVersionRequest {
    pub document_id: String,
    pub version_id: String,
    pub idempotency_key: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn document_import(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DocumentImportRequest,
) -> Result<ImportResult, crate::shared::AppError> {
    // 首屏不等待全量哈希:建档(computing)立即返回,
    // 指纹后台流式计算,完成后经 document.changed 事件通知(《M02》§3、A8)。
    let svc = std::sync::Arc::clone(&state.documents);
    let path = std::path::PathBuf::from(&request.path);
    let key = request.idempotency_key;
    let (result, doc_id, ver_id) =
        tauri::async_runtime::spawn_blocking(move || svc.begin_import(&path, &key))
            .await
            .map_err(|e| crate::shared::AppError::internal(e.to_string()))??;

    // 默认全部允许:导入即授予文档级许可(generate/embed/vision)。
    // 网关校验、载荷哈希、来源白名单与审计不变;失败只告警,不影响导入。
    let grant_key = format!("import-grant:{}", result.document_id);
    if let Err(error) = state
        .model_privacy
        .grant_document_defaults(&result.document_id, &grant_key)
    {
        tracing::warn!(
            error_code = error.code,
            "document default grants failed (import continues)"
        );
    }

    if result.fingerprint_status == FingerprintStatus::Computing {
        let svc = std::sync::Arc::clone(&state.documents);
        tauri::async_runtime::spawn(async move {
            let _ = svc.finalize_import(&doc_id, &ver_id);
        });
    }
    Ok(result)
}

#[tauri::command]
pub async fn document_list_recent(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageRequest,
) -> Result<DocumentSummaryPage, crate::shared::AppError> {
    state.documents.list_recent(request)
}

#[tauri::command]
pub async fn document_get(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DocumentGetRequest,
) -> Result<DocumentDetail, crate::shared::AppError> {
    state.documents.get_document(&request.document_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentGetRequest {
    pub document_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentOpenRequest {
    pub document_id: String,
}

#[tauri::command]
pub async fn document_open(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DocumentOpenRequest,
) -> Result<DocumentOpenTokenDto, crate::shared::AppError> {
    // 默认全部允许:打开文档时确保文档级许可存在(幂等)。
    // 早期版本导入的文档没有许可,会让卡片/问答静默降级为“无授权”。
    let grant_key = format!("open-grant:{}", request.document_id);
    if let Err(error) = state
        .model_privacy
        .grant_document_defaults(&request.document_id, &grant_key)
    {
        tracing::warn!(
            error_code = error.code,
            "document default grants on open failed (open continues)"
        );
    }
    state.documents.open_document(&request.document_id)
}

#[tauri::command]
pub async fn document_check_file(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DocumentGetRequest,
) -> Result<FileCheckResult, crate::shared::AppError> {
    let svc = std::sync::Arc::clone(&state.documents);
    let id = request.document_id;
    tauri::async_runtime::spawn_blocking(move || svc.check_file(&id))
        .await
        .map_err(|e| crate::shared::AppError::internal(e.to_string()))?
}

#[tauri::command]
pub async fn document_reconnect(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DocumentReconnectRequest,
) -> Result<FileCheckResult, crate::shared::AppError> {
    let svc = std::sync::Arc::clone(&state.documents);
    let path = std::path::PathBuf::from(&request.new_path);
    let id = request.document_id;
    let key = request.idempotency_key;
    tauri::async_runtime::spawn_blocking(move || svc.reconnect(&id, &path, &key))
        .await
        .map_err(|e| crate::shared::AppError::internal(e.to_string()))?
}

#[tauri::command]
pub async fn document_confirm_version(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DocumentConfirmVersionRequest,
) -> Result<DocumentDetail, crate::shared::AppError> {
    state.documents.confirm_version(
        &request.document_id,
        &request.version_id,
        request.expected_revision,
    )
}

/// ts-rs 契约导出冒烟:命令层请求类型不进入生成目录(仅服务端校验用)。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_deserialize_camel_case() {
        let req: DocumentConfirmVersionRequest = serde_json::from_str(
            r#"{"documentId":"d","versionId":"v","idempotencyKey":"k","expectedRevision":3}"#,
        )
        .unwrap();
        assert_eq!(req.expected_revision, 3);
        assert_eq!(req.document_id, "d");
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentDeleteRequest {
    pub document_id: String,
}

#[tauri::command]
pub async fn document_delete(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DocumentDeleteRequest,
) -> Result<(), crate::shared::AppError> {
    let svc = std::sync::Arc::clone(&state.documents);
    let attachments = std::sync::Arc::clone(&state.attachments);
    let document_id = request.document_id;
    tauri::async_runtime::spawn_blocking(move || {
        // 先取 storage key:删除后记录已级联清除,无法再定位文件。
        let keys = attachments.storage_keys_for_document(&document_id)?;
        svc.delete_document(&document_id)?;
        if let Err(error) = attachments.remove_files(&keys) {
            // 数据库已删除,文件清理失败交给启动孤儿扫描兜底,不向用户伪造失败。
            tracing::warn!(
                error_code = error.code,
                "attachment cleanup after document delete failed"
            );
        }
        Ok(())
    })
    .await
    .map_err(|e| crate::shared::AppError::internal(e.to_string()))?
}

#[tauri::command]
pub async fn document_get_suggested_prompts(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DocumentSuggestedPromptsRequest,
) -> Result<DocumentSuggestedPromptsResponse, AppError> {
    state.documents.get_suggested_prompts(&request.document_id)
}
