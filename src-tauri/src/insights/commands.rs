//! 动态卡片/文档画像命令(desktop feature)。

use serde::Deserialize;

use crate::app_shell::AppState;
use crate::shared::AppError;

use super::models::{DocumentProfileView, InsightCardV1, InsightCardsRequest};

#[tauri::command]
pub fn insight_cards_get(
    state: tauri::State<'_, AppState>,
    request: InsightCardsRequest,
) -> Result<Vec<InsightCardV1>, AppError> {
    state.cards.get_cards(&request, false)
}

#[tauri::command]
pub fn insight_cards_refresh(
    state: tauri::State<'_, AppState>,
    request: InsightCardsRequest,
) -> Result<Vec<InsightCardV1>, AppError> {
    state.cards.get_cards(&request, true)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentProfileRequest {
    pub document_id: String,
    pub version_id: String,
}

#[tauri::command]
pub fn document_profile_get(
    state: tauri::State<'_, AppState>,
    request: DocumentProfileRequest,
) -> Result<DocumentProfileView, AppError> {
    state
        .cards
        .get_profile(&request.document_id, &request.version_id)
}
