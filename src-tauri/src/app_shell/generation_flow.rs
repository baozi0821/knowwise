//! M13 产物生成编排(摘要 / 选择题),`desktop` feature。
//!
//! 统一流程:快照 → 材料 → Task(GenerateArtifact) → 结构化生成
//! (M08 唯一出口) → 校验 → M10 `commit_ai_draft` → M05 Citation →
//! 任务结果引用 ArtifactVersion。生成失败不落产物,Task 保留失败原因。
//!
//! 异步入口快速返回 TaskView,专用线程执行生成,由 generation_get 轮询结果。
//! 幂等:同 idempotencyKey 的 running/终态任务直接回放,不再发起模型请求;
//! `artifact_version.task_id` 的唯一约束兜底防重复。

// 默认特性只运行编排契约;桌面入口及其专用生成器此时不使用。
#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::artifacts::models::{
    ArtifactRegenerateResult, ArtifactView, CoverageRegion, CoverageReport, SummaryContentV1,
};
use crate::artifacts::service::{
    AiDraftCommitInput, CitationDraftForCommit, RegeneratedDraftInput,
};
use crate::context::models::ContextSnapshotView;
use crate::evidence::models::{
    CitationInput, CitationOwnerType, CitationSourceType, ClaimKind, SaveCitationsRequest,
};
use crate::parse_index::models::ContentBlockView;
use crate::persistence::task::{AttemptRow, CreateTask, TaskRow};
use crate::quiz::generator::{QuizGenerationContext, QuizGenerator};
use crate::quiz::models::{
    QuestionValidity, QuizContentV1, QuizGenerationConfig, QuizGenerationMaterials,
    QuizMaterialBlock,
};
use crate::quiz::v2::{
    parse_quiz_v2, quiz_v2_schema, validate_quiz_v2, QuizContentV2, QuizV2GenerationConfig,
    QUIZ_V2_INSTRUCTION_VERSION, QUIZ_V2_SCHEMA_VERSION,
};
use crate::retrieval_ai::models::{
    GenerationInput, SourceRole, StructuredGenerationRequest, TaskKind,
};
use crate::retrieval_ai::summary::summary_materials;
use crate::shared::dto::{
    TaskOutcome, TaskPhase, TaskResultRef, TaskResultRefKind, TaskStatus, TaskType, TaskView,
};
use crate::shared::{AppError, CoreErrorCode};

use super::quiz_port::QuizGenerationPortAdapter;
use super::AppState;

/// 生成类任务在题目中心的标题上限(与 M10 标题限制一致)。
const GENERATION_TITLE_MAX_CHARS: usize = 80;
/// 题目材料预算:超出请用户缩小范围,不静默截断。
const MAX_QUIZ_BLOCKS: usize = 80;
const MAX_QUIZ_MATERIAL_CHARS: usize = 30_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactGenerateRequest {
    pub document_id: String,
    pub snapshot_id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// summary / study_note / executive_summary / faq / comparison / timeline
    #[serde(default)]
    pub template: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizGenerateRequest {
    pub document_id: String,
    pub snapshot_id: String,
    pub config: QuizGenerationConfig,
    #[serde(default)]
    pub title: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizV2GenerateRequest {
    pub document_id: String,
    pub snapshot_id: String,
    #[serde(default)]
    pub config: QuizV2GenerationConfig,
    #[serde(default)]
    pub title: Option<String>,
    pub idempotency_key: String,
}

/// `artifact_generate`:提交摘要生成任务,不等待模型返回。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn artifact_generate(
    state: tauri::State<'_, AppState>,
    request: ArtifactGenerateRequest,
) -> Result<TaskView, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        start_background_generation(
            state,
            &request.snapshot_id,
            &request.document_id,
            &request.idempotency_key,
            move |state, snapshot, task, attempt| {
                run_summary_generation(
                    state,
                    snapshot,
                    task,
                    attempt,
                    request.title,
                    request.template.as_deref().unwrap_or("summary"),
                    None,
                )
            },
        )
    })
    .await
    .map_err(|_| AppError::internal("generation_prepare_join_failed"))?
}

/// `quiz_generate`:兼容旧调用者,在 blocking 线程等待并返回产物。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn quiz_generate(
    state: tauri::State<'_, AppState>,
    request: QuizGenerateRequest,
) -> Result<ArtifactView, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let snapshot = load_snapshot(&state, &request.snapshot_id, &request.document_id)?;
        let task = start_task(&state, &request.document_id, &request.idempotency_key)?;
        if let Some(view) = existing_if_finished(&state, &task)? {
            return Ok(view);
        }
        let (task, attempt) = begin_attempt(&state, task)?;
        execute_generation(&state, &task, &attempt, || {
            run_quiz_generation(
                &state,
                &snapshot,
                &task,
                &attempt,
                request.config,
                request.title,
                None,
            )
        })
        .map(|draft| draft.view)
    })
    .await
    .map_err(|_| AppError::internal("generation_worker_join_failed"))?
}

/// `quiz_generate_v2`:提交混合题型任务;无效题保留并标记,不参与作答。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn quiz_generate_v2(
    state: tauri::State<'_, AppState>,
    request: QuizV2GenerateRequest,
) -> Result<TaskView, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        start_background_generation(
            state,
            &request.snapshot_id,
            &request.document_id,
            &request.idempotency_key,
            move |state, snapshot, task, attempt| {
                run_quiz_v2_generation(
                    state,
                    snapshot,
                    task,
                    attempt,
                    request.config,
                    request.title,
                )
            },
        )
    })
    .await
    .map_err(|_| AppError::internal("generation_prepare_join_failed"))?
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRegenerateRequest {
    pub artifact_id: String,
    pub snapshot_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizRegenerateRequest {
    pub artifact_id: String,
    pub snapshot_id: String,
    pub config: QuizGenerationConfig,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationGetRequest {
    pub task_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationGetResult {
    pub task: TaskView,
    /// 非成功状态始终为 null,不暴露尚未完成提交的草稿。
    pub artifact: Option<ArtifactView>,
}

/// `generation_get`:只读轮询 GenerateArtifact 任务及成功产物。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn generation_get(
    state: tauri::State<'_, AppState>,
    request: GenerationGetRequest,
) -> Result<GenerationGetResult, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || generation_status(&state, &request.task_id))
        .await
        .map_err(|_| AppError::internal("generation_get_join_failed"))?
}

fn generation_status(state: &AppState, task_id: &str) -> Result<GenerationGetResult, AppError> {
    let task = get_generation_task(state, task_id)?;
    let artifact = if task.status == TaskStatus::Succeeded {
        state.artifacts.find_artifact_by_task(&task.id)?
    } else {
        None
    };
    Ok(GenerationGetResult {
        task: task.to_view(),
        artifact,
    })
}

fn get_generation_task(state: &AppState, task_id: &str) -> Result<TaskRow, AppError> {
    let task = state.tasks.get_task(task_id)?.ok_or_else(|| {
        AppError::new(CoreErrorCode::NotFound, crate::shared::new_trace_id())
            .with_detail("reason", "task_not_found".into())
    })?;
    if task.task_type != TaskType::GenerateArtifact {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "not_generation_task".into()),
        );
    }
    Ok(task)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationCancelRequest {
    pub task_id: String,
}

