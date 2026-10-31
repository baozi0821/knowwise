//! 会话滚动摘要的 Prompt 与校验(WBS-5)。
//!
//! M09 只负责把 M07 给出的"待压缩消息"组织成受预算约束的生成请求,
//! 并校验模型返回的摘要信封;摘要的持久化与覆盖范围推进由 M07 完成。
//! 摘要本身不是文档来源:系统指令明确禁止把它当证据引用。

use serde::Deserialize;

use crate::shared::AppError;

use super::budget::{estimate_tokens, truncate_to_token_budget};
use super::models::{ChatTurn, StructuredGenerationRequest, TaskKind};
use super::ports::{ConversationSummaryPlan, SummaryTurn};

pub const CONVERSATION_SUMMARY_SCHEMA_VERSION: &str = "conversation_summary.v1";
pub const CONVERSATION_SUMMARY_INSTRUCTION_VERSION: &str = "conversation-summary-instruction.v1";
/// 摘要正文上限(字符):约 1200 token,保证摘要层不会挤占检索层。
pub const MAX_SUMMARY_CHARS: usize = 1_200;
/// 单条历史消息进入摘要请求的字符上限。
pub const MAX_SUMMARY_TURN_CHARS: usize = 800;
/// 摘要请求的输入预算(摘要 + 历史消息 + 指令)。
pub const MAX_SUMMARY_INPUT_TOKENS: usize = 3_000;
/// 指令与 JSON 结构的固定预留 token。
const SUMMARY_INSTRUCTION_RESERVE: usize = 300;

/// 已按预算裁剪、可直接构造成生成请求的摘要输入。
#[derive(Debug, Clone)]
pub struct PreparedSummaryInput {
    pub turns: Vec<ChatTurn>,
    pub covered_up_to_message_id: String,
    pub previous_summary: Option<String>,
    pub question: String,
    /// 本轮未装下的待压缩消息数(下一轮继续;必须向用户说明)。
    pub skipped_turns: usize,
    /// 因单条过长被截断的消息数。
    pub truncated_turns: usize,
    /// 已有摘要因预算被截断(必须向用户说明)。
    pub previous_summary_truncated: bool,
}

/// 选择本轮要压缩的消息:从最早一条开始装入,预算不足时停在边界。
/// 只推进已装入消息的覆盖范围,绝不声称压缩了未装入的消息。
pub fn prepare_summary_input(plan: &ConversationSummaryPlan) -> Option<PreparedSummaryInput> {
    if plan.messages.is_empty() {
        return None;
    }
    let mut used = SUMMARY_INSTRUCTION_RESERVE;
    let mut truncated_turns = 0usize;
    let mut previous_summary = plan
        .previous_summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut previous_summary_truncated = false;
    if let Some(previous) = &previous_summary {
        let reserve = MAX_SUMMARY_INPUT_TOKENS / 2;
        if estimate_tokens(previous) > reserve {
            let (text, _) = truncate_to_token_budget(previous, reserve);
            previous_summary = Some(text);
            previous_summary_truncated = true;
        }
        used += estimate_tokens(previous_summary.as_deref().unwrap_or_default());
    }

    let mut turns = Vec::new();
    let mut covered = None;
    for message in &plan.messages {
        let (text, truncated) = truncate_summary_turn(message);
        let cost = estimate_tokens(&text);
        if used + cost > MAX_SUMMARY_INPUT_TOKENS {
            break;
        }
        if truncated {
            truncated_turns += 1;
        }
        used += cost;
        covered = Some(message.message_id.clone());
        turns.push(ChatTurn {
            role: message.role,
            text,
        });
    }
    let covered_up_to_message_id = covered?;
    let skipped_turns = plan.messages.len().saturating_sub(turns.len());
    let question = summary_question(previous_summary.as_deref(), &covered_up_to_message_id);
    Some(PreparedSummaryInput {
        turns,
        covered_up_to_message_id,
        previous_summary,
        question,
        skipped_turns,
        truncated_turns,
        previous_summary_truncated,
    })
}

fn truncate_summary_turn(message: &SummaryTurn) -> (String, bool) {
    let text = message.text.trim();
    if text.chars().count() <= MAX_SUMMARY_TURN_CHARS {
        return (text.to_string(), false);
    }
    let mut out: String = text.chars().take(MAX_SUMMARY_TURN_CHARS).collect();
    out.push_str("\n…(摘要输入已截断)");
    (out, true)
}

