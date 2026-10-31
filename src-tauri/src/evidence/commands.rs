//! M05 命令层(薄封装,`desktop` feature,契约见《02》§3.2)。
//! 业务逻辑全部在服务层;命令只做请求反序列化与视图返回。

use serde::Deserialize;

use super::models::*;
use crate::shared::AppError;

fn parse_owner_type(s: &str) -> Result<CitationOwnerType, AppError> {
    CitationOwnerType::parse_db(s).ok_or_else(|| {
        AppError::new(
            crate::shared::CoreErrorCode::InvalidRequest,
            crate::shared::new_trace_id(),
        )
        .with_detail("reason", "invalid_owner_type".into())
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceCreateAnchorRequest {
    pub raw_selection: RawSelectionInput,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn evidence_create_anchor(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: EvidenceCreateAnchorRequest,
) -> Result<AnchorView, AppError> {
    state
        .evidence
        .create_anchor(&request.raw_selection, &request.idempotency_key)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceResolveCitationRequest {
    pub citation_id: String,
}

#[tauri::command]
pub async fn evidence_resolve_citation(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: EvidenceResolveCitationRequest,
) -> Result<EvidenceResolution, AppError> {
    state.evidence.resolve_citation(&request.citation_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceLensRequest {
    pub owner_type: String,
    pub owner_id: String,
}

#[tauri::command]
pub async fn evidence_get_lens(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: EvidenceLensRequest,
) -> Result<EvidenceLensView, AppError> {
    let owner_type = parse_owner_type(&request.owner_type)?;
    state.evidence.get_lens(owner_type, &request.owner_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceFeedbackRequest {
    pub citation_id: String,
    pub feedback_type: String,
    #[serde(default)]
    pub comment: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn evidence_feedback(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: EvidenceFeedbackRequest,
) -> Result<FeedbackResult, AppError> {
    let ft = EvidenceFeedbackType::parse_db(&request.feedback_type).ok_or_else(|| {
        AppError::new(
            crate::shared::CoreErrorCode::InvalidRequest,
            crate::shared::new_trace_id(),
        )
        .with_detail("reason", "invalid_feedback_type".into())
    })?;
    state.evidence.submit_feedback(
        &request.citation_id,
        ft,
        &request.comment,
        &request.idempotency_key,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceConfirmSupportRequest {
    pub citation_id: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn evidence_confirm_support(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: EvidenceConfirmSupportRequest,
) -> Result<CitationView, AppError> {
    state
        .evidence
        .confirm_support(&request.citation_id, &request.idempotency_key)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceProposeRelocationRequest {
    pub anchor_id: String,
    pub target_version_id: String,
}

#[tauri::command]
pub async fn evidence_propose_relocation(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: EvidenceProposeRelocationRequest,
) -> Result<Vec<RelocationCandidate>, AppError> {
    state
        .evidence
        .propose_relocation(&request.anchor_id, &request.target_version_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceConfirmRelocationRequest {
    pub anchor_id: String,
    pub candidate_id: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn evidence_confirm_relocation(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: EvidenceConfirmRelocationRequest,
) -> Result<AnchorView, AppError> {
    state.evidence.confirm_relocation(
        &request.anchor_id,
        &request.candidate_id,
        &request.idempotency_key,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRejectRelocationRequest {
    pub anchor_id: String,
    pub candidate_id: String,
}

#[tauri::command]
pub async fn evidence_reject_relocation(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: EvidenceRejectRelocationRequest,
) -> Result<RelocationCandidate, AppError> {
    state
        .evidence
        .reject_relocation(&request.anchor_id, &request.candidate_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_deserialize_camel_case() {
        let req: EvidenceFeedbackRequest = serde_json::from_str(
            r#"{"citationId":"c1","feedbackType":"quote_wrong","comment":"","idempotencyKey":"k1"}"#,
        )
        .unwrap();
        assert_eq!(req.citation_id, "c1");
        assert_eq!(req.feedback_type, "quote_wrong");
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveBlockRequest {
    pub block_id: String,
}

/// 2B:块 ID → 可回跳定位(卡片/题目来源)。不存在返回 null,不伪造。
#[tauri::command]
pub async fn evidence_resolve_block(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ResolveBlockRequest,
) -> Result<Option<crate::shared::locator::SourceLocator>, AppError> {
    state.evidence.resolve_block_locator(&request.block_id)
}