/// `generation_cancel`:请求取消后台生成任务,传播 tasks 的取消令牌;
/// 生成线程以 TASK_CANCELED 收敛后由 finish_generation 落为 canceled。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn generation_cancel(
    state: tauri::State<'_, AppState>,
    request: GenerationCancelRequest,
) -> Result<TaskView, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cancel_generation(&state, &request.task_id))
        .await
        .map_err(|_| AppError::internal("generation_cancel_join_failed"))?
}

fn cancel_generation(state: &AppState, task_id: &str) -> Result<TaskView, AppError> {
    let task = get_generation_task(state, task_id)?;
    // 原样保留 TaskService 的版本检查、queued 直取消、running 请求/ack
    // 两阶段契约及终态 TASK_NOT_RUNNING,不伪造提前取消成功。
    let task = state.tasks.request_cancel(&task.id, task.state_version)?;
    Ok(task.to_view())
}

/// `artifact_regenerate`:保留产物/版本响应,在 blocking 线程追加 ai_draft。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn artifact_regenerate(
    state: tauri::State<'_, AppState>,
    request: ArtifactRegenerateRequest,
) -> Result<ArtifactRegenerateResult, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let artifact = state.artifacts.get_artifact(&request.artifact_id, None)?;
        require_kind(&artifact, "summary")?;
        let snapshot = load_snapshot(&state, &request.snapshot_id, &artifact.document_id)?;
        let task = start_task(&state, &artifact.document_id, &request.idempotency_key)?;
        if let Some(result) =
            existing_regeneration_if_finished(&state, &task, &request.artifact_id)?
        {
            return Ok(result);
        }
        let (task, attempt) = begin_attempt(&state, task)?;
        execute_generation(&state, &task, &attempt, || {
            run_summary_generation(
                &state,
                &snapshot,
                &task,
                &attempt,
                None,
                "summary",
                Some(&request.artifact_id),
            )
        })
        .map(|draft| ArtifactRegenerateResult {
            artifact: draft.view,
            version_id: draft.version_id,
        })
    })
    .await
    .map_err(|_| AppError::internal("generation_worker_join_failed"))?
}

/// `quiz_regenerate`:保留产物/版本响应,在 blocking 线程追加 ai_draft。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn quiz_regenerate(
    state: tauri::State<'_, AppState>,
    request: QuizRegenerateRequest,
) -> Result<ArtifactRegenerateResult, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let artifact = state.artifacts.get_artifact(&request.artifact_id, None)?;
        require_kind(&artifact, "quiz")?;
        let snapshot = load_snapshot(&state, &request.snapshot_id, &artifact.document_id)?;
        let task = start_task(&state, &artifact.document_id, &request.idempotency_key)?;
        if let Some(result) =
            existing_regeneration_if_finished(&state, &task, &request.artifact_id)?
        {
            return Ok(result);
        }
        let (task, attempt) = begin_attempt(&state, task)?;
        execute_generation(&state, &task, &attempt, || {
            run_quiz_generation(
                &state,
                &snapshot,
                &task,
                &attempt,
                request.config,
                None,
                Some(&request.artifact_id),
            )
        })
        .map(|draft| ArtifactRegenerateResult {
            artifact: draft.view,
            version_id: draft.version_id,
        })
    })
    .await
    .map_err(|_| AppError::internal("generation_worker_join_failed"))?
}

fn require_kind(artifact: &ArtifactView, expected: &str) -> Result<(), AppError> {
    if artifact.kind != expected {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "artifact_kind_mismatch".into())
                .with_detail("expected", expected.to_string().into())
                .with_detail("actual", artifact.kind.clone().into()),
        );
    }
    Ok(())
}

fn load_snapshot(
    state: &AppState,
    snapshot_id: &str,
    document_id: &str,
) -> Result<ContextSnapshotView, AppError> {
    let snapshot = state
        .context
        .get_snapshot(snapshot_id)?
        .ok_or_else(|| AppError::new(CoreErrorCode::NotFound, crate::shared::new_trace_id()))?;
    if snapshot.document_id != document_id {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "snapshot_document_mismatch".into()),
        );
    }
    Ok(snapshot)
}

fn start_task(
    state: &AppState,
    document_id: &str,
    idempotency_key: &str,
) -> Result<TaskRow, AppError> {
    let task = state.tasks.create_task(CreateTask {
        document_id: Some(document_id.to_string()),
        task_type: TaskType::GenerateArtifact,
        idempotency_key: idempotency_key.to_string(),
        phase: TaskPhase::Waiting,
    })?;
    // Task 的键是全局唯一的,不能借其他类型/文档的键启动生成。
    if task.task_type != TaskType::GenerateArtifact
        || task.document_id.as_deref() != Some(document_id)
    {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "generation_task_key_mismatch".into()),
        );
    }
    Ok(task)
}

fn start_background_generation<F>(
    state: AppState,
    snapshot_id: &str,
    document_id: &str,
    idempotency_key: &str,
    run: F,
) -> Result<TaskView, AppError>
where
    F: FnOnce(
            &AppState,
            &ContextSnapshotView,
            &TaskRow,
            &AttemptRow,
        ) -> Result<PersistedDraft, AppError>
        + Send
        + 'static,
{
    let task = start_task(&state, document_id, idempotency_key)?;
    // 回放先于快照/能力查询:既不重复模型,也不因原快照失效破坏幂等。
    if task.status != TaskStatus::Queued {
        return Ok(task.to_view());
    }
    let (task, attempt) = match begin_attempt(&state, task.clone()) {
        Ok(started) => started,
        Err(error) => {
            let current = get_generation_task(&state, &task.id)?;
            // queued 的竞争由 TaskService 的事务裁决;仅胜者启动线程。
            // capability 失败已收口为 failed,也返回可轮询的 TaskView。
            if error.code == CoreErrorCode::TaskNotRunning.code()
                || matches!(current.status, TaskStatus::Failed | TaskStatus::Canceled)
            {
                return Ok(current.to_view());
            }
            return Err(error);
        }
    };
    let snapshot_id = snapshot_id.to_string();
    let document_id = document_id.to_string();
    dispatch_generation(
        &state,
        task,
        attempt,
        move |state, task, attempt| {
            let snapshot = load_snapshot(state, &snapshot_id, &document_id)?;
            run(state, &snapshot, task, attempt).map(|_| ())
        },
        |worker| {
            std::thread::Builder::new()
                .name("kw-artifact-generation".into())
                .spawn(worker)
        },
    )
}

