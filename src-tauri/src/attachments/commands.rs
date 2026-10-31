//! M15 命令层(薄封装,`desktop` feature)。
//!
//! 全部文件/数据库操作放到 `spawn_blocking`;错误只带稳定 reason,
//! 不回显本机绝对路径或图片内容。

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Deserialize;

use crate::app_shell::AppState;
use crate::shared::{AppError, CoreErrorCode};

fn invalid(reason: &'static str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
        .with_detail("reason", reason.into())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUploadRequest {
    pub document_id: String,
    /// 前端 canvas 压缩后的 data URL(仅 IPC,不落库)。
    pub data_url: String,
    #[serde(default)]
    pub width: Option<i64>,
    #[serde(default)]
    pub height: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
}

#[tauri::command]
pub async fn attachment_upload(
    state: tauri::State<'_, AppState>,
    request: AttachmentUploadRequest,
) -> Result<super::models::AttachmentView, AppError> {
    // 压缩后单张 ≤1.5MB;base64 膨胀后约 2MB,超过此值无需解码。
    if request.data_url.len() > 3_000_000 {
        return Err(invalid("attachment_too_large"));
    }
    let (_, encoded) = request
        .data_url
        .split_once(";base64,")
        .ok_or_else(|| invalid("attachment_data_url_invalid"))?;
    let bytes = BASE64
        .decode(encoded.trim())
        .map_err(|_| invalid("attachment_data_url_invalid"))?;
    let mime = super::infer_image_mime(&bytes).ok_or_else(|| invalid("attachment_not_image"))?;
    let service = std::sync::Arc::clone(&state.attachments);
    let document_id = request.document_id;
    let width = request.width;
    let height = request.height;
    let name = request.name;
    tauri::async_runtime::spawn_blocking(move || {
        service.upload(super::UploadAttachment {
            document_id: &document_id,
            mime,
            bytes: &bytes,
            width,
            height,
            name: name.as_deref(),
        })
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentLocalImportRequest {
    /// 由系统文件对话框返回;只用于本次读取,不写库。
    pub path: String,
}

#[tauri::command]
pub async fn attachment_import_local(
    _state: tauri::State<'_, AppState>,
    request: AttachmentLocalImportRequest,
) -> Result<super::models::LocalImageData, AppError> {
    tauri::async_runtime::spawn_blocking(move || super::read_local_image(&request.path))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentThumbnailRequest {
    pub attachment_id: String,
}

#[tauri::command]
pub async fn attachment_thumbnail(
    state: tauri::State<'_, AppState>,
    request: AttachmentThumbnailRequest,
) -> Result<super::models::AttachmentThumbnail, AppError> {
    let service = std::sync::Arc::clone(&state.attachments);
    tauri::async_runtime::spawn_blocking(move || service.thumbnail(&request.attachment_id))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentCloneRequest {
    pub attachment_id: String,
}

/// 图片记忆:复制历史附件为新的未绑定附件(用户显式选择时才调用)。
#[tauri::command]
pub async fn attachment_clone(
    state: tauri::State<'_, AppState>,
    request: AttachmentCloneRequest,
) -> Result<super::models::AttachmentView, AppError> {
    let service = std::sync::Arc::clone(&state.attachments);
    tauri::async_runtime::spawn_blocking(move || service.clone_unbound(&request.attachment_id))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDeleteRequest {
    pub attachment_id: String,
}
#[tauri::command]
pub async fn attachment_delete(
    state: tauri::State<'_, AppState>,
    request: AttachmentDeleteRequest,
) -> Result<bool, AppError> {
    let service = std::sync::Arc::clone(&state.attachments);
    tauri::async_runtime::spawn_blocking(move || service.delete_unbound(&request.attachment_id))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_request_deserializes_camel_case() {
        let request: AttachmentUploadRequest = serde_json::from_str(
            r#"{"documentId":"d1","dataUrl":"data:image/jpeg;base64,AAAA","width":10,"height":20}"#,
        )
        .unwrap();
        assert_eq!(request.document_id, "d1");
        assert_eq!(request.width, Some(10));
    }
}
