//! M08 命令层(薄封装,`desktop` feature,契约见《02》§3.3)。
//! 业务逻辑全部在服务层;命令只做请求反序列化与视图返回。
//! 密钥命令只返回 configured/updatedAt,永不回显秘密。

use serde::Deserialize;

use super::models::*;
use super::settings::{ModelSettingsPatch, ModelSettingsView};
use crate::shared::AppError;

fn parse_purpose(s: &str) -> Result<ModelPurpose, AppError> {
    ModelPurpose::parse_db(s).ok_or_else(invalid_request)
}

fn parse_location(s: &str) -> Result<ProcessingLocation, AppError> {
    ProcessingLocation::parse_db(s).ok_or_else(invalid_request)
}

fn invalid_request() -> AppError {
    AppError::new(
        crate::shared::CoreErrorCode::InvalidRequest,
        crate::shared::new_trace_id(),
    )
    .with_detail("reason", "invalid_enum_value".into())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatusRequest {
    #[serde(default)]
    pub document_id: Option<String>,
}

#[tauri::command]
pub async fn model_get_status(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ModelStatusRequest,
) -> Result<ModelStatusView, AppError> {
    // documentId 目前不影响全局能力;保留参数位(《02》§3.3 契约)。
    let _ = request.document_id;
    state.model_privacy.model_get_status().await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListLocalRequest {}

#[tauri::command]
pub async fn model_list_local_models(
    state: tauri::State<'_, crate::app_shell::AppState>,
    _request: ModelListLocalRequest,
) -> Result<Vec<ModelDescriptor>, AppError> {
    state.model_privacy.model_list_local_models().await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSetReferenceRequest {
    pub role: String,
    pub provider_id: String,
    pub model_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn model_set_reference(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ModelSetReferenceRequest,
) -> Result<ModelStatusView, AppError> {
    if let Some(role) = crate::model_privacy::models::GenerationRole::parse_db(&request.role) {
        return state
            .model_privacy
            .model_set_generation_role(
                role,
                &request.provider_id,
                &request.model_id,
                request.expected_revision,
            )
            .await;
    }
    let role = parse_purpose(&request.role)?;
    state
        .model_privacy
        .model_set_reference(
            role,
            &request.provider_id,
            &request.model_id,
            request.expected_revision,
        )
        .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSecretSetRequest {
    pub provider_id: String,
    pub secret: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn remote_secret_set(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: RemoteSecretSetRequest,
) -> Result<SecretState, AppError> {
    state.model_privacy.remote_secret_set(
        &request.provider_id,
        &request.secret,
        &request.idempotency_key,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSecretDeleteRequest {
    pub provider_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn remote_secret_delete(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: RemoteSecretDeleteRequest,
) -> Result<SecretState, AppError> {
    state
        .model_privacy
        .remote_secret_delete(&request.provider_id, request.expected_revision)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionStatusRequest {
    pub document_id: String,
}

#[tauri::command]
pub async fn vision_get_status(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: VisionStatusRequest,
) -> Result<VisionStatusView, AppError> {
    state.model_privacy.vision_status(&request.document_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionAllowRequest {
    pub document_id: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn vision_allow_remote(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: VisionAllowRequest,
) -> Result<VisionStatusView, AppError> {
    state
        .model_privacy
        .vision_allow_remote(&request.document_id, &request.idempotency_key)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyStatusRequest {
    pub document_id: String,
}

#[tauri::command]
pub async fn privacy_get_status(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PrivacyStatusRequest,
) -> Result<PrivacyStatusView, AppError> {
    state
        .model_privacy
        .privacy_get_status(&request.document_id)
        .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyGrantRequest {
    pub document_id: String,
    pub processing_location: String,
    pub provider_id: String,
    pub purpose: String,
    pub policy_version: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn privacy_grant(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PrivacyGrantRequest,
) -> Result<PrivacyStatusView, AppError> {
    let location = parse_location(&request.processing_location)?;
    let purpose = parse_purpose(&request.purpose)?;
    state
        .model_privacy
        .privacy_grant(
            &request.document_id,
            location,
            &request.provider_id,
            purpose,
            &request.policy_version,
            &request.idempotency_key,
        )
        .map(|_| ())?;
    state
        .model_privacy
        .privacy_get_status(&request.document_id)
        .await
}

#[tauri::command]
pub async fn privacy_revoke(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PrivacyGrantRequest,
) -> Result<PrivacyStatusView, AppError> {
    let location = parse_location(&request.processing_location)?;
    let purpose = parse_purpose(&request.purpose)?;
    state
        .model_privacy
        .privacy_revoke(
            &request.document_id,
            location,
            &request.provider_id,
            purpose,
            &request.idempotency_key,
        )
        .map(|_| ())?;
    state
        .model_privacy
        .privacy_get_status(&request.document_id)
        .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyPreviewClearRequest {
    pub document_id: String,
    pub categories: Vec<String>,
}

#[tauri::command]
pub async fn privacy_preview_clear(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PrivacyPreviewClearRequest,
) -> Result<ClearPreview, AppError> {
    let mut categories = Vec::new();
    for c in &request.categories {
        categories.push(ClearCategory::parse_db(c).ok_or_else(invalid_request)?);
    }
    state
        .model_privacy
        .privacy_preview_clear(&request.document_id, &categories)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyExecuteClearRequest {
    pub document_id: String,
    pub clear_plan_id: String,
    pub plan_hash: String,
    pub confirmation: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn privacy_execute_clear(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: PrivacyExecuteClearRequest,
) -> Result<ClearResult, AppError> {
    state.model_privacy.privacy_execute_clear(
        &request.document_id,
        &request.clear_plan_id,
        &request.plan_hash,
        &request.confirmation,
        &request.idempotency_key,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsGetRequest {}

#[tauri::command]
pub async fn settings_get(
    state: tauri::State<'_, crate::app_shell::AppState>,
    _request: SettingsGetRequest,
) -> Result<ModelSettingsView, AppError> {
    state.model_privacy.settings_get()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateRequest {
    pub patch: ModelSettingsPatch,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn settings_update(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: SettingsUpdateRequest,
) -> Result<ModelSettingsView, AppError> {
    state
        .model_privacy
        .settings_update(&request.patch, request.expected_revision)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsPreviewRequest {
    #[serde(default)]
    pub include_paths: bool,
    #[serde(default)]
    pub include_model_details: bool,
}

#[tauri::command]
pub async fn diagnostics_preview(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DiagnosticsPreviewRequest,
) -> Result<DiagnosticsPreview, AppError> {
    state
        .model_privacy
        .diagnostics_preview(request.include_paths, request.include_model_details)
        .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsExportRequest {
    pub preview_hash: String,
    pub path: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn diagnostics_export(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: DiagnosticsExportRequest,
) -> Result<DiagnosticsExportResult, AppError> {
    state.model_privacy.diagnostics_export(
        &request.preview_hash,
        &request.path,
        &request.idempotency_key,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_deserialize_camel_case() {
        let req: PrivacyGrantRequest = serde_json::from_str(
            r#"{"documentId":"d1","processingLocation":"remote","providerId":"p1","purpose":"generate","policyVersion":"privacy.policy.v1","idempotencyKey":"k1"}"#,
        )
        .unwrap();
        assert_eq!(req.document_id, "d1");
        assert_eq!(req.processing_location, "remote");
        let req: PrivacyExecuteClearRequest = serde_json::from_str(
            r#"{"documentId":"d1","clearPlanId":"plan-1","planHash":"h","confirmation":"d1","idempotencyKey":"k"}"#,
        )
        .unwrap();
        assert_eq!(req.clear_plan_id, "plan-1");
    }

    #[test]
    fn enum_parsers_reject_unknown_values() {
        assert!(parse_purpose("generate").is_ok());
        assert!(parse_purpose("vision").is_ok());
        assert!(parse_purpose("chat").is_err());
        assert!(parse_location("remote").is_ok());
        assert!(parse_location("cloud").is_err());
    }
}

// —— 2F 长上下文:模型能力(窗口/输出)读取与手动修正 ——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilityRequest {
    pub provider_id: String,
    pub model_id: String,
}

/// 读取某模型的能力(未记录时返回内置保守默认,不报错)。
#[tauri::command]
pub async fn model_capability_get(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ModelCapabilityRequest,
) -> Result<ModelCapabilityView, AppError> {
    let capability = state
        .model_privacy
        .model_capability(&request.provider_id, &request.model_id)
        .ok_or_else(|| {
            AppError::new(
                crate::shared::CoreErrorCode::Internal,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "capability_read_failed".into())
        })?;
    Ok(ModelCapabilityView::from_capability(&capability))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilitySetRequest {
    pub provider_id: String,
    pub model_id: String,
    pub context_window: i64,
    pub max_output: i64,
    #[serde(default)]
    pub tool_calling: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub json_schema: bool,
}

/// 手动修正模型能力:写入 `source='manual'`,自动探测不再覆盖。
#[tauri::command]
pub async fn model_capability_set(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ModelCapabilitySetRequest,
) -> Result<ModelCapabilityView, AppError> {
    let saved =
        state
            .model_privacy
            .set_model_capability(super::capability::CapabilityOverride {
                provider_id: request.provider_id,
                model_id: request.model_id,
                context_window: request.context_window,
                max_output: request.max_output,
                tool_calling: request.tool_calling,
                vision: request.vision,
                json_schema: request.json_schema,
            })?;
    Ok(ModelCapabilityView::from_capability(&saved))
}
