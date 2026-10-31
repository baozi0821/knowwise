//! 翻译命令(desktop feature)。
//!
//! 整篇翻译是 150–300s 级长任务:提交只做参数校验与任务创建,立即返回
//! `TaskView`;真实翻译在专用 OS 线程执行(模型端口内部 `block_on` 不能跑在
//   异步 executor 上),由 `translation_get` 轮询与 `translation_cancel` 取消。

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::app_shell::AppState;
use crate::artifacts::models::ArtifactView;
use crate::context::models::ContextSnapshotView;
use crate::persistence::task::{AttemptRow, CreateTask, TaskRow};
use crate::shared::dto::{
    TaskOutcome, TaskPhase, TaskResultRef, TaskResultRefKind, TaskStatus, TaskType, TaskView,
};
use crate::shared::{AppError, CoreErrorCode};

use super::models::TranslationGenerateRequest;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationGetRequest {
    pub task_id: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationGetResult {
    pub task: TaskView,
    /// 只有 succeeded 才返回产物;其余状态为 null,不暴露半成品。
    pub artifact: Option<ArtifactView>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationCancelRequest {
    pub task_id: String,
}

/// `translation_generate`:立即返回可轮询的 TaskView,不等待模型。
#[tauri::command]
pub async fn translation_generate(
    state: tauri::State<'_, AppState>,
    request: TranslationGenerateRequest,
) -> Result<TaskView, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let snapshot = load_snapshot(&state, &request.snapshot_id, &request.document_id)?;
        if request.target_language.trim().is_empty() || request.target_language.chars().count() > 32
        {
            return Err(AppError::new(
                CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "invalid_target_language".into()));
        }
        let task = state.tasks.create_task(CreateTask {
            document_id: Some(request.document_id.clone()),
            task_type: TaskType::GenerateArtifact,
            idempotency_key: request.idempotency_key.clone(),
            phase: TaskPhase::Waiting,
        })?;
        if task.task_type != TaskType::GenerateArtifact
            || task.document_id.as_deref() != Some(request.document_id.as_str())
        {
            return Err(AppError::new(
                CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "generation_task_key_mismatch".into()));
        }
        // 幂等:同键任务已存在时直接回放,不重复花费模型调用。
        if task.status != TaskStatus::Queued {
            return Ok(task.to_view());
        }
        let (task, attempt) = {
            let task_id = task.id.clone();
            match begin_attempt(&state, task) {
                Ok(started) => started,
                Err(error) => {
                    // 并发同键:TaskService 事务已裁决;终态任务仍返回可轮询视图。
                    let current = get_task(&state, &task_id)?;
                    if error.code == CoreErrorCode::TaskNotRunning.code()
                        || matches!(current.status, TaskStatus::Failed | TaskStatus::Canceled)
                    {
                        return Ok(current.to_view());
                    }
                    return Err(error);
                }
            }
        };
        dispatch(&state, task, attempt, snapshot, request)
    })
    .await
    .map_err(|_| AppError::internal("translation_prepare_join_failed"))?
}

/// `translation_get`:只读轮询本次对照任务;未完成时不返回产物。
#[tauri::command]
pub async fn translation_get(
    state: tauri::State<'_, AppState>,
    request: TranslationGetRequest,
) -> Result<TranslationGetResult, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let task = get_task(&state, &request.task_id)?;
        let artifact = if task.status == TaskStatus::Succeeded {
            state.artifacts.find_artifact_by_task(&task.id)?
        } else {
            None
        };
        Ok(TranslationGetResult {
            task: task.to_view(),
            artifact,
        })
    })
    .await
    .map_err(|_| AppError::internal("translation_get_join_failed"))?
}

/// `translation_cancel`:沿用 tasks 的两阶段取消(queued 直接取消,
/// running 置请求位并唤醒取消令牌)。
#[tauri::command]
pub async fn translation_cancel(
    state: tauri::State<'_, AppState>,
    request: TranslationCancelRequest,
) -> Result<TaskView, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let task = get_task(&state, &request.task_id)?;
        let task = state.tasks.request_cancel(&task.id, task.state_version)?;
        Ok(task.to_view())
    })
    .await
    .map_err(|_| AppError::internal("translation_cancel_join_failed"))?
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

