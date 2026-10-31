//! M12 命令层(薄封装,`desktop` feature)。业务逻辑在服务层;
//! Artifact 身份与版本保存由 M10 执行,M12 命令只做内容级操作
//! 与导出(《02》§3 `graph_export`)。

use serde::Deserialize;

use super::models::*;
use super::service::GraphExportRequest;
use crate::shared::AppError;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphValidateRequest {
    pub content: GraphContent,
}

#[tauri::command]
pub async fn graph_validate(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: GraphValidateRequest,
) -> Result<GraphValidationReport, AppError> {
    Ok(state.graphs.validate(&request.content))
}

#[tauri::command]
pub async fn graph_layout(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: GraphValidateRequest,
) -> Result<GraphLayoutOutcome, AppError> {
    Ok(state.graphs.layout(&request.content))
}

#[tauri::command]
pub async fn graph_outline(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: GraphValidateRequest,
) -> Result<GraphOutline, AppError> {
    state.graphs.outline(&request.content)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphExportCommandRequest {
    pub artifact_id: String,
    pub version_id: String,
    /// svg | outline(PNG 由前端 Canvas 转换,不经本命令)。
    pub format: String,
    pub path: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn graph_export(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: GraphExportCommandRequest,
) -> Result<GraphExportResult, AppError> {
    let format = GraphExportFormat::parse(&request.format).ok_or_else(|| {
        AppError::new(
            crate::shared::CoreErrorCode::InvalidRequest,
            crate::shared::new_trace_id(),
        )
        .with_detail("reason", "invalid_format".into())
    })?;
    state.graphs.export(&GraphExportRequest {
        artifact_id: request.artifact_id,
        version_id: request.version_id,
        format,
        path: request.path.into(),
        idempotency_key: request.idempotency_key,
    })
}
