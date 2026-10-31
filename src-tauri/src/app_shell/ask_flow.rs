//! M13 问答编排命令(M07 消息生命周期 × M09 问答,`desktop` feature)。
//!
//! 命令名与请求形状沿用《02》§3 的 `ai_ask`;组装根在此把跨模块流程
//! 组合成一个入口(《00》§3.1:跨模块调用发生在应用服务层):
//!
//! 1. 快照反查文档;
//! 2. conversation 为空时自动创建会话;非空但不存在时报错;
//! 3. 追加用户消息(幂等键 = 请求幂等键 + ":user");
//! 4. 追加助手占位消息(问题发出即占位,失败也留痕);
//! 5. `ask`(引用属主绑定到助手消息 ID);
//! 6. 成功 → 用已保存答案完成助手消息;失败/取消 → fail 助手消息
//!    (用户消息保留:问题确实发出过)。

use std::sync::Arc;

use crate::app_shell::streaming::{ChannelObserver, RunWatchdog, StreamEventV1};
use crate::attachments::{build_user_message_text, strip_markdown_images};
use crate::context::models::{
    AssistantNoticeAction, AssistantNoticeV1, MessageContent, MessageRole, MessageView,
    TaskRetryResult,
};
use crate::context::service::{NewAssistantMessage, NewUserMessage};
use crate::metrics::{RunMetricsInput, RunMetricsService};
use crate::retrieval_ai::commands::AiAskRequest;
use crate::retrieval_ai::models::{ChatTurn, ChatTurnRole, GenerationAttachment};
use crate::retrieval_ai::service::AskRequest;
use crate::shared::dto::TaskView;
use crate::shared::error::AppError;
use crate::shared::CoreErrorCode;

/// 消息与会话准备结果(同一幂等键重放时消息可复用)。
pub(crate) struct PreparedMessages {
    pub document_id: String,
    pub conversation_id: String,
    pub assistant_id: String,
    pub history: Vec<ChatTurn>,
    pub attachments: Vec<GenerationAttachment>,
}

/// 会话/用户消息/助手占位/附件绑定;在模型调用之前完成。
pub(crate) fn prepare_messages(
    state: &crate::app_shell::AppState,
    request: &AiAskRequest,
) -> Result<PreparedMessages, AppError> {
    let document_id = state
        .retrieval
        .snapshot_document_id(&request.snapshot_id)?
        .ok_or_else(|| AppError::new(CoreErrorCode::NotFound, crate::shared::new_trace_id()))?;

    // 展示文本只保留纯文本;客户端图片 Markdown(base64/旧引用)一律剥离。
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
                &format!("{}:conv", request.idempotency_key),
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

    // 多轮历史必须在写入本轮用户消息之前读取。
    let history = recent_history(
        &state
            .context
            .conversation_recent_messages(&conversation_id, HISTORY_MESSAGE_LIMIT)?,
    );

    let user_message = state.context.append_user_message(NewUserMessage {
        conversation_id: &conversation_id,
        snapshot_id: Some(&request.snapshot_id),
        task_id: None,
        text: &display_text,
        idempotency_key: &format!("{}:user", request.idempotency_key),
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
            idempotency_key: &format!("{}:assistant", request.idempotency_key),
        })?;

    Ok(PreparedMessages {
        document_id,
        conversation_id,
        assistant_id: assistant.id,
        history,
        attachments,
    })
}

fn ask_request(request: &AiAskRequest, prepared: &PreparedMessages) -> AskRequest {
    AskRequest {
        document_id: prepared.document_id.clone(),
        conversation_id: prepared.conversation_id.clone(),
        assistant_message_id: Some(prepared.assistant_id.clone()),
        snapshot_id: request.snapshot_id.clone(),
        question: request.question.clone(),
        locale: request.locale.clone(),
        idempotency_key: request.idempotency_key.clone(),
        history: prepared.history.clone(),
        attachments: prepared.attachments.clone(),
    }
}

#[tauri::command]
pub fn ai_ask(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AiAskRequest,
) -> Result<TaskView, AppError> {
    Ok(run_ask_now(state.inner(), &request)?.task)
}

/// 一次问答的实际执行(命令与任务中心重试共用)。
pub(crate) struct AskStarted {
    pub task: TaskView,
    pub document_id: String,
    pub conversation_id: String,
    pub assistant_message_id: String,
}