fn get_task(state: &AppState, task_id: &str) -> Result<TaskRow, AppError> {
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

fn begin_attempt(state: &AppState, task: TaskRow) -> Result<(TaskRow, AttemptRow), AppError> {
    let cap = catch_unwind(AssertUnwindSafe(|| {
        state.generation.capability(
            task.document_id
                .as_deref()
                .ok_or_else(|| AppError::internal("generation_document_missing"))?,
        )
    }))
    .unwrap_or_else(|_| Err(AppError::internal("generation_worker_panicked")));
    let (provider_id, model_id) = cap
        .as_ref()
        .map(|cap| (cap.provider_id.clone(), cap.model_id.clone()))
        .unwrap_or((None, None));
    // 能力查询失败也先建 attempt,通过状态机落 failed,不遗留 queued。
    let (task, attempt) = state.tasks.begin_attempt(&task.id, provider_id, model_id)?;
    if let Err(error) = cap {
        let _ = state.tasks.fail_task(&task.id, &attempt.id, &error.code);
        return Err(error);
    }
    state.tasks.cancel_token(&task.id);
    Ok((task, attempt))
}

fn dispatch(
    state: &AppState,
    task: TaskRow,
    attempt: AttemptRow,
    snapshot: ContextSnapshotView,
    request: TranslationGenerateRequest,
) -> Result<TaskView, AppError> {
    let worker_state = state.clone();
    let worker_task = task.clone();
    let worker_attempt = attempt.clone();
    let spawned = catch_unwind(AssertUnwindSafe(|| {
        std::thread::Builder::new()
            .name("kw-translation".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    run_translation(
                        &worker_state,
                        &worker_task,
                        &worker_attempt,
                        &snapshot,
                        &request,
                    )
                }))
                .unwrap_or_else(|_| Err(AppError::internal("translation_worker_panicked")));
                if let Err(error) = finish(&worker_state, &worker_task, &worker_attempt, result) {
                    tracing::warn!(error_code = %error.code, "background translation failed");
                }
            })
    }));
    if spawned.is_err() {
        let error = AppError::internal("translation_thread_spawn_failed");
        let _ = finish::<ArtifactView>(state, &task, &attempt, Err(error.clone()));
        let current = get_task(state, &task.id)?;
        if matches!(current.status, TaskStatus::Queued | TaskStatus::Running) {
            return Err(error);
        }
        return Ok(current.to_view());
    }
    Ok(get_task(state, &task.id)?.to_view())
}

fn run_translation(
    state: &AppState,
    task: &TaskRow,
    attempt: &AttemptRow,
    snapshot: &ContextSnapshotView,
    request: &TranslationGenerateRequest,
) -> Result<ArtifactView, AppError> {
    let cancel = state.tasks.cancel_token(&task.id);
    let current = get_task(state, &task.id)?;
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
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Generating);
    let view = state.translation.translate_with_cancel(
        &request.document_id,
        snapshot,
        &request.target_language,
        request.style.as_deref(),
        &request.glossary,
        request.title.clone(),
        Some(task.id.as_str()),
        &cancel,
    )?;
    if cancel.is_cancelled() {
        // 取消可能发生在最后一次模型调用返回之后:不把已算出的结果当作成功。
        return Err(AppError::new(
            CoreErrorCode::TaskCanceled,
            crate::shared::new_trace_id(),
        ));
    }
    commit_success(state, task, attempt, &view)?;
    Ok(view)
}

fn commit_success(
    state: &AppState,
    task: &TaskRow,
    attempt: &AttemptRow,
    view: &ArtifactView,
) -> Result<(), AppError> {
    let _ = state
        .tasks
        .set_phase(&task.id, &attempt.id, TaskPhase::Committing);
    let version_id = view
        .current_version_id
        .clone()
        .ok_or_else(|| AppError::internal("artifact_version_missing"))?;
    let result_json = serde_json::json!({
        "artifactId": view.id,
        "versionId": version_id,
    });
    state.tasks.commit_result(
        &task.id,
        &attempt.id,
        result_json,
        TaskOutcome::Answered,
        Some(TaskResultRef {
            kind: TaskResultRefKind::ArtifactVersion,
            id: version_id,
        }),
        |_conn, _task| Ok(()),
    )?;
    Ok(())
}

fn finish<T>(
    state: &AppState,
    task: &TaskRow,
    attempt: &AttemptRow,
    result: Result<T, AppError>,
) -> Result<T, AppError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Ok(current) = get_task(state, &task.id) {
                if current.status == TaskStatus::Running
                    && current.current_attempt_id.as_deref() == Some(attempt.id.as_str())
                {
                    if error.code == CoreErrorCode::TaskCanceled.code() && !current.cancel_requested
                    {
                        // 端口单独取消不等于用户请求取消:按失败处理,不伪造 ack。
                        state.tasks.fail_task(&task.id, &attempt.id, &error.code)?;
                    } else {
                        state.tasks.settle_generation_failure(
                            &task.id,
                            &attempt.id,
                            &error.code,
                        )?;
                    }
                }
            }
            Err(error)
        }
    }
}

