//! P2 Agent 命令层(`desktop` feature)。
//!
//! `agent_run` 复用 M07 会话消息生命周期:立即返回 TaskView,循环在后台
//! 阻塞线程执行;`agent_get_steps` 轮询步骤;`agent_cancel` 走 M01 取消令牌。
//! 默认问答仍是 `ai_ask`;本命令只服务显式“深度研究”。

use std::sync::Arc;

use serde::Deserialize;

use crate::app_shell::ask_flow::{
    conversation_title, notice_for_failure, recent_history, visible_failure_message,
    HISTORY_MESSAGE_LIMIT,
};
use crate::app_shell::streaming::{ChannelObserver, RunWatchdog, StreamEventV1};
use crate::app_shell::AppState;
use crate::attachments::build_user_message_text;
use crate::context::service::{NewAssistantMessage, NewUserMessage};
use crate::metrics::{RunMetricsInput, RunMetricsService};
use crate::retrieval_ai::models::GenerationAttachment;
use crate::shared::dto::TaskView;
use crate::shared::error::{AppError, DetailValue};
use crate::shared::stream::{noop_observer, SharedObserver};
use crate::shared::CoreErrorCode;

use super::models::{AgentRunInput, AgentRunStepsView, AgentStart};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunRequest {
    pub conversation_id: String,
    pub snapshot_id: String,
    /// 模型输入:纯文本问题,不含图片 base64。
    pub question: String,
    #[serde(default)]
    pub display_text: Option<String>,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    pub locale: String,
    pub idempotency_key: String,
}

/// Agent 会话/消息/快照准备结果。
pub(crate) struct PreparedAgent {
    document_id: String,
    conversation_id: String,
    assistant_id: String,
    snapshot: crate::context::models::ContextSnapshotView,
    history: Vec<crate::retrieval_ai::models::ChatTurn>,
    history_summary: Option<String>,
    attachments: Vec<GenerationAttachment>,
}

fn prepare_agent_messages(
    state: &AppState,
    request: &AgentRunRequest,
) -> Result<PreparedAgent, AppError> {
    if request.question.trim().is_empty() && request.attachment_ids.is_empty() {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "empty_question".into()),
        );
    }
    let document_id = state
        .retrieval
        .snapshot_document_id(&request.snapshot_id)?
        .ok_or_else(|| AppError::new(CoreErrorCode::NotFound, crate::shared::new_trace_id()))?;
    let base_display = request
        .display_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(request.question.as_str());
    let title = conversation_title(base_display, &request.question);

    let conversation_id = if request.conversation_id.trim().is_empty() {
        state
            .context
            .conversation_create(
                &document_id,
                Some(&request.snapshot_id),
                &title,
                &format!("{}:agent_conv", request.idempotency_key),
            )?
            .id
    } else {
        let _ = state.context.conversation_messages(
            &request.conversation_id,
            crate::shared::paging::PageRequest {
                limit: 1,
                cursor: None,
            },
        )?;
        request.conversation_id.clone()
    };

    let attachment_ids = request.attachment_ids.clone();
    if !attachment_ids.is_empty() {
        state
            .attachments
            .validate_for_ask(&document_id, &conversation_id, &attachment_ids)?;
    }
    let display_text = build_user_message_text(base_display, &attachment_ids);
    // 历史必须在写入本轮用户消息之前读取。
    let history = recent_history(
        &state
            .context
            .conversation_recent_messages(&conversation_id, HISTORY_MESSAGE_LIMIT)?,
    );
    let history_summary = state
        .context
        .conversation_summary(&conversation_id)?
        .map(|row| row.summary_text)
        .filter(|summary| !summary.trim().is_empty());

    let user_message = state.context.append_user_message(NewUserMessage {
        conversation_id: &conversation_id,
        snapshot_id: Some(&request.snapshot_id),
        task_id: None,
        text: &display_text,
        idempotency_key: &format!("{}:agent_user", request.idempotency_key),
    })?;
    let attachments = if attachment_ids.is_empty() {
        Vec::new()
    } else {
        state.attachments.bind_to_message(
            &document_id,
            &conversation_id,
            &user_message.id,
            &attachment_ids,
        )?;
        state
            .attachments
            .load_for_generation(&document_id, &attachment_ids)?
    };
    let assistant = state
        .context
        .append_assistant_placeholder(NewAssistantMessage {
            conversation_id: &conversation_id,
            snapshot_id: Some(&request.snapshot_id),
            task_id: None,
            idempotency_key: &format!("{}:agent_assistant", request.idempotency_key),
        })?;
    let snapshot = match state.context.get_snapshot(&request.snapshot_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            let error = AppError::new(CoreErrorCode::NotFound, crate::shared::new_trace_id());
            let _ = state.context.fail_assistant_message_with_notice(
                &assistant.id,
                &error.code,
                false,
                &notice_for_failure(&error.code, false),
            );
            return Err(error);
        }
        Err(error) => {
            let _ = state.context.fail_assistant_message_with_notice(
                &assistant.id,
                &error.code,
                false,
                &notice_for_failure(&error.code, false),
            );
            return Err(error);
        }
    };

    Ok(PreparedAgent {
        document_id,
        conversation_id,
        assistant_id: assistant.id,
        snapshot,
        history,
        history_summary,
        attachments,
    })
}