/// 模型端口内部有独立 rt.block_on:只能在专用 OS 线程执行,不能直接
/// 放到 async 命令/异步 executor 上。spawn 返回后即释放准备线程与 IPC。
fn dispatch_generation<F, S>(
    state: &AppState,
    task: TaskRow,
    attempt: AttemptRow,
    run: F,
    spawn: S,
) -> Result<TaskView, AppError>
where
    F: FnOnce(&AppState, &TaskRow, &AttemptRow) -> Result<(), AppError> + Send + 'static,
    S: FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<std::thread::JoinHandle<()>>,
{
    let worker_state = state.clone();
    let worker_task = task.clone();
    let worker_attempt = attempt.clone();
    let spawned = catch_generation(|| {
        spawn(Box::new(move || {
            if let Err(error) =
                execute_generation(&worker_state, &worker_task, &worker_attempt, || {
                    run(&worker_state, &worker_task, &worker_attempt)
                })
            {
                // 只记录稳定错误码,不记录 panic/model 正文。
                tracing::warn!(error_code = %error.code, "background generation failed");
            }
        }))
        .map_err(|_| AppError::internal("generation_thread_spawn_failed"))
    });
    if let Err(error) = spawned {
        let error = finish_generation::<()>(state, &task, &attempt, Err(error)).unwrap_err();
        let current = get_generation_task(state, &task.id)?;
        if matches!(current.status, TaskStatus::Queued | TaskStatus::Running) {
            return Err(error);
        }
        return Ok(current.to_view());
    }
    Ok(get_generation_task(state, &task.id)?.to_view())
}

/// 同键任务已终态:能找回产物就返回;否则明确拒绝(不重复花费模型调用)。
fn existing_if_finished(
    state: &AppState,
    task: &TaskRow,
) -> Result<Option<ArtifactView>, AppError> {
    if matches!(
        task.status,
        TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Canceled | TaskStatus::Interrupted
    ) {
        if let Some(view) = state.artifacts.find_artifact_by_task(&task.id)? {
            return Ok(Some(view));
        }
        return Err(
            AppError::new(CoreErrorCode::TaskNotRunning, crate::shared::new_trace_id())
                .with_detail("reason", "generation_task_already_finished".into()),
        );
    }
    // 同键请求仍在运行:拒绝并发第二条模型调用(不重复花费)。
    // 注意:刚创建的 Task 也是 queued,不能把 queued 当成"已在运行"。
    if task.status == TaskStatus::Running {
        return Err(
            AppError::new(CoreErrorCode::TaskNotRunning, crate::shared::new_trace_id())
                .with_detail("reason", "generation_in_progress".into()),
        );
    }
    Ok(None)
}

/// 重新生成的幂等回放:同键 Task 已成功且已产出目标 artifact 的版本时
/// 直接返回该版本;找不到版本说明任务失败或键被其他命令复用,明确拒绝。
fn existing_regeneration_if_finished(
    state: &AppState,
    task: &TaskRow,
    artifact_id: &str,
) -> Result<Option<ArtifactRegenerateResult>, AppError> {
    if matches!(
        task.status,
        TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Canceled | TaskStatus::Interrupted
    ) {
        if let Some(draft) = state
            .artifacts
            .find_regenerated_by_task(&task.id, artifact_id)?
        {
            return Ok(Some(ArtifactRegenerateResult {
                artifact: draft.artifact,
                version_id: draft.version_id,
            }));
        }
        return Err(
            AppError::new(CoreErrorCode::TaskNotRunning, crate::shared::new_trace_id())
                .with_detail("reason", "generation_task_already_finished".into()),
        );
    }
    if task.status == TaskStatus::Running {
        return Err(
            AppError::new(CoreErrorCode::TaskNotRunning, crate::shared::new_trace_id())
                .with_detail("reason", "generation_in_progress".into()),
        );
    }
    Ok(None)
}

fn begin_attempt(state: &AppState, task: TaskRow) -> Result<(TaskRow, AttemptRow), AppError> {
    let cap = catch_generation(|| {
        state.generation.capability(
            task.document_id
                .as_deref()
                .ok_or_else(|| AppError::internal("generation_document_missing"))?,
        )
    });
    let (provider_id, model_id) = cap
        .as_ref()
        .map(|cap| (cap.provider_id.clone(), cap.model_id.clone()))
        .unwrap_or((None, None));
    // 即使能力查询失败也先创建 attempt,通过原状态机落 failed,不遗留 queued。
    let (task, attempt) = state.tasks.begin_attempt(&task.id, provider_id, model_id)?;
    if let Err(error) = cap {
        return finish_generation(state, &task, &attempt, Err(error));
    }
    // 预建取消令牌;execute_generation 还会读取持久化的取消标志兜住竞态。
    state.tasks.cancel_token(&task.id);
    Ok((task, attempt))
}

fn catch_generation<T>(run: impl FnOnce() -> Result<T, AppError>) -> Result<T, AppError> {
    catch_unwind(AssertUnwindSafe(run))
        .unwrap_or_else(|_| Err(AppError::internal("generation_worker_panicked")))
}

fn execute_generation<T>(
    state: &AppState,
    task: &TaskRow,
    attempt: &AttemptRow,
    run: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    let result = catch_generation(|| {
        let cancel = state.tasks.cancel_token(&task.id);
        let current = get_generation_task(state, &task.id)?;
        if current.cancel_requested {
            cancel.cancel();
            return Err(AppError::new(
                CoreErrorCode::TaskCanceled,
                crate::shared::new_trace_id(),
            ));
        }
        if current.status != TaskStatus::Running
            || current.current_attempt_id.as_deref() != Some(attempt.id.as_str())
        {
            return Err(AppError::new(
                CoreErrorCode::TaskNotRunning,
                crate::shared::new_trace_id(),
            ));
        }
        run()
    });
    finish_generation(state, task, attempt, result)
}

fn finish_generation<T>(
    state: &AppState,
    task: &TaskRow,
    attempt: &AttemptRow,
    result: Result<T, AppError>,
) -> Result<T, AppError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            let current = get_generation_task(state, &task.id)?;
            // 不覆盖成功/取消等终态,也不触碰显式重试后的新 attempt。
            if current.status == TaskStatus::Running
                && current.current_attempt_id.as_deref() == Some(attempt.id.as_str())
            {
                if error.code == CoreErrorCode::TaskCanceled.code() && !current.cancel_requested {
                    // 端口独自取消不等于用户请求取消;原 tasks 契约禁止无请求 ack。
                    state.tasks.fail_task(&task.id, &attempt.id, &error.code)?;
                } else {
                    state
                        .tasks
                        .settle_generation_failure(&task.id, &attempt.id, &error.code)?;
                }
            }
            Err(error)
        }
    }
}

/// 已提交的产物版本(生成/重新生成共用)。
struct PersistedDraft {
    view: ArtifactView,
    version_id: String,
}

fn snapshot_blocks(
    state: &AppState,
    snapshot: &ContextSnapshotView,
) -> Result<Vec<ContentBlockView>, AppError> {
    let mut blocks = if snapshot.block_ids.is_empty() {
        match snapshot.parse_generation_id.as_deref() {
            Some(generation_id) => state.parse.list_generation_blocks(generation_id)?,
            None => Vec::new(),
        }
    } else {
        state.parse.get_blocks_by_ids(&snapshot.block_ids)?
    };
    blocks.sort_by_key(|block| block.ordinal.unwrap_or(i64::MAX));
    blocks.retain(|block| !block.text.trim().is_empty());
    Ok(blocks)
}

fn coverage_for(snapshot: &ContextSnapshotView, block_count: usize) -> CoverageReport {
    CoverageReport {
        complete: snapshot.coverage.scope_covered,
        regions: vec![CoverageRegion {
            label: snapshot.summary.name.clone(),
            block_count: block_count as i64,
        }],
    }
}