fn summary_question(previous_summary: Option<&str>, covered_up_to: &str) -> String {
    let previous = previous_summary
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("\n\n已有摘要(合并其中仍然相关的内容):\n{s}"))
        .unwrap_or_default();
    format!(
        "请把上面的对话记录压缩成一份会话摘要,供后续追问使用。\n\
         要求:\n\
         - 保留用户目标、已确认结论、关键数字与名称、未解决的问题;\n\
         - 摘要只是对话记录,不是文档原文证据,不得写成本文引用;\n\
         - 不要编造未出现的事实;\n\
         - 摘要不超过 {MAX_SUMMARY_CHARS} 字;\n\
         - 只输出 JSON(schemaVersion={CONVERSATION_SUMMARY_SCHEMA_VERSION}),\
         并原样回填 coveredUpToMessageId。\n\
         最后一条输入消息 id:{covered_up_to}{previous}"
    )
}

/// 构建滚动摘要生成请求(与问答共用 StructuredGenerationPort / EgressGateway)。
#[allow(clippy::too_many_arguments)]
pub fn build_conversation_summary_request(
    task_id: &str,
    attempt_id: &str,
    document_id: &str,
    snapshot_id: &str,
    locale: &str,
    prepared: &PreparedSummaryInput,
) -> StructuredGenerationRequest {
    StructuredGenerationRequest {
        task_id: task_id.to_string(),
        attempt_id: attempt_id.to_string(),
        document_id: document_id.to_string(),
        snapshot_id: snapshot_id.to_string(),
        schema_version: CONVERSATION_SUMMARY_SCHEMA_VERSION.to_string(),
        schema: conversation_summary_json_schema(),
        instruction_version: CONVERSATION_SUMMARY_INSTRUCTION_VERSION.to_string(),
        task_kind: TaskKind::ConversationSummary,
        inputs: Vec::new(),
        locale: locale.to_string(),
        question: prepared.question.clone(),
        history: prepared.turns.clone(),
        history_summary: None,
        attachments: Vec::new(),
        document_map: None,
        scope: crate::model_privacy::gateway::EgressScope::Task,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummaryRaw {
    schema_version: String,
    summary: String,
    covered_up_to_message_id: String,
}

/// 已校验的摘要正文。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedConversationSummary {
    pub text: String,
    pub tokens: usize,
    /// 模型输出超过上限时截断;调用方必须把该事实写入用户可见说明。
    pub truncated: bool,
}

/// 解析摘要信封:覆盖范围必须与本次输入的最后一条消息一致,
/// 防止模型把未看到的旧消息声称已压缩。
pub fn parse_conversation_summary(
    raw: &serde_json::Value,
    expected_up_to_message_id: &str,
) -> Result<ParsedConversationSummary, AppError> {
    let parsed: SummaryRaw = serde_json::from_value(raw.clone())
        .map_err(|_| invalid("conversation_summary_decode_failed"))?;
    if parsed.schema_version != CONVERSATION_SUMMARY_SCHEMA_VERSION {
        return Err(invalid("conversation_summary_schema_version_mismatch"));
    }
    if parsed.covered_up_to_message_id.trim() != expected_up_to_message_id {
        return Err(invalid("conversation_summary_coverage_mismatch"));
    }
    let text = parsed.summary.trim();
    if text.is_empty() {
        return Err(invalid("conversation_summary_empty"));
    }
    let (text, truncated) = if text.chars().count() > MAX_SUMMARY_CHARS {
        (
            text.chars().take(MAX_SUMMARY_CHARS).collect::<String>(),
            true,
        )
    } else {
        (text.to_string(), false)
    };
    let tokens = estimate_tokens(&text);
    Ok(ParsedConversationSummary {
        text,
        tokens,
        truncated,
    })
}

fn invalid(reason: &str) -> AppError {
    super::error::model_output_invalid(reason)
}

/// 摘要信封 JSON Schema。
pub fn conversation_summary_json_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["schemaVersion", "summary", "coveredUpToMessageId"],
        "additionalProperties": false,
        "properties": {
            "schemaVersion": {"const": CONVERSATION_SUMMARY_SCHEMA_VERSION},
            "summary": {"type": "string", "minLength": 1},
            "coveredUpToMessageId": {"type": "string", "minLength": 1}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(messages: Vec<(&str, &str)>) -> ConversationSummaryPlan {
        ConversationSummaryPlan {
            previous_summary: None,
            previous_up_to_message_id: None,
            previous_status: Some("ok".into()),
            previous_error_code: None,
            older_unsummarized_turns: messages.len(),
            messages: messages
                .into_iter()
                .map(|(id, text)| SummaryTurn {
                    message_id: id.into(),
                    role: super::super::models::ChatTurnRole::User,
                    text: text.into(),
                })
                .collect(),
        }
    }

    #[test]
    fn prepare_selects_oldest_messages_and_reports_truncation() {
        let long = "长".repeat(MAX_SUMMARY_TURN_CHARS + 10);
        let plan = plan(vec![
            ("m1", "第一轮问题"),
            ("m2", &long),
            ("m3", "第三轮问题"),
        ]);
        let prepared = prepare_summary_input(&plan).unwrap();
        assert_eq!(prepared.turns.len(), 3);
        assert_eq!(prepared.covered_up_to_message_id, "m3");
        assert_eq!(prepared.truncated_turns, 1);
        assert_eq!(prepared.skipped_turns, 0);
        assert!(prepared.question.contains("m3"));
    }

    #[test]
    fn prepare_stops_at_token_budget_without_claiming_more_coverage() {
        let huge = "超".repeat(2_000);
        let plan = plan(vec![
            ("m1", "旧问题"),
            ("m2", &huge),
            ("m3", &huge),
            ("m4", &huge),
            ("m5", &huge),
            ("m6", "新问题"),
        ]);
        let prepared = prepare_summary_input(&plan).unwrap();
        assert!(prepared.turns.len() < 6);
        assert_eq!(prepared.covered_up_to_message_id, "m4");
        assert_eq!(prepared.skipped_turns, 2);
        // 每条超长消息在进入摘要前截断;覆盖范围只推进到最后一条装入的消息。
        assert_eq!(prepared.truncated_turns, 3);
    }

    #[test]
    fn oversized_previous_summary_is_truncated_and_flagged() {
        let mut plan = plan(vec![("m1", "短问题")]);
        plan.previous_summary = Some("摘要".repeat(5_000));
        let prepared = prepare_summary_input(&plan).unwrap();
        assert!(prepared.previous_summary_truncated);
        assert!(prepared.question.contains("已有摘要"));
        assert_eq!(prepared.covered_up_to_message_id, "m1");
    }

    #[test]
    fn parse_requires_matching_coverage_and_bounds_summary() {
        let raw = serde_json::json!({
            "schemaVersion": CONVERSATION_SUMMARY_SCHEMA_VERSION,
            "summary": "预算阈值是 500 万元。",
            "coveredUpToMessageId": "m9"
        });
        let parsed = parse_conversation_summary(&raw, "m9").unwrap();
        assert!(parsed.text.contains("500"));
        assert!(!parsed.truncated);
        assert!(parsed.tokens >= 4);

        let wrong = parse_conversation_summary(&raw, "m8").unwrap_err();
        assert_eq!(wrong.code, "MODEL_OUTPUT_INVALID");

        let long = serde_json::json!({
            "schemaVersion": CONVERSATION_SUMMARY_SCHEMA_VERSION,
            "summary": "长".repeat(MAX_SUMMARY_CHARS + 5),
            "coveredUpToMessageId": "m9"
        });
        let parsed = parse_conversation_summary(&long, "m9").unwrap();
        assert!(parsed.truncated);
        assert_eq!(parsed.text.chars().count(), MAX_SUMMARY_CHARS);
    }

    #[test]
    fn summary_request_uses_dedicated_task_kind_and_no_sources() {
        let prepared = prepare_summary_input(&plan(vec![("m1", "问题一")])).unwrap();
        let request =
            build_conversation_summary_request("t1", "a1", "d1", "s1", "zh-CN", &prepared);
        assert_eq!(request.task_kind, TaskKind::ConversationSummary);
        assert_eq!(request.history.len(), 1);
        assert!(request.inputs.is_empty());
        assert!(request.history_summary.is_none());
        assert!(request.attachments.is_empty());
    }
}