/// 质量模式失败时的快速降级上下文(同一冻结快照,不换范围)。
struct FastFallbackContext {
    document_id: String,
    conversation_id: String,
    snapshot_id: String,
    question: String,
    locale: String,
    history: Vec<crate::retrieval_ai::models::ChatTurn>,
    attachments: Vec<GenerationAttachment>,
}

/// 质量模式失败后的可见降级:用同一快照跑一次快速单次问答。
///
/// 只在“质量链路整体失败(超时/不可用/输出不可用)”时调用;取消不降级。
/// 成功时把答案写入原助手消息并标注降级原因;失败返回 false,由调用方
/// 继续走 `settle_failure`(原文抽取或可见通知)。
fn try_fast_fallback(
    retrieval: &crate::retrieval_ai::RetrievalAiService,
    context: &crate::context::service::ContextService,
    ctx: &FastFallbackContext,
    assistant_id: &str,
    error_code: &str,
) -> bool {
    use crate::retrieval_ai::service::AskRequest;
    let ask = AskRequest {
        document_id: ctx.document_id.clone(),
        conversation_id: ctx.conversation_id.clone(),
        // 引用属主仍绑定同一条助手消息,引用可回跳。
        assistant_message_id: Some(assistant_id.to_string()),
        snapshot_id: ctx.snapshot_id.clone(),
        question: ctx.question.clone(),
        locale: ctx.locale.clone(),
        idempotency_key: format!("{}:fast-fallback", ctx.question_hash_key()),
        history: ctx.history.clone(),
        attachments: ctx.attachments.clone(),
    };
    let task = match retrieval.ask(ask) {
        Ok(task) => task,
        Err(error) => {
            tracing::warn!(error_code = error.code, "fast fallback failed");
            return false;
        }
    };
    let Some(mut answer) = retrieval.task_answer(&task.id).ok().flatten() else {
        return false;
    };
    // 明确标注:这不是质量模式的结论。
    answer.unverified_notes.push(format!(
        "质量模式未完成({error_code});已自动改用快速单次回答"
    ));
    context
        .complete_assistant_message(assistant_id, &answer, None, None)
        .is_ok()
}

impl FastFallbackContext {
    /// 幂等键只需在同一次提问内稳定;用快照+问题长度做轻量派生,
    /// 不把问题正文写入键(避免正文进日志/数据库键)。
    fn question_hash_key(&self) -> String {
        format!(
            "{}:{}",
            self.snapshot_id,
            crate::shared::textnorm::sha256_str(&self.question)
        )
    }
}

