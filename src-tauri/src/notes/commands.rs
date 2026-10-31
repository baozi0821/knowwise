//! M06 命令层(薄封装,`desktop` feature,契约见《02》§3.2)。
//! `clarification_delete`/`bookmark_list` 是 M06 分册 §6/§8 要求的补充命令。

use serde::Deserialize;

use super::models::*;
use crate::shared::dto::DeleteResult;
use crate::shared::locator::SourceLocator;
use crate::shared::paging::PageRequest;
use crate::shared::AppError;

// —— 标注 ——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationCreateRequest {
    #[serde(flatten)]
    pub input: AnnotationCreateInput,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn annotation_create(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AnnotationCreateRequest,
) -> Result<AnnotationView, AppError> {
    state
        .notes
        .create_annotation(&request.input, &request.idempotency_key)
}

#[tauri::command]
pub async fn annotation_update(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AnnotationUpdateInput,
) -> Result<AnnotationView, AppError> {
    state.notes.update_annotation(&request)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationDeleteRequest {
    pub annotation_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn annotation_delete(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AnnotationDeleteRequest,
) -> Result<DeleteResult, AppError> {
    state
        .notes
        .delete_annotation(&request.annotation_id, request.expected_revision)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationListRequest {
    pub document_id: String,
}

#[tauri::command]
pub async fn annotation_list(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AnnotationListRequest,
) -> Result<Vec<AnnotationView>, AppError> {
    state.notes.list_annotations(&request.document_id)
}

// —— 书签 ——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkToggleRequest {
    pub document_id: String,
    #[serde(default)]
    pub anchor_id: Option<String>,
    pub locator: SourceLocator,
    #[serde(default)]
    pub label: String,
    /// 期望状态:避免双击创建两个相反结果。
    pub expected_state: bool,
}

#[tauri::command]
pub async fn bookmark_toggle(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: BookmarkToggleRequest,
) -> Result<BookmarkResult, AppError> {
    state.notes.toggle_bookmark(
        &request.document_id,
        request.anchor_id.as_deref(),
        &request.locator,
        &request.label,
        request.expected_state,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkListRequest {
    pub document_id: String,
}

#[tauri::command]
pub async fn bookmark_list(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: BookmarkListRequest,
) -> Result<Vec<BookmarkView>, AppError> {
    state.notes.list_bookmarks(&request.document_id)
}

// —— 笔记 ——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkRenameRequest {
    pub document_id: String,
    pub bookmark_id: String,
    pub label: String,
    /// 备注(可选;None=只改名称,保留原备注)。
    #[serde(default)]
    pub note: Option<String>,
}

#[tauri::command]
pub async fn bookmark_rename(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: BookmarkRenameRequest,
) -> Result<BookmarkView, AppError> {
    state.notes.rename_bookmark(
        &request.document_id,
        &request.bookmark_id,
        &request.label,
        request.note.as_deref(),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSaveRequest {
    #[serde(flatten)]
    pub input: NoteSaveInput,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn note_save(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: NoteSaveRequest,
) -> Result<NoteView, AppError> {
    state
        .notes
        .save_note(&request.input, &request.idempotency_key)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteListRequest {
    pub document_id: String,
    #[serde(default)]
    pub filter: NoteListFilter,
    #[serde(default = "default_page")]
    pub page: PageRequest,
}

fn default_page() -> PageRequest {
    PageRequest {
        limit: 50,
        cursor: None,
    }
}

#[tauri::command]
pub async fn note_list(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: NoteListRequest,
) -> Result<NotePage, AppError> {
    state
        .notes
        .list_notes(&request.document_id, &request.filter, request.page)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteGetRequest {
    pub note_id: String,
}

#[tauri::command]
pub async fn note_get(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: NoteGetRequest,
) -> Result<NoteView, AppError> {
    state.notes.get_note(&request.note_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteDeleteRequest {
    pub note_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn note_delete(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: NoteDeleteRequest,
) -> Result<DeleteResult, AppError> {
    state
        .notes
        .delete_note(&request.note_id, request.expected_revision)
}

// —— 待澄清 ——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClarificationCreateRequest {
    #[serde(flatten)]
    pub input: ClarificationCreateInput,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn clarification_create(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ClarificationCreateRequest,
) -> Result<ClarificationView, AppError> {
    state
        .notes
        .create_clarification(&request.input, &request.idempotency_key)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClarificationListRequest {
    pub document_id: String,
    #[serde(default)]
    pub status: Option<ClarificationStatus>,
    #[serde(default)]
    pub source_type: Option<ClarificationSourceType>,
    #[serde(default = "default_page")]
    pub page: PageRequest,
}

#[tauri::command]
pub async fn clarification_list(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ClarificationListRequest,
) -> Result<ClarificationPage, AppError> {
    state.notes.list_clarifications(
        &request.document_id,
        request.status,
        request.source_type,
        request.page,
    )
}

#[tauri::command]
pub async fn clarification_update(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ClarificationUpdateInput,
) -> Result<ClarificationView, AppError> {
    state.notes.update_clarification(&request)
}

#[tauri::command]
pub async fn clarification_set_remark(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ClarificationRemarkInput,
) -> Result<ClarificationView, AppError> {
    state.notes.set_clarification_remark(&request)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClarificationDeleteRequest {
    pub clarification_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn clarification_delete(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ClarificationDeleteRequest,
) -> Result<DeleteResult, AppError> {
    state
        .notes
        .delete_clarification(&request.clarification_id, request.expected_revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_deserialize_camel_case() {
        let req: NoteDeleteRequest =
            serde_json::from_str(r#"{"noteId":"n1","expectedRevision":2}"#).unwrap();
        assert_eq!(req.note_id, "n1");
        assert_eq!(req.expected_revision, 2);
    }
}
