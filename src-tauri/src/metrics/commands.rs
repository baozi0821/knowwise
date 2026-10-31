//! 运行指标读取命令(desktop feature)。

use serde::Deserialize;

use crate::app_shell::AppState;
use crate::shared::AppError;

use super::models::RunMetricsView;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsRecentRequest {
    #[serde(default)]
    pub limit: Option<i64>,
}

#[tauri::command]
pub async fn metrics_recent(
    state: tauri::State<'_, AppState>,
    request: MetricsRecentRequest,
) -> Result<Vec<RunMetricsView>, AppError> {
    state.metrics.recent(request.limit.unwrap_or(20))
}
