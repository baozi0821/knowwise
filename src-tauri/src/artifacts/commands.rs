//! M10 命令层(薄封装,`desktop` feature,契约见《02》§3.2)。
//! `artifact_generate`/`artifact_regenerate` 需要 M09 Generator,M13 组装后
//! 由 M11/M12 落地;本层提供落库与编辑导出命令。

use serde::Deserialize;

use super::models::*;
use super::service::{AiDraftCommitInput, CitationDraftForCommit};
use crate::shared::dto::DeleteResult;
use crate::shared::paging::PageRequest;
use crate::shared::{AppError, Clock, IdGenerator};
use std::sync::Arc;

// —— 生成落库(M09 编排完成后调用;当前也可由测试/集成直接驱动)——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCommitDraftRequest {
    pub document_id: String,
    pub kind: String,
    pub title: String,
    pub content: serde_json::Value,
    pub coverage: crate::artifacts::models::CoverageReport,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn artifact_commit_draft(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactCommitDraftRequest,
) -> Result<ArtifactView, AppError> {
    let input = AiDraftCommitInput {
        document_id: request.document_id,
        kind: request.kind,
        title: request.title,
        content: request.content,
        coverage: request.coverage,
        snapshot_id: request.snapshot_id,
        task_id: request.task_id,
        citations: vec![],
    };
    state
        .artifacts
        .commit_ai_draft(&input, &request.idempotency_key)
}

// —— 产物中心 ——

#[tauri::command]
pub async fn artifact_list(
    state: tauri::State<'_, crate::app_shell::AppState>,
    document_id: String,
    kind: Option<String>,
    cursor: Option<String>,
) -> Result<ArtifactPage, AppError> {
    state.artifacts.list_artifacts(
        &document_id,
        kind.as_deref(),
        PageRequest { limit: 50, cursor },
    )
}