fn start_agent(
    state: &AppState,
    request: &AgentRunRequest,
    prepared: PreparedAgent,
    observer: SharedObserver,
    channel: Option<Arc<ChannelObserver>>,
) -> Result<TaskView, AppError> {
    let document_id = prepared.document_id.clone();
    // 降级上下文必须在 input 移动 prepared 字段之前取好。
    let fallback_ctx = FastFallbackContext {
        document_id: prepared.document_id.clone(),
        conversation_id: prepared.conversation_id.clone(),
        snapshot_id: request.snapshot_id.clone(),
        question: request.question.clone(),
        locale: request.locale.clone(),
        history: prepared.history.clone(),
        attachments: prepared.attachments.clone(),
    };
    let input = AgentRunInput {
        document_id: prepared.document_id,
        snapshot: prepared.snapshot,
        conversation_id: prepared.conversation_id,
        assistant_message_id: prepared.assistant_id.clone(),
        question: request.question.clone(),
        locale: request.locale.clone(),
        idempotency_key: request.idempotency_key.clone(),
        history: prepared.history,
        history_summary: prepared.history_summary,
        attachments: prepared.attachments,
        observer,
    };

    match state.agent.start(input) {
        Ok(AgentStart::Ready { task, answer }) => {
            let _ = state
                .context
                .bind_message_task(&prepared.assistant_id, &task.id);
            let _ =
                state
                    .context
                    .complete_agent_message(&prepared.assistant_id, &answer, None, None);
            if let Some(observer) = &channel {
                observer.set_task_id(&task.id);
                observer.run_finished(true);
            }
            Ok(task.to_view())
        }
        Ok(AgentStart::Started { task, job }) => {
            let _ = state
                .context
                .bind_message_task(&prepared.assistant_id, &task.id);
            if let Some(observer) = &channel {
                observer.set_task_id(&task.id);
            }
            let agent = state.agent.clone();
            let context = state.context.clone();
            let metrics_service = Arc::clone(&state.metrics);
            let metrics_task_id = task.id.clone();
            let save_reasoning = state.model_privacy.save_reasoning_enabled();
            let reasoning_service = Arc::clone(&state.reasoning);
            let reasoning_document_id = document_id.clone();
            let message_id = prepared.assistant_id.clone();
            // 降级为快速单次回答所需的上下文(与质量模式同一冻结快照)。
            let job = *job;
            let panic_context = Arc::clone(&context);
            let panic_observer = channel.clone();
            let panic_message = message_id.clone();
            // 快速降级需要 &RetrievalAiService / &ContextService;
            // 在后台线程里直接用 Arc 化的服务跑一次快速问答,
            // 避免持有 tauri::State 的生命周期。
            let fallback_retrieval = Arc::clone(&state.retrieval);
            let fallback_context = Arc::clone(&state.context);
            // 后台阻塞线程:循环内含独立 tokio 运行时桥接网关;不阻塞命令返回。
            std::thread::spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match agent.execute(&job) {
                        Ok(outcome) => {
                            match context.complete_agent_message(
                                &message_id,
                                &outcome.answer,
                                outcome.provider_id.as_deref(),
                                outcome.model_id.as_deref(),
                            ) {
                                Ok(_) => {
                                    if let Some(observer) = &channel {
                                        observer.run_finished(outcome.answer.verified);
                                    }
                                }
                                Err(error) => {
                                    let _ = context.fail_assistant_message_with_notice(
                                        &message_id,
                                        &error.code,
                                        false,
                                        &notice_for_failure(&error.code, false),
                                    );
                                    if let Some(observer) = &channel {
                                        observer.mark_fallback();
                                        observer
                                            .run_failed(&error.code, "回答已生成,但消息保存失败。");
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            // 只记录稳定错误码与非敏感 reason,不含正文/路径。
                            let reason = error
                                .details
                                .as_ref()
                                .and_then(|details| details.get("reason"))
                                .and_then(|value| match value {
                                    DetailValue::Str(value) => Some(value.as_str()),
                                    _ => None,
                                })
                                .unwrap_or("-");
                            tracing::warn!(
                                event = "agent_run_failed",
                                error_code = error.code,
                                phase = reason
                            );
                            let canceled = error.code == "TASK_CANCELED";
                            // 质量链路失败:先尝试快速单次降级(可见答案),
                            // 再退回原文抽取/可见通知。取消不降级。
                            let degraded = !canceled
                                && !is_in_progress(&error)
                                && try_fast_fallback(
                                    &fallback_retrieval,
                                    &fallback_context,
                                    &fallback_ctx,
                                    &message_id,
                                    &error.code,
                                );
                            if degraded {
                                if let Some(observer) = &channel {
                                    observer.mark_fallback();
                                    observer.run_finished(false);
                                }
                            } else {
                                crate::app_shell::ask_flow::settle_failure(
                                    &context,
                                    &message_id,
                                    &fallback_ctx.snapshot_id,
                                    &error.code,
                                    canceled,
                                    Some(fallback_ctx.question.as_str()),
                                );
                                if let Some(observer) = &channel {
                                    observer.mark_fallback();
                                    observer.run_failed(
                                        &error.code,
                                        &visible_failure_message(&error.code),
                                    );
                                }
                            }
                        }
                    }
                }));
                if outcome.is_err() {
                    if let Some(observer) = &panic_observer {
                        observer.mark_fallback();
                    }
                    let _ = panic_context.fail_assistant_message_with_notice(
                        &panic_message,
                        "INTERNAL_ERROR",
                        false,
                        &notice_for_failure("INTERNAL_ERROR", false),
                    );
                    if let Some(observer) = &panic_observer {
                        observer.run_failed("INTERNAL_ERROR", "研究执行中断,请重试。");
                    }
                }
                if let Some(observer) = &panic_observer {
                    record_quality_metrics(
                        &metrics_service,
                        &metrics_task_id,
                        &message_id,
                        observer,
                    );
                    if save_reasoning {
                        let text = observer.reasoning_text();
                        if !text.trim().is_empty() {
                            if let Err(error) = reasoning_service.save(
                                &reasoning_document_id,
                                &metrics_task_id,
                                &message_id,
                                &text,
                            ) {
                                tracing::warn!(
                                    error_code = error.code,
                                    "reasoning persistence failed (answer unaffected)"
                                );
                            }
                        }
                    }
                }
            });
            Ok(task.to_view())
        }
        Err(error) => {
            // 同一幂等键仍在运行:不要把手上的助手占位标记失败。
            if !is_in_progress(&error) {
                let canceled = error.code == "TASK_CANCELED";
                let _ = state.context.fail_assistant_message_with_notice(
                    &prepared.assistant_id,
                    &error.code,
                    canceled,
                    &notice_for_failure(&error.code, canceled),
                );
            }
            if let Some(observer) = &channel {
                observer.mark_fallback();
                observer.run_failed(&error.code, &visible_failure_message(&error.code));
            }
            Err(error)
        }
    }
}