// —— 内联翻译(浏览器式):把一段原文就地译成目标语言,不落产物 ——

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineTranslateRequest {
    pub document_id: String,
    pub target_language: String,
    /// 同一批要翻译的原文片段(保持顺序返回)。
    pub segments: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineTranslateSegment {
    pub source: String,
    pub translated: Option<String>,
    /// 失败原因(仅稳定错误码,不返回模型正文)。
    pub error_code: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineTranslateResult {
    pub target_language: String,
    /// "local" = 本地离线引擎;"model" = 远程模型回退。
    pub engine: String,
    pub segments: Vec<InlineTranslateSegment>,
}

const INLINE_MAX_SEGMENTS: usize = 40;
const INLINE_MAX_CHARS: usize = 12_000;

/// `translation_inline`:阅读器内联翻译。
///
/// 与整篇翻译不同,这里**不创建任务、不落产物**:它就是一次可见的即时翻译,
/// 用户看到的就是原文旁边的译文。仍经同一 EgressGateway(文档级许可),
/// 占位符保护/术语/数字原样保留规则与整篇翻译一致。
#[tauri::command]
pub async fn translation_inline(
    state: tauri::State<'_, AppState>,
    request: InlineTranslateRequest,
) -> Result<InlineTranslateResult, AppError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || inline_translate(&state, request))
        .await
        .map_err(|_| AppError::internal("inline_translation_join_failed"))?
}