fn title_or(requested: Option<String>, fallback: String) -> String {
    let candidate = requested
        .map(|title| title.trim().to_string())
        .filter(|title| !title.is_empty())
        .unwrap_or(fallback);
    candidate.chars().take(GENERATION_TITLE_MAX_CHARS).collect()
}

/// 写产物前的最后一个取消检查点。
///
/// 「模型已返回、解析完成」与 `commit_ai_draft` 之间曾经没有任何检查:
/// 取消恰好落在这个窗口时,产物版本仍会写库,任务却因 `commit_result`
/// 的 cancel_requested 守卫显示为已取消——库里就多出一个"已取消任务
/// 产出的产物"。这里把所有提交前的路径统一收口到同一检查。
fn ensure_not_canceled(state: &AppState, task: &TaskRow) -> Result<(), AppError> {
    if state.tasks.cancel_token(&task.id).is_cancelled() {
        return Err(AppError::new(
            CoreErrorCode::TaskCanceled,
            crate::shared::new_trace_id(),
        ));
    }
    Ok(())
}

fn run_summary_generation(
    state: &AppState,
    snapshot: &ContextSnapshotView,
    task: &TaskRow,
    attempt: &AttemptRow,
    title: Option<String>,
    template: &str,
    target_artifact_id: Option<&str>,
) -> Result<PersistedDraft, AppError> {
    let blocks = snapshot_blocks(state, snapshot)?;
    let (blocks, truncated) = summary_materials(&blocks)?;
    let mut coverage = coverage_for(snapshot, blocks.len());
    if truncated {
        // 超出单次材料预算:显式缩小覆盖,UI 依据 coverage=false 提示,
        // 不把"部分材料"说成全文摘要。
        coverage.complete = false;
    }
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Generating);

    let request = crate::retrieval_ai::summary::build_summary_request_with_template(
        &task.id,
        &attempt.id,
        &snapshot.document_id,
        &snapshot.id,
        "zh-CN",
        &blocks,
        coverage.complete,
        template,
    );
    let cancel = state.tasks.cancel_token(&task.id);
    let generated = state.generation.generate(&request, &cancel)?;
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Validating);
    // 统一走容错解析:结构化失败时在解析器内降级为可读正文,
    // 不再先按 plain_text 分支截走结构化内容(会丢失引用)。
    let mut outcome = crate::retrieval_ai::summary::parse_summary_generation(
        generated.content,
        &blocks,
        coverage.clone(),
    )?;
    if truncated {
        let note = format!(
            "本次摘要只覆盖预算内的前 {} 个原文块,未覆盖全文",
            blocks.len()
        );
        outcome.content.open_questions.push(note.clone());
        outcome.notes.push(note);
    }
    if cancel.is_cancelled() {
        return Err(AppError::new(
            CoreErrorCode::TaskCanceled,
            crate::shared::new_trace_id(),
        ));
    }

    ensure_not_canceled(state, task)?;
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Committing);
    let template_label = match template {
        "study_note" => "学习笔记",
        "executive_summary" => "执行摘要",
        "faq" => "FAQ",
        "comparison" => "对比表",
        "timeline" => "时间线",
        _ => "摘要",
    };
    let artifact_title = title_or(
        title,
        format!("{template_label} · {}", snapshot.summary.name)
            .trim()
            .to_string(),
    );
    let content =
        serde_json::to_value(&outcome.content).map_err(|e| AppError::internal(e.to_string()))?;
    let persisted = if let Some(artifact_id) = target_artifact_id {
        let result = state.artifacts.append_ai_draft(
            artifact_id,
            &RegeneratedDraftInput {
                content,
                coverage,
                snapshot_id: Some(snapshot.id.clone()),
                task_id: Some(task.id.clone()),
            },
        )?;
        PersistedDraft {
            view: result.artifact,
            version_id: result.version_id,
        }
    } else {
        let view = state.artifacts.commit_ai_draft(
            &AiDraftCommitInput {
                document_id: snapshot.document_id.clone(),
                kind: "summary".into(),
                title: artifact_title,
                content,
                coverage,
                snapshot_id: Some(snapshot.id.clone()),
                task_id: Some(task.id.clone()),
                citations: Vec::new(),
            },
            &format!("{}:artifact", task.idempotency_key),
        )?;
        let version_id = view
            .current_version_id
            .clone()
            .ok_or_else(|| AppError::internal("artifact_version_missing"))?;
        PersistedDraft { view, version_id }
    };
    save_citations(
        state,
        snapshot,
        &persisted.version_id,
        outcome
            .citations
            .iter()
            .map(|draft| CitationDraftForCommit {
                claim_key: draft.claim_key.clone(),
                claim_kind: draft.claim_kind,
                content_block_id: draft.content_block_id.clone(),
                quoted_text: draft.quoted_text.clone(),
            }),
        &task.id,
    )?;
    complete_task(
        state,
        task,
        attempt,
        &persisted.view.id,
        &persisted.version_id,
        &outcome.notes,
    )?;
    // 重新读取:引用写入后 citation_count 与完整版本列表才是最新视图。
    let view = state.artifacts.get_artifact(&persisted.view.id, None)?;
    Ok(PersistedDraft {
        view,
        version_id: persisted.version_id,
    })
}

/// 模型未按目标 schema 输出时的可见产物兜底:
/// 把原始正文落为 summary.v1 的 needs_verification 段,任务成功、UI 可见,
/// 同时明确说明“未按题目/结构化格式返回”,不冒充题目或已验证事实。
fn commit_plain_text_fallback(
    state: &AppState,
    snapshot: &ContextSnapshotView,
    task: &TaskRow,
    attempt: &AttemptRow,
    title: Option<String>,
    text: &str,
    coverage: CoverageReport,
) -> Result<PersistedDraft, AppError> {
    let sections = crate::retrieval_ai::summary::sections_from_plain_text(text);
    if sections.is_empty() {
        return Err(crate::retrieval_ai::error::model_output_invalid(
            "plain_text_fallback_empty",
        ));
    }
    let content = SummaryContentV1 {
        schema_version: "summary.v1".into(),
        sections,
        open_questions: vec!["格式未完全校验:已保留可读正文;可重试生成结构化内容。".into()],
        coverage: coverage.clone(),
    };
    let content_json =
        serde_json::to_value(&content).map_err(|e| AppError::internal(e.to_string()))?;
    let artifact_title = title_or(
        title,
        format!("未校验草稿 · {}", snapshot.summary.name)
            .trim()
            .to_string(),
    );
    ensure_not_canceled(state, task)?;
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Committing);
    let view = state.artifacts.commit_ai_draft(
        &AiDraftCommitInput {
            document_id: snapshot.document_id.clone(),
            kind: "summary".into(),
            title: artifact_title,
            content: content_json,
            coverage,
            snapshot_id: Some(snapshot.id.clone()),
            task_id: Some(task.id.clone()),
            citations: Vec::new(),
        },
        &format!("{}:fallback", task.idempotency_key),
    )?;
    let version_id = view
        .current_version_id
        .clone()
        .ok_or_else(|| AppError::internal("artifact_version_missing"))?;
    complete_task(
        state,
        task,
        attempt,
        &view.id,
        &version_id,
        &["模型未按目标结构返回;已保存为未校验草稿".to_string()],
    )?;
    let view = state.artifacts.get_artifact(&view.id, None)?;
    Ok(PersistedDraft { view, version_id })
}