#[tauri::command]
pub async fn artifact_get(
    state: tauri::State<'_, crate::app_shell::AppState>,
    artifact_id: String,
    version_id: Option<String>,
) -> Result<ArtifactView, AppError> {
    state
        .artifacts
        .get_artifact(&artifact_id, version_id.as_deref())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRenameRequest {
    pub artifact_id: String,
    pub title: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn artifact_rename(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactRenameRequest,
) -> Result<ArtifactView, AppError> {
    state.artifacts.rename_artifact(
        &request.artifact_id,
        &request.title,
        request.expected_revision,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCopyRequest {
    pub artifact_id: String,
    pub version_id: String,
    /// 默认 true:复制为新产物;false 时在原产物下创建 copy 版本。
    #[serde(default = "default_true")]
    pub as_new_artifact: bool,
    pub idempotency_key: String,
}

fn default_true() -> bool {
    true
}

#[tauri::command]
pub async fn artifact_copy(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactCopyRequest,
) -> Result<ArtifactView, AppError> {
    state.artifacts.copy_artifact(
        &request.artifact_id,
        &request.version_id,
        request.as_new_artifact,
        &request.idempotency_key,
    )
}

// —— 编辑与版本保护 ——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactBeginEditRequest {
    #[serde(flatten)]
    pub input: ArtifactBeginEditInput,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn artifact_begin_edit(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactBeginEditRequest,
) -> Result<ArtifactView, AppError> {
    state
        .artifacts
        .begin_edit(&request.input, &request.idempotency_key)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSaveRequest {
    #[serde(flatten)]
    pub input: ArtifactSaveInput,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn artifact_save(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactSaveRequest,
) -> Result<ArtifactView, AppError> {
    state
        .artifacts
        .save_version(&request.input, &request.idempotency_key)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSealRequest {
    pub artifact_id: String,
    pub version_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn artifact_seal(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactSealRequest,
) -> Result<ArtifactView, AppError> {
    state.artifacts.seal_version(
        &request.artifact_id,
        &request.version_id,
        request.expected_revision,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRestoreRequest {
    pub artifact_id: String,
    pub source_version_id: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn artifact_restore(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactRestoreRequest,
) -> Result<ArtifactView, AppError> {
    state.artifacts.restore_version(
        &request.artifact_id,
        &request.source_version_id,
        &request.idempotency_key,
    )
}

// —— 删除 ——

#[tauri::command]
pub async fn artifact_delete_preview(
    state: tauri::State<'_, crate::app_shell::AppState>,
    artifact_id: String,
) -> Result<ArtifactDeletePreview, AppError> {
    state.artifacts.delete_preview(&artifact_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDeleteRequest {
    pub artifact_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn artifact_delete(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactDeleteRequest,
) -> Result<DeleteResult, AppError> {
    state
        .artifacts
        .delete_artifact(&request.artifact_id, request.expected_revision)
}

// —— 导出 ——

#[tauri::command]
pub async fn artifact_export_preview(
    state: tauri::State<'_, crate::app_shell::AppState>,
    artifact_id: String,
    version_id: String,
    variant: Option<String>,
) -> Result<ArtifactExportPreview, AppError> {
    let markdown = crate::exporters::markdown::preview_markdown_with_variant(
        &state.artifacts,
        &artifact_id,
        &version_id,
        variant.as_deref().unwrap_or("bilingual"),
    )?;
    Ok(ArtifactExportPreview {
        artifact_id,
        version_id,
        format: "markdown".into(),
        markdown,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactExportRequest {
    pub artifact_id: String,
    pub version_id: String,
    pub format: String,
    /// 用户经保存对话框选择的显式目标路径。
    pub path: String,
    /// translation 导出变体:bilingual / translation_only。
    #[serde(default)]
    pub variant: Option<String>,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn artifact_export(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactExportRequest,
) -> Result<ExportResult, AppError> {
    let path = std::path::PathBuf::from(&request.path);
    let deps = crate::exporters::ExportDeps {
        service: &state.artifacts,
        clock: state.core.clock.clone(),
        ids: state.core.ids.clone(),
        variant: request.variant.as_deref().unwrap_or("bilingual"),
    };
    // 格式白名单与分支集中在 `exporters` 门面:markdown/docx,
    // 未知格式仍返回 INVALID_REQUEST(reason=unsupported_format)。
    crate::exporters::export_artifact(
        &deps,
        &request.artifact_id,
        &request.version_id,
        &request.format,
        &path,
    )
}

// —— 引用草稿落库(M09 校验后经 M05 保存;本命令归 M13 组装期使用)——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSaveCitationsRequest {
    pub artifact_id: String,
    pub version_id: String,
    pub citations: Vec<ArtifactCitationDraft>,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCitationDraft {
    pub claim_key: String,
    pub claim_kind: String,
    pub content_block_id: String,
    pub quoted_text: String,
}

#[tauri::command]
pub async fn artifact_save_citations(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: ArtifactSaveCitationsRequest,
) -> Result<i64, AppError> {
    save_citations_impl(&state, &request)
}

fn save_citations_impl(
    state: &tauri::State<'_, crate::app_shell::AppState>,
    request: &ArtifactSaveCitationsRequest,
) -> Result<i64, AppError> {
    use crate::evidence::models::{
        CitationInput, CitationOwnerType, CitationSourceType, ClaimKind, SaveCitationsRequest,
    };
    let view = state
        .artifacts
        .get_artifact(&request.artifact_id, Some(&request.version_id))?;
    let version = view
        .current_version
        .clone()
        .or_else(|| view.versions.first().map(|v| Box::new(v.clone())))
        .ok_or_else(|| AppError::new(crate::shared::CoreErrorCode::NotFound, "export"))?;
    // Citation.document_version_id 必须指向真实 document_version;
    // 版本表只保存 snapshot_id,这里经 M07 快照反查(此前误把 snapshotId
    // 当 versionId 写入,会触发外键失败)。
    let document_version_id = match &version.snapshot_id {
        Some(snapshot_id) => state
            .context
            .get_snapshot(snapshot_id)?
            .map(|snapshot| snapshot.document_version_id)
            .unwrap_or_default(),
        None => String::new(),
    };
    if document_version_id.is_empty() {
        return Err(AppError::new(
            crate::shared::CoreErrorCode::InvalidRequest,
            crate::shared::new_trace_id(),
        )
        .with_detail("reason", "artifact_snapshot_missing".into()));
    }
    let citations: Vec<CitationInput> = request
        .citations
        .iter()
        .map(|c| {
            let claim_kind = ClaimKind::parse_db(&c.claim_kind).unwrap_or(ClaimKind::DocumentFact);
            CitationInput {
                claim_key: c.claim_key.clone(),
                claim_kind,
                source_type: CitationSourceType::ContentBlock,
                source_id: None,
                anchor_id: None,
                content_block_id: Some(c.content_block_id.clone()),
                quoted_text: c.quoted_text.clone(),
                locator: None,
                support_status: None,
                support_source: None,
            }
        })
        .collect();
    let req = SaveCitationsRequest {
        owner_type: CitationOwnerType::ArtifactVersion,
        owner_id: version.id.clone(),
        document_id: view.document_id.clone(),
        document_version_id,
        citations,
        idempotency_key: request.idempotency_key.clone(),
    };
    let saved = state.evidence.save_citations(&req)?;
    Ok(saved.len() as i64)
}

/// 引用草稿入参(复用上面内部类型;测试驱动用)。
#[allow(dead_code)]
fn _draft_type_hint(_: &CitationDraftForCommit) {}

#[allow(dead_code)]
fn _arc_hint(_: &Arc<dyn Clock>) {}

#[allow(dead_code)]
fn _arc_hint2(_: &Arc<dyn IdGenerator>) {}