fn inline_translate(
    state: &AppState,
    request: InlineTranslateRequest,
) -> Result<InlineTranslateResult, AppError> {
    use crate::retrieval_ai::models::{
        GenerationInput, SourceRole, StructuredGenerationRequest, TaskKind,
    };

    let target = request.target_language.trim().to_string();
    if target.is_empty() || target.chars().count() > 32 {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "invalid_target_language".into()),
        );
    }
    let sources: Vec<String> = request
        .segments
        .into_iter()
        .take(INLINE_MAX_SEGMENTS)
        .map(|s| s.trim().to_string())
        .collect();
    if sources.is_empty() || sources.iter().all(|s| s.is_empty()) {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "empty_selection".into()),
        );
    }
    let total: usize = sources.iter().map(|s| s.chars().count()).sum();
    if total > INLINE_MAX_CHARS {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "selection_too_long".into()),
        );
    }

    // —— 本地引擎优先(离线,不联网、不调大模型)——
    if let Some(result) = try_local_translate(state, &target, &sources) {
        return Ok(result);
    }

    // 占位符保护:代码/URL/邮箱/公式/数字原样保留(与整篇翻译同一实现)。
    let masked: Vec<(String, Vec<(String, String)>)> = sources
        .iter()
        .map(|text| crate::translation::service::protect(text))
        .collect();
    let inputs: Vec<GenerationInput> = masked
        .iter()
        .enumerate()
        .map(|(index, (masked_text, _))| GenerationInput {
            source_id: format!("i{}", index + 1),
            role: SourceRole::Original,
            text: masked_text.clone(),
        })
        .collect();
    // 指令尽量短:实测长指令会被弱模型当作原文翻译回来。
    // 规则在系统指令里;这里只保留任务与响应形状,并给出真实 id 示例。
    let ids = (1..=sources.len())
        .map(|index| format!("i{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let question = format!(
        "Translate each SOURCE segment into {target}. The SOURCE ids are: {ids}. \
         Respond with JSON only, using those exact ids, for example \
         {{\"translations\":[{{\"id\":\"i1\",\"text\":\"translated text\"}}]}} schemaVersion={}.",
        crate::translation::models::TRANSLATION_SCHEMA_VERSION
    );
    let spec = StructuredGenerationRequest {
        task_id: format!("inline-translation:{}", crate::shared::new_trace_id()),
        attempt_id: String::new(),
        document_id: request.document_id.clone(),
        snapshot_id: String::new(),
        schema_version: crate::translation::models::TRANSLATION_SCHEMA_VERSION.to_string(),
        schema: crate::translation::service::inline_translation_schema(),
        instruction_version: crate::translation::service::TRANSLATION_INSTRUCTION_VERSION
            .to_string(),
        task_kind: TaskKind::Translation,
        inputs,
        locale: target.clone(),
        question,
        history: Vec::new(),
        history_summary: None,
        attachments: Vec::new(),
        document_map: None,
        // 文档级许可:不绑定任务,但授权/载荷哈希/白名单/审计仍由网关校验。
        scope: crate::model_privacy::gateway::EgressScope::Document,
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    let generated = state.generation.generate_translation(&spec, &cancel);

    let mut out: Vec<InlineTranslateSegment> = sources
        .iter()
        .map(|source| InlineTranslateSegment {
            source: source.clone(),
            translated: None,
            error_code: None,
        })
        .collect();

    match generated {
        Ok(result) => {
            let map = crate::translation::service::parse_inline_translations(&result.content);
            // 单段请求的兜底:模型只返回一条但 id 形状不可识别时,仍能对上唯一的段。
            let single_fallback = if sources.len() == 1 && map.len() == 1 {
                map.values().next()
            } else {
                None
            };
            for (index, (_, tokens)) in masked.iter().enumerate() {
                let id = format!("i{}", index + 1);
                match crate::translation::service::lookup_translation(&map, &id).or(single_fallback)
                {
                    Some(text) => {
                        let restored = crate::translation::service::restore(text, tokens);
                        // 模型回显指令 / 原样返回原文:都不是译文,如实报错。
                        if crate::translation::service::looks_like_instruction(&restored) {
                            out[index].error_code = Some("translation_echoed_instruction".into());
                        } else if crate::translation::service::translation_not_applied(
                            &sources[index],
                            &restored,
                        ) {
                            out[index].error_code = Some("translation_not_applied".into());
                        } else {
                            out[index].translated = Some(restored);
                        }
                    }
                    None => out[index].error_code = Some("segment_missing_in_response".into()),
                }
            }
        }
        Err(error) => {
            // 模型不可用/超时:如实返回错误码,由 UI 显示下一步,不伪造译文。
            for segment in &mut out {
                segment.error_code = Some(error.code.clone());
            }
        }
    }

    Ok(InlineTranslateResult {
        target_language: target,
        engine: "model".into(),
        segments: out,
    })
}

/// 本地引擎翻译;引擎不可用(未安装模型)时返回 None 交给调用方回退。
fn try_local_translate(
    state: &AppState,
    target: &str,
    sources: &[String],
) -> Option<InlineTranslateResult> {
    use crate::translation::offline::{split_sentences, EngineStatus};

    // 逐个片段翻译:片段内再按句切分,句级模型对长句更稳。
    let mut segments: Vec<InlineTranslateSegment> = Vec::new();
    let mut any_success = false;
    for source in sources {
        let sentences = split_sentences(source);
        if sentences.is_empty() {
            segments.push(InlineTranslateSegment {
                source: source.clone(),
                translated: Some(String::new()),
                error_code: None,
            });
            continue;
        }
        match state
            .local_translation
            .translate_batch("auto", target, &sentences)
        {
            Ok(translated) => {
                any_success = true;
                let joined = translated
                    .iter()
                    .map(|item| item.text.trim_end().to_string())
                    .collect::<Vec<_>>()
                    .join("");
                segments.push(InlineTranslateSegment {
                    source: source.clone(),
                    translated: Some(joined),
                    error_code: None,
                });
            }
            Err(error) => {
                let info = state.local_translation.info("auto", target);
                if info.status == EngineStatus::ModelMissing {
                    // 模型没装:明确告诉调用方"本地不可用",由上层决定回退或报错。
                    return None;
                }
                segments.push(InlineTranslateSegment {
                    source: source.clone(),
                    translated: None,
                    error_code: Some(format!("{error:?}")),
                });
            }
        }
    }
    if !any_success {
        return None;
    }
    Some(InlineTranslateResult {
        target_language: target.to_string(),
        engine: "local".to_string(),
        segments,
    })
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTranslationStatusRequest {
    pub source_language: String,
    pub target_language: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTranslationStatus {
    /// ready / model_missing / backend_unavailable
    pub status: String,
    pub engine_name: String,
    /// 模型应放置的目录(便于用户核对;不含任何密钥)。
    pub model_dir: String,
}

/// `translation_local_status`:本地翻译引擎是否可用(不加载模型)。
#[tauri::command]
pub async fn translation_local_status(
    state: tauri::State<'_, AppState>,
    request: LocalTranslationStatusRequest,
) -> Result<LocalTranslationStatus, AppError> {
    let info = state
        .local_translation
        .info(&request.source_language, &request.target_language);
    Ok(LocalTranslationStatus {
        status: match info.status {
            crate::translation::offline::EngineStatus::Ready => "ready".into(),
            crate::translation::offline::EngineStatus::ModelMissing => "model_missing".into(),
            crate::translation::offline::EngineStatus::BackendUnavailable => {
                "backend_unavailable".into()
            }
        },
        engine_name: info.engine_name,
        model_dir: info
            .model_dir
            .map(|dir| dir.display().to_string())
            .unwrap_or_default(),
    })
}
