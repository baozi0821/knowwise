//! M03 阅读器命令层(薄封装,`desktop` feature)。
//! 范围读取经 spawn_blocking;位置保存只在命令成功返回后才算已保存。

use crate::shared::dto::BinaryChunk;

use super::reader_models::*;

#[tauri::command]
pub async fn reader_get_render_plan(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ReaderRenderPlanRequest,
) -> Result<RenderPlan, crate::shared::AppError> {
    state
        .reader
        .get_render_plan(&request.document_id, &request.version_id)
}

#[tauri::command]
pub async fn reader_read_range(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ReaderReadRangeRequest,
) -> Result<BinaryChunk, crate::shared::AppError> {
    let svc = std::sync::Arc::clone(&state.reader);
    let token = request.access_token;
    tauri::async_runtime::spawn_blocking(move || {
        svc.read_range(&token, request.offset, request.length)
    })
    .await
    .map_err(|e| crate::shared::AppError::internal(e.to_string()))?
}

#[tauri::command]
pub async fn reader_save_position(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ReaderSavePositionRequest,
) -> Result<ReadingPosition, crate::shared::AppError> {
    state
        .reader
        .save_position(&request.document_id, &request.version_id, &request.locator)
}

#[tauri::command]
pub async fn reader_get_position(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ReaderGetPositionRequest,
) -> Result<Option<ReadingPosition>, crate::shared::AppError> {
    state.reader.get_position(&request.document_id)
}

/// ts-rs 契约导出冒烟:请求类型 camelCase 反序列化(不进入生成目录)。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_deserialize_camel_case() {
        let req: ReaderReadRangeRequest =
            serde_json::from_str(r#"{"accessToken":"t","offset":0,"length":4096}"#).unwrap();
        assert_eq!(req.offset, 0);
        assert_eq!(req.length, 4096);
        let save: ReaderSavePositionRequest = serde_json::from_str(
            r#"{"documentId":"d","versionId":"v","idempotencyKey":"k",
                "locator":{"kind":"markdown","startOffset":0,"endOffset":5,
                "mdastNodeType":"paragraph","headingPath":[],"locatorVersion":1}}"#,
        )
        .unwrap();
        assert_eq!(save.document_id, "d");
        assert!(matches!(
            save.locator,
            crate::shared::locator::SourceLocator::Markdown { .. }
        ));
    }
}