fn plain_text_of(value: &serde_json::Value) -> Option<String> {
    if value.get("schemaVersion").and_then(|v| v.as_str()) != Some("plain_text_fallback.v1") {
        return None;
    }
    value
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn run_quiz_generation(
    state: &AppState,
    snapshot: &ContextSnapshotView,
    task: &TaskRow,
    attempt: &AttemptRow,
    config: QuizGenerationConfig,
    title: Option<String>,
    target_artifact_id: Option<&str>,
) -> Result<PersistedDraft, AppError> {
    let (blocks, truncated) = quiz_materials(state, snapshot)?;
    let mut coverage = coverage_for(snapshot, blocks.len());
    if truncated {
        coverage.complete = false;
    }
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Generating);

    let cancel = state.tasks.cancel_token(&task.id);
    let generation_port: Arc<dyn crate::retrieval_ai::ports::StructuredGenerationPort> =
        Arc::clone(&state.generation);
    let adapter = Arc::new(
        QuizGenerationPortAdapter::new(generation_port, snapshot.document_id.clone())
            .with_cancel(cancel.clone()),
    );
    let generator = QuizGenerator::new(adapter);
    let quiz_context = QuizGenerationContext {
        task_id: task.id.clone(),
        attempt_id: attempt.id.clone(),
        snapshot_id: snapshot.id.clone(),
        locale: "zh-CN".into(),
        config,
        materials: QuizGenerationMaterials {
            document_version_id: snapshot.document_version_id.clone(),
            blocks: blocks
                .iter()
                .map(|block| QuizMaterialBlock {
                    block_id: block.id.clone(),
                    ordinal: block.ordinal.unwrap_or(0),
                    text: block.text.clone(),
                })
                .collect(),
        },
    };
    let outcome = generator.generate(&quiz_context)?;
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Validating);
    if let Some(text) = outcome.plain_text.clone() {
        return commit_plain_text_fallback(state, snapshot, task, attempt, title, &text, coverage);
    }

    // WBS-4A 验收:无效题不写入产物;全部无效则明确失败(不展示坏题)。
    let valid_questions: Vec<_> = outcome
        .report
        .content
        .questions
        .into_iter()
        .filter(|question| question.validity == QuestionValidity::Valid)
        .collect();
    if valid_questions.is_empty() {
        return Err(crate::quiz::validation::quiz_question_invalid(
            "no_valid_questions",
        ));
    }
    if cancel.is_cancelled() {
        return Err(AppError::new(
            CoreErrorCode::TaskCanceled,
            crate::shared::new_trace_id(),
        ));
    }
    let content = QuizContentV1 {
        schema_version: crate::quiz::models::QUIZ_SCHEMA_VERSION.into(),
        questions: valid_questions,
    };
    ensure_not_canceled(state, task)?;
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Committing);
    let artifact_title = title_or(
        title,
        format!("练习题 · {}", snapshot.summary.name)
            .trim()
            .to_string(),
    );
    let content_json =
        serde_json::to_value(&content).map_err(|e| AppError::internal(e.to_string()))?;
    let persisted = if let Some(artifact_id) = target_artifact_id {
        let result = state.artifacts.append_ai_draft(
            artifact_id,
            &RegeneratedDraftInput {
                content: content_json,
                coverage,
                snapshot_id: Some(snapshot.id.clone()),
                task_id: Some(task.id.clone()),
            },
        )?;
        PersistedDraft {
            view: result.artifact,
            version_id: result.version_id,
        }
    } else {
        let view = state.artifacts.commit_ai_draft(
            &AiDraftCommitInput {
                document_id: snapshot.document_id.clone(),
                kind: "quiz".into(),
                title: artifact_title,
                content: content_json,
                coverage,
                snapshot_id: Some(snapshot.id.clone()),
                task_id: Some(task.id.clone()),
                citations: Vec::new(),
            },
            &format!("{}:artifact", task.idempotency_key),
        )?;
        let version_id = view
            .current_version_id
            .clone()
            .ok_or_else(|| AppError::internal("artifact_version_missing"))?;
        PersistedDraft { view, version_id }
    };
    let citation_drafts = content.questions.iter().flat_map(|question| {
        question
            .citations
            .iter()
            .map(move |citation| CitationDraftForCommit {
                claim_key: question.id.clone(),
                claim_kind: ClaimKind::DocumentFact,
                content_block_id: citation.block_id.clone(),
                quoted_text: citation.quoted_text.clone(),
            })
    });
    save_citations(
        state,
        snapshot,
        &persisted.version_id,
        citation_drafts,
        &task.id,
    )?;

    let mut notes = Vec::new();
    if outcome.report.invalid_count > 0 {
        notes.push(format!(
            "{} 道题未通过结构校验,已从产物中剔除",
            outcome.report.invalid_count
        ));
    }
    if outcome.model_partial {
        notes.push("模型自报输出不完整,请检查题目内容".into());
    }
    if truncated {
        notes.push(format!(
            "本次出题只使用预算内的前 {} 个原文块,未覆盖全文",
            blocks.len()
        ));
    }
    complete_task(
        state,
        task,
        attempt,
        &persisted.view.id,
        &persisted.version_id,
        &notes,
    )?;
    let view = state.artifacts.get_artifact(&persisted.view.id, None)?;
    Ok(PersistedDraft {
        view,
        version_id: persisted.version_id,
    })
}