/// 一次质量模式运行的实际启动结果(命令与任务中心重试共用)。
pub(crate) struct AgentRunStarted {
    pub task: TaskView,
    pub document_id: String,
    pub conversation_id: String,
    pub assistant_message_id: String,
}

/// 准备会话消息并启动 Agent 循环(同步返回 TaskView;循环在后台线程)。
pub(crate) fn run_agent_now(
    state: &AppState,
    request: &AgentRunRequest,
) -> Result<AgentRunStarted, AppError> {
    let prepared = prepare_agent_messages(state, request)?;
    let document_id = prepared.document_id.clone();
    let conversation_id = prepared.conversation_id.clone();
    let assistant_message_id = prepared.assistant_id.clone();
    let task = start_agent(state, request, prepared, noop_observer(), None)?;
    Ok(AgentRunStarted {
        task,
        document_id,
        conversation_id,
        assistant_message_id,
    })
}

#[tauri::command]
pub async fn agent_run(
    state: tauri::State<'_, AppState>,
    request: AgentRunRequest,
) -> Result<TaskView, AppError> {
    Ok(run_agent_now(state.inner(), &request)?.task)
}

/// 质量模式流式入口:立即返回 TaskView;步骤/工具/推理通过 Channel 推送。
#[tauri::command]
pub async fn agent_run_streamed(
    state: tauri::State<'_, AppState>,
    request: AgentRunRequest,
    on_event: tauri::ipc::Channel<StreamEventV1>,
) -> Result<TaskView, AppError> {
    let prepared = prepare_agent_messages(state.inner(), &request)?;
    let observer = Arc::new(ChannelObserver::new(on_event, &prepared.assistant_id));
    observer.run_started("quality");
    RunWatchdog::start(Arc::clone(&observer), "quality");
    let stream_observer: SharedObserver = observer.clone();
    start_agent(
        state.inner(),
        &request,
        prepared,
        stream_observer,
        Some(observer),
    )
}