pub(crate) fn run_ask_now(
    state: &crate::app_shell::AppState,
    request: &AiAskRequest,
) -> Result<AskStarted, AppError> {
    let prepared = prepare_messages(state, request)?;
    let document_id = prepared.document_id.clone();
    let conversation_id = prepared.conversation_id.clone();
    let assistant_message_id = prepared.assistant_id.clone();
    let ask = ask_request(request, &prepared);
    let (task, prepared_ask) = state.retrieval.prepare_ask(ask)?;
    // 任务一创建就绑定到助手占位:失败/取消也能从任务中心找到原问题并重试。
    let _ = state
        .context
        .bind_message_task(&prepared.assistant_id, &task.id);

    match prepared_ask {
        None => {
            // 同一幂等键重放/已终态:不重复生成;有结果则写回消息。
            if let Some(answer) = state.retrieval.task_answer(&task.id)? {
                let _ = state.context.complete_assistant_message(
                    &prepared.assistant_id,
                    &answer,
                    None,
                    None,
                );
            } else if matches!(
                task.status,
                crate::shared::dto::TaskStatus::Failed
                    | crate::shared::dto::TaskStatus::Canceled
                    | crate::shared::dto::TaskStatus::Interrupted
            ) {
                settle_failure(
                    &state.context,
                    &prepared.assistant_id,
                    &request.snapshot_id,
                    task.error_code.as_deref().unwrap_or("ANSWER_MISSING"),
                    task.status == crate::shared::dto::TaskStatus::Canceled,
                    Some(&request.question),
                );
            }
            Ok(AskStarted {
                task: task.to_view(),
                document_id,
                conversation_id,
                assistant_message_id,
            })
        }
        Some(prepared_ask) => match state
            .retrieval
            .execute_prepared(prepared_ask, crate::shared::stream::noop_observer())
        {
            Ok(task) => {
                match state.retrieval.task_answer(&task.id)? {
                    Some(answer) => {
                        let _ = state.context.complete_assistant_message(
                            &prepared.assistant_id,
                            &answer,
                            None,
                            None,
                        );
                    }
                    None => {
                        settle_failure(
                            &state.context,
                            &prepared.assistant_id,
                            &request.snapshot_id,
                            "ANSWER_MISSING",
                            false,
                            Some(&request.question),
                        );
                    }
                }
                Ok(AskStarted {
                    task: task.to_view(),
                    document_id,
                    conversation_id,
                    assistant_message_id,
                })
            }
            Err(e) => {
                let canceled = e.code == "TASK_CANCELED";
                settle_failure(
                    &state.context,
                    &prepared.assistant_id,
                    &request.snapshot_id,
                    &e.code,
                    canceled,
                    Some(&request.question),
                );
                Err(e)
            }
        },
    }
}