fn run_quiz_v2_generation(
    state: &AppState,
    snapshot: &ContextSnapshotView,
    task: &TaskRow,
    attempt: &AttemptRow,
    config: QuizV2GenerationConfig,
    title: Option<String>,
) -> Result<PersistedDraft, AppError> {
    // 分题型数量:显式指定时总数 = 各题型之和;否则按 count 自动分配。
    let config = config.normalized().map_err(|reason| {
        AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
            .with_detail("reason", reason.into())
    })?;
    let types = config.types.clone();
    let (blocks, truncated) = quiz_materials(state, snapshot)?;
    let mut coverage = coverage_for(snapshot, blocks.len());
    if truncated {
        coverage.complete = false;
    }
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Generating);
    let cancel = state.tasks.cancel_token(&task.id);
    let type_list = types
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let question = format!(
        "Generate exactly {} mixed questions using these types: {}. Difficulty mix: {}. Style: {}. \
         Every question needs a verbatim citation and explanation; no duplicate prompts.",
        config.count,
        type_list,
        config
            .difficulties
            .iter()
            .map(|d| d.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        if config.style.trim().is_empty() {
            "concept boundary"
        } else {
            config.style.trim()
        }
    );
    let request = StructuredGenerationRequest {
        task_id: task.id.clone(),
        attempt_id: attempt.id.clone(),
        document_id: snapshot.document_id.clone(),
        snapshot_id: snapshot.id.clone(),
        schema_version: QUIZ_V2_SCHEMA_VERSION.to_string(),
        schema: quiz_v2_schema(),
        instruction_version: QUIZ_V2_INSTRUCTION_VERSION.to_string(),
        task_kind: TaskKind::QuizV2,
        inputs: blocks
            .iter()
            .map(|block| GenerationInput {
                source_id: block.id.clone(),
                role: SourceRole::Original,
                text: block.text.clone(),
            })
            .collect(),
        locale: "zh-CN".into(),
        question,
        history: Vec::new(),
        history_summary: None,
        attachments: Vec::new(),
        document_map: None,
        scope: crate::model_privacy::gateway::EgressScope::Task,
    };
    let generated = state.generation.generate(&request, &cancel)?;
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Validating);
    if let Some(text) = plain_text_of(&generated.content) {
        return commit_plain_text_fallback(state, snapshot, task, attempt, title, &text, coverage);
    }
    let raw =
        serde_json::to_string(&generated.content).map_err(|e| AppError::internal(e.to_string()))?;
    let parsed: QuizContentV2 = parse_quiz_v2(&raw)?;
    let materials: Vec<(String, String)> = blocks
        .iter()
        .map(|b| (b.id.clone(), b.text.clone()))
        .collect();
    let (content, invalid_count) = validate_quiz_v2(parsed, &materials);
    if cancel.is_cancelled() {
        return Err(AppError::new(
            CoreErrorCode::TaskCanceled,
            crate::shared::new_trace_id(),
        ));
    }
    ensure_not_canceled(state, task)?;
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Committing);
    let artifact_title = title_or(
        title,
        format!("练习题 · {}", snapshot.summary.name)
            .trim()
            .to_string(),
    );
    let content_json =
        serde_json::to_value(&content).map_err(|e| AppError::internal(e.to_string()))?;
    let view = state.artifacts.commit_ai_draft(
        &AiDraftCommitInput {
            document_id: snapshot.document_id.clone(),
            kind: "quiz".into(),
            title: artifact_title,
            content: content_json,
            coverage,
            snapshot_id: Some(snapshot.id.clone()),
            task_id: Some(task.id.clone()),
            citations: Vec::new(),
        },
        &format!("{}:artifact", task.idempotency_key),
    )?;
    let version_id = view
        .current_version_id
        .clone()
        .ok_or_else(|| AppError::internal("artifact_version_missing"))?;
    let citation_drafts = content
        .questions
        .iter()
        .filter(|question| question.validity == QuestionValidity::Valid)
        .flat_map(|question| {
            question
                .citations
                .iter()
                .map(move |citation| CitationDraftForCommit {
                    claim_key: question.id.clone(),
                    claim_kind: ClaimKind::DocumentFact,
                    content_block_id: citation.block_id.clone(),
                    quoted_text: citation.quoted_text.clone(),
                })
        });
    save_citations(state, snapshot, &version_id, citation_drafts, &task.id)?;
    let mut notes = Vec::new();
    if invalid_count > 0 {
        notes.push(format!(
            "{} 道题无法可靠判分,已保留并标注原因,不参与作答",
            invalid_count
        ));
    }
    if truncated {
        notes.push(format!(
            "本次出题只使用预算内的前 {} 个原文块,未覆盖全文",
            blocks.len()
        ));
    }
    complete_task(state, task, attempt, &view.id, &version_id, &notes)?;
    let view = state.artifacts.get_artifact(&view.id, None)?;
    Ok(PersistedDraft { view, version_id })
}

fn quiz_materials(
    state: &AppState,
    snapshot: &ContextSnapshotView,
) -> Result<(Vec<ContentBlockView>, bool), AppError> {
    let blocks = snapshot_blocks(state, snapshot)?;
    if blocks.is_empty() {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "quiz_material_no_evidence".into()),
        );
    }
    let mut selected = Vec::new();
    let mut chars = 0usize;
    let mut truncated = false;
    for block in blocks {
        let len = block.text.chars().count();
        if selected.len() >= MAX_QUIZ_BLOCKS || chars + len > MAX_QUIZ_MATERIAL_CHARS {
            truncated = true;
            break;
        }
        chars += len;
        selected.push(block);
    }
    Ok((selected, truncated))
}

fn save_citations(
    state: &AppState,
    snapshot: &ContextSnapshotView,
    version_id: &str,
    drafts: impl Iterator<Item = CitationDraftForCommit>,
    task_id: &str,
) -> Result<(), AppError> {
    let version_id = version_id.to_string();
    let citations: Vec<CitationInput> = drafts
        .map(|draft| CitationInput {
            claim_key: draft.claim_key,
            claim_kind: draft.claim_kind,
            source_type: CitationSourceType::ContentBlock,
            source_id: Some(draft.content_block_id.clone()),
            anchor_id: None,
            content_block_id: Some(draft.content_block_id),
            quoted_text: draft.quoted_text,
            locator: None,
            support_status: None,
            support_source: None,
        })
        .collect();
    if citations.is_empty() {
        return Ok(());
    }
    let request = SaveCitationsRequest {
        owner_type: CitationOwnerType::ArtifactVersion,
        owner_id: version_id,
        document_id: snapshot.document_id.clone(),
        document_version_id: snapshot.document_version_id.clone(),
        citations,
        idempotency_key: format!("gen:{}", task_id),
    };
    state.evidence.save_citations(&request)?;
    Ok(())
}