/// 质量链路运行指标(不含正文/推理/路径)。
fn record_quality_metrics(
    service: &RunMetricsService,
    task_id: &str,
    message_id: &str,
    observer: &ChannelObserver,
) {
    let snapshot = observer.metrics();
    let input = RunMetricsInput {
        task_id: task_id.to_string(),
        message_id: Some(message_id.to_string()),
        mode: "quality".to_string(),
        status: observer
            .final_status()
            .unwrap_or_else(|| "failed".to_string()),
        first_token_ms: snapshot.first_token_ms,
        first_visible_answer_ms: snapshot.first_visible_answer_ms,
        reasoning_first_event_ms: snapshot.reasoning_first_event_ms,
        max_event_gap_ms: snapshot.max_event_gap_ms,
        first_answer_ms: snapshot.first_answer_ms,
        total_ms: snapshot.total_ms.or_else(|| Some(observer.elapsed_ms())),
        fallback_used: snapshot.fallback_used,
        citation_valid_rate: snapshot.citation_valid_rate,
    };
    if let Err(error) = service.record(input) {
        tracing::warn!(
            error_code = error.code,
            "quality run metrics write failed (answer unaffected)"
        );
    }
}

fn is_in_progress(error: &AppError) -> bool {
    error
        .details
        .as_ref()
        .and_then(|details| details.get("reason"))
        .map(|reason| matches!(reason, DetailValue::Str(value) if value == "agent_run_in_progress"))
        .unwrap_or(false)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentGetStepsRequest {
    pub task_id: String,
}

#[tauri::command]
pub async fn agent_get_steps(
    state: tauri::State<'_, AppState>,
    request: AgentGetStepsRequest,
) -> Result<AgentRunStepsView, AppError> {
    state.agent.get_steps(&request.task_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfirmToolRequest {
    pub task_id: String,
    pub step_id: String,
    /// approve / reject
    pub decision: String,
    /// export_file 批准时由保存对话框给出的目标路径;不落库、不进日志。
    #[serde(default)]
    pub path: Option<String>,
}

#[tauri::command]
pub async fn agent_confirm_tool(
    state: tauri::State<'_, AppState>,
    request: AgentConfirmToolRequest,
) -> Result<bool, AppError> {
    state.agent.confirm_tool(
        &request.task_id,
        &request.step_id,
        &request.decision,
        request.path,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCancelRequest {
    pub task_id: String,
    pub expected_revision: i64,
}

#[tauri::command]
pub async fn agent_cancel(
    state: tauri::State<'_, AppState>,
    request: AgentCancelRequest,
) -> Result<TaskView, AppError> {
    let task = state
        .tasks
        .request_cancel(&request.task_id, request.expected_revision)?;
    // 立即唤醒所有等待确认的写工具(零写入),后台循环随后收敛为 canceled。
    state.agent.cancel_pending_for_task(&request.task_id);
    Ok(task.to_view())
}