/// 快速模式流式入口:立即返回 TaskView;检索/生成/校验在后台线程执行,
/// 真实阶段与 reasoning 通过 Channel 推送。最终消息仍由数据库提交。
#[tauri::command]
pub async fn ai_ask_streamed(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: AiAskRequest,
    on_event: tauri::ipc::Channel<StreamEventV1>,
) -> Result<TaskView, AppError> {
    let prepared = prepare_messages(state.inner(), &request)?;
    let observer = Arc::new(ChannelObserver::new(on_event, &prepared.assistant_id));
    observer.run_started("fast");
    // 看门狗独立于 UI:无可见内容时发真实通知,质量模式按真实间隔发心跳。
    RunWatchdog::start(Arc::clone(&observer), "fast");
    let ask = ask_request(&request, &prepared);

    match state.retrieval.prepare_ask(ask) {
        Ok((task, Some(prepared_ask))) => {
            let _ = state
                .context
                .bind_message_task(&prepared.assistant_id, &task.id);
            observer.set_task_id(&task.id);
            let task_view = task.to_view();
            let retrieval = Arc::clone(&state.retrieval);
            let context = Arc::clone(&state.context);
            let metrics_service = Arc::clone(&state.metrics);
            let metrics_task_id = task.id.clone();
            let message_id = prepared.assistant_id.clone();
            let save_reasoning = state.model_privacy.save_reasoning_enabled();
            let reasoning_service = Arc::clone(&state.reasoning);
            let reasoning_document_id = prepared.document_id.clone();
            let panic_context = Arc::clone(&context);
            let panic_observer = Arc::clone(&observer);
            let panic_message = message_id.clone();
            let snapshot_id = request.snapshot_id.clone();
            let question = request.question.clone();
            std::thread::spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match retrieval.execute_prepared(prepared_ask, observer.clone()) {
                        Ok(task) => match retrieval.task_answer(&task.id) {
                            Ok(Some(answer)) => {
                                match context.complete_assistant_message(
                                    &message_id,
                                    &answer,
                                    None,
                                    None,
                                ) {
                                    Ok(_) => {
                                        observer.set_citation_valid_rate(
                                            if answer.citations.is_empty() {
                                                None
                                            } else {
                                                Some(1.0)
                                            },
                                        );
                                        observer.run_finished(true);
                                    }
                                    Err(error) => {
                                        let _ = context.fail_assistant_message_with_notice(
                                            &message_id,
                                            &error.code,
                                            false,
                                            &notice_for_failure(&error.code, false),
                                        );
                                        observer
                                            .run_failed(&error.code, "回答已生成,但消息保存失败。")
                                    }
                                }
                            }
                            Ok(None) => {
                                observer.mark_fallback();
                                settle_failure(
                                    &context,
                                    &message_id,
                                    &snapshot_id,
                                    "ANSWER_MISSING",
                                    false,
                                    Some(question.as_str()),
                                );
                                observer.run_failed(
                                    "ANSWER_MISSING",
                                    "模型未返回可用答案,已改用原文要点。",
                                )
                            }
                            Err(error) => {
                                observer.mark_fallback();
                                settle_failure(
                                    &context,
                                    &message_id,
                                    &snapshot_id,
                                    &error.code,
                                    false,
                                    Some(question.as_str()),
                                );
                                observer
                                    .run_failed(&error.code, &visible_failure_message(&error.code))
                            }
                        },
                        Err(error) => {
                            let canceled = error.code == "TASK_CANCELED";
                            observer.mark_fallback();
                            settle_failure(
                                &context,
                                &message_id,
                                &snapshot_id,
                                &error.code,
                                canceled,
                                Some(question.as_str()),
                            );
                            observer.run_failed(&error.code, &visible_failure_message(&error.code));
                        }
                    }
                }));
                if outcome.is_err() {
                    // panic 也要留下可见终态,绝不让占位无限 pending。
                    panic_observer.mark_fallback();
                    let _ = panic_context.fail_assistant_message_with_notice(
                        &panic_message,
                        "INTERNAL_ERROR",
                        false,
                        &notice_for_failure("INTERNAL_ERROR", false),
                    );
                    panic_observer.run_failed("INTERNAL_ERROR", "回答执行中断,请重试。");
                }
                record_run_metrics(
                    &metrics_service,
                    &metrics_task_id,
                    &message_id,
                    &panic_observer,
                );
                if save_reasoning {
                    let text = panic_observer.reasoning_text();
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
            });
            Ok(task_view)
        }
        Ok((task, None)) => {
            let _ = state
                .context
                .bind_message_task(&prepared.assistant_id, &task.id);
            observer.set_task_id(&task.id);
            if let Some(answer) = state.retrieval.task_answer(&task.id)? {
                let _ = state.context.complete_assistant_message(
                    &prepared.assistant_id,
                    &answer,
                    None,
                    None,
                );
            }
            observer.run_finished(true);
            Ok(task.to_view())
        }
        Err(error) => {
            let canceled = error.code == "TASK_CANCELED";
            observer.mark_fallback();
            settle_failure(
                &state.context,
                &prepared.assistant_id,
                &request.snapshot_id,
                &error.code,
                canceled,
                Some(&request.question),
            );
            observer.run_failed(&error.code, &visible_failure_message(&error.code));
            Err(error)
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickAnswerRequest {
    pub snapshot_id: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// 5 秒速答卡(原文抽取)。只读快照,不创建任务、不调用模型。
#[tauri::command]
pub async fn ai_quick_answer(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: QuickAnswerRequest,
) -> Result<crate::context::models::QuickAnswerView, AppError> {
    state.context.quick_answer(
        &request.snapshot_id,
        request.limit.unwrap_or(3).max(1) as usize,
    )
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRetryRequest {
    pub task_id: String,
    pub idempotency_key: String,
}

/// 任务中心「重试」:用原问题与原快照真正重发一轮,不覆盖旧消息。
///
/// 只允许失败/取消/中断的问答或研究任务(成功任务重复发送会重复计费);
/// 原问题从落库消息恢复,图片引用被剥离且默认不重发附件。
#[tauri::command]
pub fn task_retry(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: TaskRetryRequest,
) -> Result<TaskRetryResult, AppError> {
    use crate::shared::dto::{TaskStatus, TaskType};

    if request.idempotency_key.trim().is_empty() {
        return Err(invalid_request("retry_requires_idempotency_key"));
    }
    let task = state
        .tasks
        .get_task(&request.task_id)?
        .ok_or_else(|| AppError::new(CoreErrorCode::NotFound, crate::shared::new_trace_id()))?;
    if !matches!(
        task.status,
        TaskStatus::Failed | TaskStatus::Canceled | TaskStatus::Interrupted
    ) {
        return Err(invalid_request("task_not_retryable"));
    }
    let source = state
        .context
        .task_retry_source(&task.id)?
        .ok_or_else(|| invalid_request("task_retry_source_missing"))?;
    let question = strip_markdown_images(&source.question).trim().to_string();
    if question.is_empty() {
        return Err(invalid_request("retry_requires_text_question"));
    }

    match task.task_type {
        TaskType::AskQuestion => {
            let ask = AiAskRequest {
                conversation_id: source.conversation_id,
                snapshot_id: source.snapshot_id,
                question: question.clone(),
                display_text: Some(question),
                attachment_ids: Vec::new(),
                locale: "zh-CN".into(),
                idempotency_key: request.idempotency_key,
            };
            let started = run_ask_now(state.inner(), &ask)?;
            Ok(TaskRetryResult {
                task: started.task,
                document_id: started.document_id,
                conversation_id: started.conversation_id,
                assistant_message_id: started.assistant_message_id,
            })
        }
        TaskType::AgentRun => {
            let run = crate::agent::commands::AgentRunRequest {
                conversation_id: source.conversation_id,
                snapshot_id: source.snapshot_id,
                question: question.clone(),
                display_text: Some(question),
                attachment_ids: Vec::new(),
                locale: "zh-CN".into(),
                idempotency_key: request.idempotency_key,
            };
            let started = crate::agent::commands::run_agent_now(state.inner(), &run)?;
            Ok(TaskRetryResult {
                task: started.task,
                document_id: started.document_id,
                conversation_id: started.conversation_id,
                assistant_message_id: started.assistant_message_id,
            })
        }
        _ => Err(invalid_request("task_type_not_retryable")),
    }
}

fn invalid_request(reason: &'static str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
        .with_detail("reason", reason.into())
}

/// 记录一次运行的耗时/比例指标(不含正文/密钥/路径);失败只告警。
fn record_run_metrics(
    service: &RunMetricsService,
    task_id: &str,
    message_id: &str,
    observer: &crate::app_shell::streaming::ChannelObserver,
) {
    let snapshot = observer.metrics();
    let input = RunMetricsInput {
        task_id: task_id.to_string(),
        message_id: Some(message_id.to_string()),
        mode: if snapshot.mode.is_empty() {
            "fast".to_string()
        } else {
            snapshot.mode
        },
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
            "run metrics write failed (answer unaffected)"
        );
    }
}

/// 模型类失败是否可以用“原文抽取兜底”代替纯错误码。
/// 只覆盖“模型没给出可用答案”的失败;权限/覆盖/参数错误必须如实说明。
fn recoverable_by_extractive(error_code: &str) -> bool {
    matches!(
        error_code,
        "MODEL_TIMEOUT" | "MODEL_UNAVAILABLE" | "MODEL_OUTPUT_INVALID" | "ANSWER_MISSING"
    )
}

/// 用快照原文生成可见兜底答案(抽取式,标注原因与局限)。
///
/// 返回 true 表示已写入一条 completed 的 answer.v1;false 表示没有可用原文,
/// 调用方必须改走 `assistant_notice`(仍然可见)。
pub(crate) fn complete_with_extractive_fallback(
    context: &crate::context::service::ContextService,
    assistant_id: &str,
    snapshot_id: &str,
    error_code: &str,
    question: Option<&str>,
) -> bool {
    // 模型超时不能悄悄变成"回答已完成":用户会以为模型只是简要作答。
    // 先写一条可见失败通知(带重试/检查设置的下一步),再附上原文摘录作为参考。
    if error_code == "MODEL_TIMEOUT" {
        let _ = context.fail_assistant_message_with_notice(
            assistant_id,
            error_code,
            false,
            &notice_for_failure(error_code, false),
        );
        return false;
    }
    // 问候/能力问题不能用页码原文冒充 AI 回答;应显示模型不可用通知。
    if question
        .map(crate::retrieval_ai::service::is_conversational_question)
        .unwrap_or(false)
    {
        return false;
    }
    let quick = match context.quick_answer(snapshot_id, 3) {
        Ok(view) => view,
        Err(_) => return false,
    };
    let sentences: Vec<(String, Option<i64>)> = quick
        .sentences
        .iter()
        .map(|sentence| (sentence.text.clone(), sentence.page))
        .collect();
    let note = format!(
        "{};以下为当前范围原文摘录,不是 AI 生成的回答,也不代表已核对结论",
        fallback_reason_label(error_code)
    );
    let Some(answer) = crate::retrieval_ai::models::extractive_fallback_answer(&sentences, note)
    else {
        return false;
    };
    context
        .complete_assistant_message(assistant_id, &answer, None, None)
        .is_ok()
}

/// 兜底原因标签(紧凑,不写长句)。
fn fallback_reason_label(error_code: &str) -> &'static str {
    match error_code {
        "MODEL_TIMEOUT" => "模型超时,回答未完整",
        "MODEL_UNAVAILABLE" => "模型暂不可用",
        "MODEL_OUTPUT_INVALID" => "模型输出不可用",
        _ => "模型未返回可用答案",
    }
}

/// 统一失败落定:优先用原文抽取兜底成可见答案,否则写可见通知。
/// 无论哪条路径,占位消息都必须离开 pending 状态。
pub(crate) fn settle_failure(
    context: &crate::context::service::ContextService,
    assistant_id: &str,
    snapshot_id: &str,
    error_code: &str,
    canceled: bool,
    question: Option<&str>,
) -> bool {
    if !canceled
        && recoverable_by_extractive(error_code)
        && complete_with_extractive_fallback(
            context,
            assistant_id,
            snapshot_id,
            error_code,
            question,
        )
    {
        return true;
    }
    let _ = context.fail_assistant_message_with_notice(
        assistant_id,
        error_code,
        canceled,
        &notice_for_failure(error_code, canceled),
    );
    false
}

/// 失败兜底通知:保证每个已创建的回合都有一条可见助手消息。
pub(crate) fn notice_for_failure(error_code: &str, canceled: bool) -> AssistantNoticeV1 {
    let actions = match error_code {
        // 远程提供方 5xx/连接失败属于瞬态故障:首选"重试";持续失败再查设置。
        "MODEL_UNAVAILABLE" => vec![
            AssistantNoticeAction {
                id: "retry".into(),
                label: "重试".into(),
            },
            AssistantNoticeAction {
                id: "open_settings".into(),
                label: "检查模型设置".into(),
            },
        ],
        "PRIVACY_GRANT_REQUIRED" | "PRIVACY_EGRESS_DENIED" => vec![AssistantNoticeAction {
            id: "grant".into(),
            label: "授权远程模型".into(),
        }],
        _ => vec![AssistantNoticeAction {
            id: "retry".into(),
            label: "重试".into(),
        }],
    };
    AssistantNoticeV1::new(
        visible_failure_message(error_code),
        if canceled {
            "TASK_CANCELED".into()
        } else {
            error_code.to_string()
        },
        actions,
    )
}

/// 失败可见文案(不含模型正文/路径);前端也会按错误码映射,这里保证事件里有话可说。
pub(crate) fn visible_failure_message(error_code: &str) -> String {
    match error_code {
        "TASK_CANCELED" => "已取消本次回答;已生成的部分未保存。".into(),
        "MODEL_TIMEOUT" => {
            "模型在本轮超时未返回完整回答;已保留问题与范围,可重试或缩小范围后再问。".into()
        }
        "MODEL_UNAVAILABLE" => {
            "模型服务暂时不可用(可能是网络或提供方波动);问题与范围已保留,可直接重试。若持续失败,请检查模型设置。".into()
        }
        "PRIVACY_GRANT_REQUIRED" | "PRIVACY_EGRESS_DENIED" => {
            "尚未授权本次远程模型调用,请先授权或切换到本地模型。".into()
        }
        "CONTEXT_COVERAGE_INCOMPLETE" => {
            "当前范围的解析尚未覆盖完整;可以先用可用页回答,或等待解析完成。".into()
        }
        _ => format!("回答未完成(错误码:{error_code});问题与范围已保留,可重试。"),
    }
}

/// 读取历史的原始消息上限:原文窗口 + 摘要触发阈值。
/// 即使摘要尚未刷新,最近历史也不会掉出 Prompt;token 预算由 M09 再裁剪。
pub(crate) const HISTORY_MESSAGE_LIMIT: i64 =
    crate::context::summary::RECENT_HISTORY_WINDOW_TURNS as i64;

/// M07 消息 → M09 历史投影:只取用户原文与已完成答案;
/// 占位/失败消息不进入模型,图片引用/旧 data-URI 在进入前剥离。
pub(crate) fn recent_history(messages: &[MessageView]) -> Vec<ChatTurn> {
    let mut out = Vec::new();
    for message in messages {
        match (&message.role, &message.content) {
            (MessageRole::User, MessageContent::UserMessage { text }) => {
                let text = strip_markdown_images(text).trim().to_string();
                if !text.is_empty() {
                    out.push(ChatTurn {
                        role: ChatTurnRole::User,
                        text,
                    });
                }
            }
            (MessageRole::Assistant, MessageContent::Answer { content }) => {
                let text = content
                    .segments
                    .iter()
                    .map(|segment| segment.text.trim())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    out.push(ChatTurn {
                        role: ChatTurnRole::Assistant,
                        text,
                    });
                }
            }
            // P2:Agent 回答也进入后续追问历史(只取最终答复正文,不含步骤)。
            (MessageRole::Assistant, MessageContent::AgentAnswer { content }) => {
                let text = content
                    .answer
                    .segments
                    .iter()
                    .map(|segment| segment.text.trim())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    out.push(ChatTurn {
                        role: ChatTurnRole::Assistant,
                        text,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// 会话标题:优先展示文本的首个非空行;图片引用不进入标题。
/// 展示文本为空时回退纯文本 question。
pub(crate) fn conversation_title(display_text: &str, question: &str) -> String {
    // 先持有剥离后的 String,再取 &str,避免临时值在语句结束被释放(E0716)。
    let stripped = strip_markdown_images(display_text);
    let first_line = stripped
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if first_line.is_empty() {
        question.trim().to_string()
    } else {
        first_line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_notices_always_have_visible_text_and_action() {
        for code in [
            "TASK_CANCELED",
            "MODEL_UNAVAILABLE",
            "PRIVACY_GRANT_REQUIRED",
            "PRIVACY_EGRESS_DENIED",
            "CONTEXT_COVERAGE_INCOMPLETE",
            "INTERNAL_ERROR",
            "ANSWER_MISSING",
        ] {
            let notice = notice_for_failure(code, code == "TASK_CANCELED");
            assert!(!notice.message.trim().is_empty(), "{code} 必须有可见文案");
            assert!(!notice.actions.is_empty(), "{code} 必须给下一步动作");
            assert!(!visible_failure_message(code).trim().is_empty());
        }
    }

    #[test]
    fn conversation_title_uses_display_text_without_image_payload() {
        let display = "这个预算表怎么看?\n\n![截图](data:image/png;base64,AAAABBBB)";
        assert_eq!(
            conversation_title(display, "纯文本问题"),
            "这个预算表怎么看?"
        );
    }

    #[test]
    fn conversation_title_falls_back_to_question() {
        assert_eq!(
            conversation_title("![截图](data:image/png;base64,AAAA)", "纯文本问题"),
            "纯文本问题"
        );
        assert_eq!(conversation_title("   \n", "纯文本问题"), "纯文本问题");
    }
}
