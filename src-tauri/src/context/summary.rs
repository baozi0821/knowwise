//! M07 会话滚动摘要(WBS-5 长上下文)。
//!
//! 职责边界:
//! - M07 决定"哪些历史消息应被压缩"(保留最近原文窗口、每次压缩多少);
//! - 摘要的模型调用由 M09 复用 StructuredGenerationPort/EgressGateway 完成;
//! - 本模块只读写 `conversation_summary`(m17),失败不阻塞主问答但记录状态。
//!
//! 分层窗口:保留最近 `SUMMARY_KEEP_RECENT_TURNS` 条有效消息作为原文;
//! 每次问题前若有至少 `SUMMARY_TRIGGER_TURNS` 条更早消息尚未摘要,
//! 则把其中最早的一批交给 M09 压缩。M13 读取最近
//! `RECENT_HISTORY_WINDOW_TURNS` 条消息,保证"来不及摘要"的少量消息
//! 仍能作为原文进入 Prompt(不静默丢失)。

use rusqlite::{params, Connection, OptionalExtension};

use crate::attachments::strip_markdown_images;
use crate::persistence::db_app_error;
use crate::shared::{new_trace_id, AppError, CoreErrorCode};

use super::models::{AnswerLifecycle, MessageContent, MessageRole, MessageRow, MESSAGE_COLS};

/// 保留最近多少条有效消息(用户/已完成回答,约 6 轮问答)作为原文。
pub const SUMMARY_KEEP_RECENT_TURNS: usize = 12;
/// 最近原文窗口之外积压多少条有效消息(约 2 轮问答)才触发一次压缩,
/// 避免每轮都调用模型;触发前少量积压仍在 M13 的历史窗口内。
pub const SUMMARY_TRIGGER_TURNS: usize = 4;
/// 单次摘要最多压缩多少条消息;未装入的下一轮继续。
pub const SUMMARY_MAX_SOURCE_TURNS: usize = 24;
/// M13 每次读取的历史消息上限 = 原文窗口 + 触发阈值:
/// 即使摘要尚未刷新,最近的历史也不会掉出原文窗口。
pub const RECENT_HISTORY_WINDOW_TURNS: usize = SUMMARY_KEEP_RECENT_TURNS + SUMMARY_TRIGGER_TURNS;

pub const SUMMARY_STATUS_OK: &str = "ok";
pub const SUMMARY_STATUS_FAILED: &str = "failed";

/// 已保存摘要(内部行)。
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationSummaryRow {
    pub conversation_id: String,
    pub up_to_message_id: Option<String>,
    pub summary_text: String,
    pub token_count: i64,
    pub status: String,
    pub error_code: Option<String>,
    pub updated_at: i64,
}

/// 一条待压缩的有效历史消息。
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryMessageRow {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
}

/// 摘要计划:已有摘要 + 下一批待压缩消息。
#[derive(Debug, Clone, Default)]
pub struct SummaryPlanRow {
    pub previous: Option<ConversationSummaryRow>,
    /// 本轮计划压缩的消息(时间升序,已排除最近原文窗口)。
    pub messages: Vec<SummaryMessageRow>,
    /// 最近原文窗口之外、尚未摘要的消息总数(可能大于 messages.len())。
    pub older_unsummarized_turns: usize,
}

fn not_found() -> AppError {
    AppError::new(CoreErrorCode::NotFound, new_trace_id())
}

fn invalid(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, new_trace_id())
        .with_detail("reason", reason.into())
}

fn ensure_conversation_tx(conn: &Connection, conversation_id: &str) -> Result<(), AppError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM conversation WHERE id=?1",
            [conversation_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    if exists.is_none() {
        return Err(not_found());
    }
    Ok(())
}

