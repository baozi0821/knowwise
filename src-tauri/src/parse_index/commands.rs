//! M04 命令层(薄封装,`desktop` feature)。
//!
//! `parser_*` 是解析桥内部命令,仅由 M04 的 Worker 调用(《02》§5);
//! 文件 I/O 经 spawn_blocking,不阻塞 Tauri 异步运行时。

use serde::Deserialize;

use crate::shared::dto::TaskView;

use super::models::*;
use super::service::EmbeddingInput;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseStartRequest {
    pub document_id: String,
    pub version_id: String,
    #[serde(default)]
    pub reason: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn parse_start(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ParseStartRequest,
) -> Result<TaskView, crate::shared::AppError> {
    let task = state.parse.start_parse(
        &request.document_id,
        &request.version_id,
        &request.reason,
        &request.idempotency_key,
    )?;
    Ok(task.to_view())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseStatusRequest {
    pub document_id: String,
}

#[tauri::command]
pub async fn parse_get_status(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ParseStatusRequest,
) -> Result<ParseStatusView, crate::shared::AppError> {
    state.parse.get_status(&request.document_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseCancelRequest {
    pub task_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn parse_cancel(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ParseCancelRequest,
) -> Result<TaskView, crate::shared::AppError> {
    let task = state
        .parse
        .cancel_parse(&request.task_id, request.expected_revision)?;
    Ok(task.to_view())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseRetryRequest {
    pub task_id: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn parse_retry(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ParseRetryRequest,
) -> Result<TaskView, crate::shared::AppError> {
    let _ = request.idempotency_key; // 重试幂等性由 Task 状态机承载
    let task = state.parse.retry_parse(&request.task_id)?;
    Ok(task.to_view())
}

// —— 解析桥内部命令(仅 Worker 调用)——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParserReadInputRequest {
    pub parse_job_token: String,
    pub offset: i64,
    pub length: i64,
}

#[tauri::command]
pub async fn parser_read_input(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ParserReadInputRequest,
) -> Result<crate::shared::dto::BinaryChunk, crate::shared::AppError> {
    let svc = std::sync::Arc::clone(&state.parse);
    let token = request.parse_job_token;
    tauri::async_runtime::spawn_blocking(move || {
        svc.read_input(&token, request.offset, request.length)
    })
    .await
    .map_err(|e| crate::shared::AppError::internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParserSubmitBatchRequest {
    pub parse_job_token: String,
    pub batch: ParserBatchInput,
}

#[tauri::command]
pub async fn parser_submit_batch(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ParserSubmitBatchRequest,
) -> Result<super::service::BatchAck, crate::shared::AppError> {
    state
        .parse
        .submit_batch(&request.parse_job_token, request.batch)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParserFinishRequest {
    pub parse_job_token: String,
}

#[tauri::command]
pub async fn parser_complete(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ParserFinishRequest,
) -> Result<TaskView, crate::shared::AppError> {
    let task = state.parse.complete_parse(&request.parse_job_token)?;
    Ok(task.to_view())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParserFailRequest {
    pub parse_job_token: String,
    pub error_code: String,
}

#[tauri::command]
pub async fn parser_fail(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ParserFailRequest,
) -> Result<TaskView, crate::shared::AppError> {
    let task = state
        .parse
        .fail_parse(&request.parse_job_token, &request.error_code)?;
    Ok(task.to_view())
}

// —— 2D 页级 OCR ——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrReportRequest {
    pub parse_job_token: String,
    pub entries: Vec<PageOcrReportEntry>,
}

#[tauri::command]
pub async fn page_ocr_report(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageOcrReportRequest,
) -> Result<(), crate::shared::AppError> {
    let svc = std::sync::Arc::clone(&state.parse);
    tauri::async_runtime::spawn_blocking(move || {
        svc.report_page_ocr(&request.parse_job_token, &request.entries)
    })
    .await
    .map_err(|e| crate::shared::AppError::internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrStatusRequest {
    pub document_id: String,
    pub version_id: String,
}

#[tauri::command]
pub async fn page_ocr_get_status(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageOcrStatusRequest,
) -> Result<Vec<PageOcrStatusView>, crate::shared::AppError> {
    state
        .parse
        .get_page_ocr_status(&request.document_id, &request.version_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrControlRequest {
    pub document_id: String,
    pub version_id: String,
}

#[tauri::command]
pub async fn page_ocr_pause(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageOcrControlRequest,
) -> Result<PageOcrControlView, crate::shared::AppError> {
    state
        .parse
        .set_page_ocr_paused(&request.document_id, &request.version_id, true)
}

#[tauri::command]
pub async fn page_ocr_resume(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageOcrControlRequest,
) -> Result<PageOcrControlView, crate::shared::AppError> {
    state
        .parse
        .set_page_ocr_paused(&request.document_id, &request.version_id, false)
}

#[tauri::command]
pub async fn page_ocr_control_get(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageOcrControlRequest,
) -> Result<PageOcrControlView, crate::shared::AppError> {
    state
        .parse
        .get_page_ocr_control(&request.document_id, &request.version_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrTextRequest {
    pub document_id: String,
    pub version_id: String,
    pub page_number: i64,
}

#[tauri::command]
pub async fn page_ocr_get_text(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageOcrTextRequest,
) -> Result<PageOcrText, crate::shared::AppError> {
    state.parse.get_page_ocr_text(
        &request.document_id,
        &request.version_id,
        request.page_number,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrCorrectRequest {
    pub document_id: String,
    pub version_id: String,
    pub page_number: i64,
    pub blocks: Vec<PageOcrBlockCorrection>,
    /// 编辑器中明确删除的块 ID(软删除:移出索引与编辑视图)。
    #[serde(default)]
    pub remove_block_ids: Vec<String>,
    pub idempotency_key: String,
}

/// 人工纠错 OCR 文本并重建该页 FTS/覆盖;只改用户明确编辑的文本。
#[tauri::command]
pub async fn page_ocr_apply_correction(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageOcrCorrectRequest,
) -> Result<Vec<PageOcrStatusView>, crate::shared::AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.parse.correct_page_ocr(
            &request.document_id,
            &request.version_id,
            request.page_number,
            &request.blocks,
            &request.remove_block_ids,
            &request.idempotency_key,
        )
    })
    .await
    .map_err(|e| crate::shared::AppError::internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrRecognitionRequest {
    pub document_id: String,
    pub version_id: String,
    pub pages: Vec<PageOcrRecognitionPage>,
    pub idempotency_key: String,
}

/// 指定页 OCR:前端识别结果落库并重建这些页的 FTS/覆盖(不调用模型)。
#[tauri::command]
pub async fn page_ocr_save_recognition(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageOcrRecognitionRequest,
) -> Result<Vec<PageOcrStatusView>, crate::shared::AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.parse.save_page_ocr_recognition(
            &request.document_id,
            &request.version_id,
            &request.pages,
            &request.idempotency_key,
        )
    })
    .await
    .map_err(|e| crate::shared::AppError::internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrReprocessRequest {
    pub document_id: String,
    pub version_id: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn page_ocr_reprocess(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PageOcrReprocessRequest,
) -> Result<TaskView, crate::shared::AppError> {
    let task = state.parse.reprocess_ocr(
        &request.document_id,
        &request.version_id,
        &request.idempotency_key,
    )?;
    Ok(task.to_view())
}

// —— 嵌入批次(由 M08 ModelGateway 编排调用;命令形态便于后续接入)——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingBatchItemDto {
    pub block_id: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingBatchRequest {
    pub embedding_generation_id: String,
    pub items: Vec<EmbeddingBatchItemDto>,
}

#[tauri::command]
pub async fn parser_submit_embeddings(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: EmbeddingBatchRequest,
) -> Result<(), crate::shared::AppError> {
    let items: Vec<EmbeddingInput> = request
        .items
        .into_iter()
        .map(|i| EmbeddingInput {
            block_id: i.block_id,
            vector: i.vector,
        })
        .collect();
    state
        .parse
        .submit_embedding_batch(&request.embedding_generation_id, &items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_deserialize_camel_case() {
        let req: ParserReadInputRequest =
            serde_json::from_str(r#"{"parseJobToken":"t","offset":10,"length":1024}"#).unwrap();
        assert_eq!(req.offset, 10);
        assert_eq!(req.length, 1024);
        let batch: ParserBatchInput = serde_json::from_str(
            r#"{"batchSeq":1,"blocks":[],"regions":[],"unsupportedRegions":[]}"#,
        )
        .unwrap();
        assert_eq!(batch.batch_seq, 1);
        assert!(batch.blocks.is_empty());
    }
}