fn complete_task(
    state: &AppState,
    task: &TaskRow,
    attempt: &AttemptRow,
    artifact_id: &str,
    version_id: &str,
    notes: &[String],
) -> Result<TaskView, AppError> {
    let result_json = serde_json::json!({
        "artifactId": artifact_id,
        "versionId": version_id,
        "notes": notes,
    });
    let outcome = TaskOutcome::Answered;
    let result_ref = TaskResultRef {
        kind: TaskResultRefKind::ArtifactVersion,
        id: version_id.to_string(),
    };
    let committed = state.tasks.commit_result(
        &task.id,
        &attempt.id,
        result_json,
        outcome,
        Some(result_ref),
        |_conn, _task| Ok(()),
    )?;
    Ok(match committed {
        crate::persistence::task::CommitOutcome::Committed(task)
        | crate::persistence::task::CommitOutcome::AlreadyCommitted(task) => task.to_view(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval_ai::ports::{ModelCapabilityView, StructuredGenerationPort};
    use crate::shared::{DomainEvent, DomainEventType, EventSink};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(10);

    struct CapabilityStub {
        behavior: &'static str,
        calls: AtomicUsize,
    }

    impl StructuredGenerationPort for CapabilityStub {
        fn capability(&self, _: &str) -> Result<ModelCapabilityView, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behavior {
                "error" => return Err(crate::retrieval_ai::error::model_unavailable()),
                "panic" => panic!("capability panic fixture"),
                _ => {}
            }
            Ok(ModelCapabilityView {
                generation_available: true,
                generation_authorized: true,
                provider_id: Some("test-provider".into()),
                model_id: Some("test-model".into()),
            })
        }

        fn generate(
            &self,
            _: &StructuredGenerationRequest,
            _: &tokio_util::sync::CancellationToken,
        ) -> Result<crate::retrieval_ai::models::StructuredGenerationResult, AppError> {
            panic!("contract tests must not contact a model")
        }
    }

    struct TerminalEvents(mpsc::Sender<String>);
    impl EventSink for TerminalEvents {
        fn emit(&self, event: DomainEvent) {
            if event.event_type == DomainEventType::TaskChanged
                && matches!(
                    event.payload.get("status").and_then(|v| v.as_str()),
                    Some("succeeded" | "failed" | "canceled" | "interrupted")
                )
            {
                let _ = self.0.send(event.aggregate_id);
            }
        }
    }

    fn fixture(behavior: &'static str) -> (AppState, Arc<CapabilityStub>, mpsc::Receiver<String>) {
        // 仅在唯一的临时目录创建测试库,绝不打开应用的真实用户数据。
        let dir = std::env::temp_dir().join(format!("kw-generation-{}", uuid::Uuid::now_v7()));
        let (tx, rx) = mpsc::channel();
        let mut state =
            super::super::bootstrap_with(&dir, &dir.join("config"), Arc::new(TerminalEvents(tx)))
                .unwrap();
        let port = Arc::new(CapabilityStub {
            behavior,
            calls: AtomicUsize::new(0),
        });
        state.generation = port.clone();
        state.db.write(|conn| {
            conn.execute_batch(
                r#"INSERT INTO document(id, display_name, format, current_version_id, created_at, updated_at)
                   VALUES ('d', 'Test document', 'markdown', 'v', 1, 1);
                   INSERT INTO document_version(id, document_id, fingerprint_status, confirmed, created_at)
                   VALUES ('v', 'd', 'ready', 1, 1);
                   INSERT INTO context_snapshot(id, document_id, document_version_id, scope_kind,
                     scope_json, coverage_json, canonical_hash, idempotency_key, created_at)
                   VALUES ('s', 'd', 'v', 'sources', '{"kind":"sources","sources":[]}',
                     '{"schemaVersion":"context_coverage.v1","parseState":"complete","scopeCovered":true,
                       "completedRegions":[],"partialRegions":[],"missingRegions":[],"notes":[]}',
                     'fixture', 'snapshot-fixture', 1);"#,
            ).map_err(|e| crate::persistence::db_app_error(&e))
        }).unwrap();
        (state, port, rx)
    }

    fn draft(state: &AppState, task: &TaskRow) -> PersistedDraft {
        let view = state.artifacts.commit_ai_draft(
            &AiDraftCommitInput {
                document_id: "d".into(),
                kind: "summary".into(),
                title: "Test summary".into(),
                content: serde_json::json!({
                    "schemaVersion": "summary.v1",
                    "sections": [{"id":"section", "title":"Summary", "segments":[{
                        "id":"segment", "kind":"needs_verification", "text":"Test draft", "citationIds":[]
                    }]}],
                    "openQuestions": [],
                    "coverage": {"complete":true,"regions":[]}
                }),
                coverage: CoverageReport { complete: true, regions: vec![] },
                snapshot_id: Some("s".into()),
                task_id: Some(task.id.clone()),
                citations: vec![],
            },
            &format!("{}:artifact", task.idempotency_key),
        ).unwrap();
        PersistedDraft {
            version_id: view.current_version_id.clone().unwrap(),
            view,
        }
    }

    fn assert_attempt(state: &AppState, task_id: &str, expected_status: &str) {
        let rows: Vec<String> = state
            .db
            .read(|conn| {
                let mut stmt = conn
                    .prepare("SELECT status FROM task_attempt WHERE task_id=?1")
                    .map_err(|e| crate::persistence::db_app_error(&e))?;
                let rows = stmt
                    .query_map([task_id], |row| row.get(0))
                    .map_err(|e| crate::persistence::db_app_error(&e))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| crate::persistence::db_app_error(&e))?;
                Ok(rows)
            })
            .unwrap();
        assert_eq!(rows, vec![expected_status]);
    }

    #[test]
    fn background_returns_before_completion_and_same_key_replays_without_model() {
        let (state, port, terminal) = fixture("ready");
        let (staged_tx, staged_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let submit_state = state.clone();
        let rt = tokio::runtime::Runtime::new().unwrap();
        // 与命令相同的 async -> blocking 准备路径;模型主体必须逃离 Tokio。
        let task = rt.block_on(async {
            tokio::task::spawn_blocking(move || {
                start_background_generation(
                    submit_state,
                    "s",
                    "d",
                    "same-key",
                    move |state, _, task, attempt| {
                        assert!(tokio::runtime::Handle::try_current().is_err());
                        let draft = draft(state, task);
                        staged_tx.send(draft.view.id.clone()).unwrap();
                        release_rx.recv_timeout(WAIT).unwrap();
                        complete_task(
                            state,
                            task,
                            attempt,
                            &draft.view.id,
                            &draft.version_id,
                            &[],
                        )?;
                        Ok(draft)
                    },
                )
            })
            .await
            .unwrap()
            .unwrap()
        });
        let artifact_id = staged_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        let pending = generation_status(&state, &task.id).unwrap();
        assert!(pending.artifact.is_none(), "提交成功前不能暴露半成品");
        assert!(serde_json::to_value(pending).unwrap()["artifact"].is_null());
        // 生成还在等待时 document_get 的服务与普通读照常返回。
        assert!(state.documents.get_document("d").is_ok());
        assert!(state.artifacts.get_artifact(&artifact_id, None).is_ok());
        let replay = start_background_generation(
            state.clone(),
            "missing-snapshot",
            "d",
            "same-key",
            |_, _, _, _| panic!("running replay must not generate"),
        )
        .unwrap();
        assert_eq!(replay.id, task.id);
        assert_eq!(replay.status, TaskStatus::Running);
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
        release_tx.send(()).unwrap();
        assert_eq!(terminal.recv_timeout(WAIT).unwrap(), task.id);
        let done = generation_status(&state, &task.id).unwrap();
        assert_eq!(done.task.status, TaskStatus::Succeeded);
        assert_eq!(done.artifact.unwrap().id, artifact_id);
        let replay = start_background_generation(
            state.clone(),
            "missing-snapshot",
            "d",
            "same-key",
            |_, _, _, _| panic!("terminal replay must not generate"),
        )
        .unwrap();
        assert_eq!(replay.id, task.id);
        assert_eq!(replay.status, TaskStatus::Succeeded);
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
        assert_attempt(&state, &task.id, "succeeded");
    }

    #[test]
    fn concurrent_same_key_has_only_one_attempt_and_worker() {
        let (state, _, terminal) = fixture("ready");
        let starts = Arc::new(std::sync::Barrier::new(8));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];
        for _ in 0..8 {
            let state = state.clone();
            let starts = starts.clone();
            let calls = calls.clone();
            handles.push(std::thread::spawn(move || {
                starts.wait();
                start_background_generation(state, "s", "d", "concurrent", move |_, _, _, _| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(crate::retrieval_ai::error::model_unavailable())
                })
                .unwrap()
            }));
        }
        let tasks: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(tasks.iter().all(|task| task.id == tasks[0].id));
        assert_eq!(terminal.recv_timeout(WAIT).unwrap(), tasks[0].id);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_attempt(&state, &tasks[0].id, "failed");
    }

    #[test]
    fn capability_error_and_panic_are_visible_failed_tasks_not_queued() {
        for (behavior, code) in [("error", "MODEL_UNAVAILABLE"), ("panic", "INTERNAL")] {
            let (state, port, _) = fixture(behavior);
            let task = start_background_generation(
                state.clone(),
                "s",
                "d",
                "cap-failure",
                |_, _, _, _| panic!("failed capability must not launch generation"),
            )
            .unwrap();
            assert_eq!(task.status, TaskStatus::Failed);
            assert_eq!(task.error.unwrap().code, code);
            assert!(generation_status(&state, &task.id)
                .unwrap()
                .artifact
                .is_none());
            let replay = start_background_generation(
                state.clone(),
                "s",
                "d",
                "cap-failure",
                |_, _, _, _| panic!("failed replay must not generate"),
            )
            .unwrap();
            assert_eq!(replay.id, task.id);
            assert_eq!(port.calls.load(Ordering::SeqCst), 1);
            assert_attempt(&state, &task.id, "failed");
        }
    }

    #[test]
    fn spawn_failure_and_panic_settle_the_owned_attempt() {
        let (state, _, _) = fixture("ready");
        for (key, panic_spawn) in [("spawn-error", false), ("spawn-panic", true)] {
            let task = start_task(&state, "d", key).unwrap();
            let (task, attempt) = begin_attempt(&state, task).unwrap();
            let view = dispatch_generation(
                &state,
                task,
                attempt,
                |_, _, _| panic!("spawn failure must never run worker"),
                move |_| {
                    if panic_spawn {
                        panic!("spawn fixture panic");
                    }
                    Err(std::io::Error::other("spawn fixture failure"))
                },
            )
            .unwrap();
            assert_eq!(view.status, TaskStatus::Failed);
            assert!(view.error.is_some());
            assert_attempt(&state, &view.id, "failed");
        }
    }

    #[test]
    fn worker_panic_and_missing_snapshot_are_terminal_failures() {
        let (state, _, terminal) = fixture("ready");
        for snapshot in ["s", "missing"] {
            let task = start_background_generation(
                state.clone(),
                snapshot,
                "d",
                snapshot,
                |_, _, _, _| panic!("generation fixture panic"),
            )
            .unwrap();
            assert_eq!(terminal.recv_timeout(WAIT).unwrap(), task.id);
            let done = generation_status(&state, &task.id).unwrap();
            assert_eq!(done.task.status, TaskStatus::Failed);
            assert!(done.task.error.is_some());
            assert!(done.artifact.is_none());
            assert_attempt(&state, &task.id, "failed");
        }
    }

    #[test]
    fn cancellation_keeps_queued_and_running_ack_contracts() {
        let (state, _, _) = fixture("ready");
        let queued = start_task(&state, "d", "queued-cancel").unwrap();
        assert_eq!(
            cancel_generation(&state, &queued.id).unwrap().status,
            TaskStatus::Canceled
        );
        assert_eq!(
            cancel_generation(&state, &queued.id).unwrap_err().code,
            "TASK_NOT_RUNNING"
        );
        let task = start_task(&state, "d", "running-cancel").unwrap();
        // 故意不预建 token,覆盖取消发生在 begin_attempt 与 token 注册之间的窗口。
        let (task, attempt) = state.tasks.begin_attempt(&task.id, None, None).unwrap();
        let requested = cancel_generation(&state, &task.id).unwrap();
        assert_eq!(requested.status, TaskStatus::Running);
        assert!(requested.cancel_requested);
        let error = execute_generation::<()>(&state, &task, &attempt, || {
            panic!("canceled task must not contact model")
        })
        .unwrap_err();
        assert_eq!(error.code, "TASK_CANCELED");
        let done = generation_status(&state, &task.id).unwrap();
        assert_eq!(done.task.status, TaskStatus::Canceled);
        assert!(done.artifact.is_none());
        assert_attempt(&state, &task.id, "canceled");
        // 没有取消请求的端口错误不得强行 cancel_ack,也不得遗留 running。
        let task = start_task(&state, "d", "provider-cancel").unwrap();
        let (task, attempt) = begin_attempt(&state, task).unwrap();
        let _ = execute_generation::<()>(&state, &task, &attempt, || {
            Err(AppError::new(CoreErrorCode::TaskCanceled, "fixture"))
        });
        assert_eq!(
            generation_status(&state, &task.id).unwrap().task.status,
            TaskStatus::Failed
        );
        assert_attempt(&state, &task.id, "failed");
    }

    #[test]
    fn running_worker_observes_cancel_token_and_acknowledges_without_artifact() {
        let (state, _, terminal) = fixture("ready");
        let (entered_tx, entered_rx) = mpsc::channel();
        let task = start_background_generation(
            state.clone(),
            "s",
            "d",
            "active-cancel",
            move |state, _, task, _| {
                let cancel = state.tasks.cancel_token(&task.id);
                let rt = tokio::runtime::Runtime::new().unwrap();
                entered_tx.send(()).unwrap();
                rt.block_on(async {
                    tokio::time::timeout(WAIT, cancel.cancelled())
                        .await
                        .unwrap();
                });
                Err(AppError::new(CoreErrorCode::TaskCanceled, "fixture"))
            },
        )
        .unwrap();
        entered_rx.recv_timeout(WAIT).unwrap();
        assert!(
            cancel_generation(&state, &task.id)
                .unwrap()
                .cancel_requested
        );
        assert_eq!(terminal.recv_timeout(WAIT).unwrap(), task.id);
        let done = generation_status(&state, &task.id).unwrap();
        assert_eq!(done.task.status, TaskStatus::Canceled);
        assert!(done.artifact.is_none());
        assert_attempt(&state, &task.id, "canceled");
    }

    #[test]
    fn generation_poll_and_cancel_reject_other_task_types_and_key_reuse() {
        let (state, _, _) = fixture("ready");
        let other = state
            .tasks
            .create_task(CreateTask {
                document_id: Some("d".into()),
                task_type: TaskType::AskQuestion,
                idempotency_key: "other-task".into(),
                phase: TaskPhase::Waiting,
            })
            .unwrap();
        assert_eq!(
            generation_status(&state, &other.id).unwrap_err().code,
            "INVALID_REQUEST"
        );
        assert_eq!(
            cancel_generation(&state, &other.id).unwrap_err().code,
            "INVALID_REQUEST"
        );
        assert_eq!(
            start_task(&state, "d", "other-task").unwrap_err().code,
            "INVALID_REQUEST"
        );
        assert_eq!(
            generation_status(&state, "missing").unwrap_err().code,
            "NOT_FOUND"
        );
        let task = start_task(&state, "d", "document-key").unwrap();
        assert_eq!(
            start_task(&state, "another-document", "document-key")
                .unwrap_err()
                .code,
            "INVALID_REQUEST"
        );
        let (task, attempt) = begin_attempt(&state, task).unwrap();
        let draft = draft(&state, &task);
        let _ =
            finish_generation::<()>(&state, &task, &attempt, Err(AppError::internal("fixture")));
        assert!(state.artifacts.get_artifact(&draft.view.id, None).is_ok());
        assert!(generation_status(&state, &task.id)
            .unwrap()
            .artifact
            .is_none());
    }
}