pub fn load_summary_tx(
    conn: &Connection,
    conversation_id: &str,
) -> Result<Option<ConversationSummaryRow>, AppError> {
    conn.query_row(
        "SELECT conversation_id, up_to_message_id, summary_text, token_count, status, \
         error_code, updated_at FROM conversation_summary WHERE conversation_id=?1",
        [conversation_id],
        |r| {
            Ok(ConversationSummaryRow {
                conversation_id: r.get(0)?,
                up_to_message_id: r.get(1)?,
                summary_text: r.get(2)?,
                token_count: r.get(3)?,
                status: r.get(4)?,
                error_code: r.get(5)?,
                updated_at: r.get(6)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn message_text(row: &MessageRow) -> Option<String> {
    let content: MessageContent = serde_json::from_str(&row.content_json).ok()?;
    match content {
        MessageContent::UserMessage { text } => {
            let text = strip_markdown_images(&text).trim().to_string();
            (!text.is_empty()).then_some(text)
        }
        MessageContent::Answer { content } if row.answer_status == AnswerLifecycle::Completed => {
            let text = content
                .segments
                .iter()
                .map(|segment| segment.text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        MessageContent::AgentAnswer { content }
            if row.answer_status == AnswerLifecycle::Completed =>
        {
            let text = content
                .answer
                .segments
                .iter()
                .map(|segment| segment.text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// 读取摘要并计算下一批待压缩消息。
pub fn build_plan_tx(conn: &Connection, conversation_id: &str) -> Result<SummaryPlanRow, AppError> {
    ensure_conversation_tx(conn, conversation_id)?;
    let previous = load_summary_tx(conn, conversation_id)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM message WHERE conversation_id=?1 \
             ORDER BY created_at, id"
        ))
        .map_err(|e| db_app_error(&e))?;
    let rows = stmt
        .query_map([conversation_id], |r| MessageRow::try_from(r))
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_app_error(&e))?;
    drop(stmt);

    let turns: Vec<SummaryMessageRow> = rows
        .iter()
        .filter_map(|row| {
            message_text(row).map(|text| SummaryMessageRow {
                id: row.id.clone(),
                role: row.role,
                text,
            })
        })
        .collect();

    let recent_start = turns.len().saturating_sub(SUMMARY_KEEP_RECENT_TURNS);
    let after_previous = previous
        .as_ref()
        .and_then(|summary| summary.up_to_message_id.as_deref())
        .and_then(|up_to| turns.iter().position(|turn| turn.id == up_to))
        .map(|index| index + 1)
        .unwrap_or(0);
    let source_start = after_previous.min(recent_start);
    let older_unsummarized_turns = recent_start.saturating_sub(source_start);
    let messages = if older_unsummarized_turns >= SUMMARY_TRIGGER_TURNS {
        turns[source_start..recent_start]
            .iter()
            .take(SUMMARY_MAX_SOURCE_TURNS)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    Ok(SummaryPlanRow {
        previous,
        messages,
        older_unsummarized_turns,
    })
}

/// 保存成功摘要并推进覆盖范围。失败不会覆盖旧摘要文本。
pub fn save_summary_tx(
    conn: &Connection,
    conversation_id: &str,
    up_to_message_id: &str,
    summary_text: &str,
    token_count: i64,
    now: i64,
) -> Result<(), AppError> {
    ensure_conversation_tx(conn, conversation_id)?;
    if summary_text.trim().is_empty() {
        return Err(invalid("summary_text_empty"));
    }
    let anchored: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM message WHERE id=?1 AND conversation_id=?2",
            params![up_to_message_id, conversation_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    if anchored.is_none() {
        return Err(invalid("summary_anchor_not_in_conversation"));
    }
    conn.execute(
        "INSERT INTO conversation_summary(conversation_id, up_to_message_id, summary_text, \
         token_count, status, error_code, created_at, updated_at) \
         VALUES (?1,?2,?3,?4,'ok',NULL,?5,?5) \
         ON CONFLICT(conversation_id) DO UPDATE SET \
         up_to_message_id=excluded.up_to_message_id, summary_text=excluded.summary_text, \
         token_count=excluded.token_count, status='ok', error_code=NULL, \
         updated_at=excluded.updated_at",
        params![
            conversation_id,
            up_to_message_id,
            summary_text.trim(),
            token_count.max(0),
            now
        ],
    )
    .map_err(|e| db_app_error(&e))?;
    Ok(())
}

/// 记录摘要失败:保留上次成功的摘要与覆盖范围,只更新状态/错误码。
pub fn record_failure_tx(
    conn: &Connection,
    conversation_id: &str,
    error_code: &str,
    now: i64,
) -> Result<(), AppError> {
    ensure_conversation_tx(conn, conversation_id)?;
    conn.execute(
        "INSERT INTO conversation_summary(conversation_id, up_to_message_id, summary_text, \
         token_count, status, error_code, created_at, updated_at) \
         VALUES (?1,NULL,'',0,'failed',?2,?3,?3) \
         ON CONFLICT(conversation_id) DO UPDATE SET \
         status='failed', error_code=excluded.error_code, updated_at=excluded.updated_at",
        params![conversation_id, error_code, now],
    )
    .map_err(|e| db_app_error(&e))?;
    Ok(())
}
