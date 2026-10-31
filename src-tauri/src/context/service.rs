//! M07 上下文快照与会话服务(《M07》§2–§8)。
//!
//! 关键不变量:
//! - M07 独占 ContextSnapshot 冻结;快照创建后禁止 UPDATE(触发器 +
//!   Repository 双重约束),修改范围必须创建新快照,旧消息引用不变。
//! - 局部范围(选区/位置/章节/页面/多来源)存 SnapshotBlock 精确列表;
//!   全文只在代次完整且 blockSetHash 已封存时绑定 parseGenerationId+blockSetHash,
//!   部分解析不得冻结全文、不得显示"全文已读取"。
//! - 笔记与 AI 草稿以文本快照进入(textSnapshot 复制当时正文),后续编辑
//!   不反写历史任务;AI 草稿保持 ai_draft 层级,sourceRole 由服务端推导。
//! - 预算超限不静默截断(CONTEXT_BUDGET_EXCEEDED);局部无结果绝不自动扩为全文。
//! - 消息不保存模型原始流与未校验 JSON;用户消息与 Task 同事务写入
//!   (create_user_message_tx 供 M09 在任务创建事务内调用);助手消息按 Task
//!   幂等完成。跨文档任务/快照写回被拒绝,旧任务结果仍回原文档会话。
//! - M07 不执行清除:clear_contribution_preview 只产出计划,M08 聚合、M01 执行;
//!   仍被消息、活动快照或注册保护器(产物)引用的快照保留。
//!
//! 跨模块读取说明:对 `document`/`document_version`(M02)、`content_block`/
//! `parse_generation`/`parse_region`/`content_block_fts`/`vector_embedding`(M04)、
//! `note`(M06)、`task`(M01)只做单表/显式 JOIN 的只读校验,与 evidence 模块
//! 读取 document/content_block 的既有先例一致;写入严格限于本模块表。
//!
//! 版本绑定:MVP 冻结与预览以文档当前版本为准(《M07》§8"校验当前版本"),
//! 选区草案自带版本,与当前版本不一致即拒绝,不悄悄换用新版本文件。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension};

use crate::evidence::models::RawSelectionInput;
use crate::notes::models::{NoteOrigin, SourceRole};
use crate::parse_index::{ParseIndexService, SelectionBlockRequest};
use crate::persistence::{db_app_error, Db};
use crate::shared::dto::DeleteResult;
use crate::shared::locator::SourceLocator;
use crate::shared::paging::{decode_cursor, encode_cursor};
use crate::shared::textnorm::sha256_str;
use crate::shared::{
    AppError, Clock, CoreErrorCode, DomainEvent, DomainEventType, EventSink, IdGenerator, Page,
    PageRequest,
};

use super::export::ConversationExportData;
use super::models::*;
use super::summary::{
    build_plan_tx, load_summary_tx, record_failure_tx, save_summary_tx, ConversationSummaryRow,
    SummaryPlanRow,
};

/// 默认上下文 Token 预算(M0 实测前基线;超限不截断)。
pub const DEFAULT_CONTEXT_TOKEN_BUDGET: i64 = 65_536;
/// 单快照来源上限(去重后)。
pub const MAX_SOURCES_PER_SNAPSHOT: usize = 32;
/// 局部快照显式块上限(防滥用硬界;真实约束是 Token 预算)。
pub const MAX_SNAPSHOT_BLOCKS: usize = 20_000;
/// 显示名称截断长度。
const NAME_MAX_CHARS: usize = 24;

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

fn cerr(code: ContextErrorCode) -> AppError {
    code.to_app_error()
}

// —— Token 估算(CJK 主导启发式;预览与冻结共用,不伪报检索候选)——

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x30FF      // CJK 标点与假名
        | 0x3400..=0x4DBF    // 扩展 A
        | 0x4E00..=0x9FFF    // CJK 统一表意
        | 0xAC00..=0xD7AF    // 谚文
        | 0xF900..=0xFAFF    // 兼容表意
        | 0xFF00..=0xFFEF    // 全角形式
    )
}

/// Token 估算:CJK 字符约 1 token/字,其余约 4 字符/token。
/// 仅用于预算提示;实际送入模型的文字量由 RetrievalManifest 记录。
pub fn estimate_tokens(text: &str) -> i64 {
    let mut cjk = 0i64;
    let mut other = 0i64;
    for c in text.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + (other + 3) / 4
}

// —— 跨模块只读校验(M02/M04/M01 表)——

fn document_exists_tx(conn: &Connection, document_id: &str) -> Result<bool, AppError> {
    let exists: Option<i64> = conn
        .query_row("SELECT 1 FROM document WHERE id=?1", [document_id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| db_app_error(&e))?;
    Ok(exists.is_some())
}

/// 校验版本归属与指纹状态(版本属于文档且 fingerprint=ready)。
fn validate_version_tx(
    conn: &Connection,
    document_id: &str,
    version_id: &str,
) -> Result<(), AppError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT document_id, fingerprint_status FROM document_version WHERE id=?1",
            [version_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    let (owner, fp) = row.ok_or_else(|| err(CoreErrorCode::NotFound))?;
    if owner != document_id {
        return Err(err(CoreErrorCode::NotFound));
    }
    if fp != "ready" {
        return Err(err(CoreErrorCode::IndexGenerationUnavailable)
            .with_detail("reason", "fingerprint_not_ready".into()));
    }
    Ok(())
}

/// 文档当前版本 ID。
fn current_version_id_tx(conn: &Connection, document_id: &str) -> Result<String, AppError> {
    let vid: Option<String> = conn
        .query_row(
            "SELECT current_version_id FROM document WHERE id=?1",
            [document_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    vid.filter(|v| !v.is_empty())
        .ok_or_else(|| err(CoreErrorCode::NotFound))
}

/// 冻结时点的当前解析代次(可能不存在:选区问答不要求全文解析)。
struct GenerationBrief {
    id: String,
    status: String,
    block_set_hash: Option<String>,
}

fn current_generation_tx(
    conn: &Connection,
    version_id: &str,
) -> Result<Option<GenerationBrief>, AppError> {
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT id, status, block_set_hash FROM parse_generation \
             WHERE document_version_id=?1 AND is_current=1 LIMIT 1",
            [version_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    Ok(row.map(|(id, status, block_set_hash)| GenerationBrief {
        id,
        status,
        block_set_hash,
    }))
}

fn parse_state_of(gen: Option<&GenerationBrief>) -> &'static str {
    match gen {
        None => "not_started",
        Some(g) => match g.status.as_str() {
            "complete" => "complete",
            "partial" => "partial",
            "failed" | "canceled" => "failed",
            _ => "in_progress",
        },
    }
}

/// 只读块引用(M04 content_block;与 evidence 的只读模式一致)。
struct BlockRef {
    id: String,
    document_version_id: String,
    parse_generation_id: Option<String>,
    origin: String,
    block_key: String,
    ordinal: Option<i64>,
    text: String,
    heading_path_json: String,
    locator_json: String,
}

fn load_block_tx(conn: &Connection, block_id: &str) -> Result<Option<BlockRef>, AppError> {
    conn.query_row(
        "SELECT id, document_version_id, parse_generation_id, origin, block_key, ordinal, \
         text, heading_path_json, locator_json FROM content_block WHERE id=?1",
        [block_id],
        |r| {
            Ok(BlockRef {
                id: r.get(0)?,
                document_version_id: r.get(1)?,
                parse_generation_id: r.get(2)?,
                origin: r.get(3)?,
                block_key: r.get(4)?,
                ordinal: r.get(5)?,
                text: r.get(6)?,
                heading_path_json: r.get(7)?,
                locator_json: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

/// 块必须属于冻结版本;跨文档/跨版本块 ID 一律拒绝(《M07》§8)。
fn require_block_in_version_tx(
    conn: &Connection,
    document_id: &str,
    version_id: &str,
    block_id: &str,
) -> Result<BlockRef, AppError> {
    let Some(block) = load_block_tx(conn, block_id)? else {
        return Err(err(CoreErrorCode::NotFound).with_detail("reason", "block_not_found".into()));
    };
    if block.document_version_id != version_id {
        let owner: Option<String> = conn
            .query_row(
                "SELECT document_id FROM document_version WHERE id=?1",
                [&block.document_version_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| db_app_error(&e))?;
        let reason = if owner.as_deref() != Some(document_id) {
            "cross_document_block"
        } else {
            "block_version_mismatch"
        };
        return Err(err(CoreErrorCode::InvalidRequest).with_detail("reason", reason.into()));
    }
    Ok(block)
}

// —— 显示名称 ——

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// 范围显示名称(不含动态计数;计数由 summary 字段单独展示)。
fn display_name_of(scope: &ScopeDraft) -> String {
    match scope {
        ScopeDraft::Selection { .. } => "当前选区".to_string(),
        ScopeDraft::CurrentPosition { .. } => "当前段落".to_string(),
        ScopeDraft::Section { heading_path, .. } => {
            // 显示最近一级标题;重复标题的 #N 只用于定位消歧,不进入用户文案。
            let last = heading_path
                .last()
                .map(|title| title.split('#').next().unwrap_or(title))
                .unwrap_or("本节");
            format!("章节:{}", truncate_chars(last, NAME_MAX_CHARS))
        }
        ScopeDraft::Pages {
            start_page,
            end_page,
        } => {
            if start_page == end_page {
                format!("第 {start_page} 页")
            } else {
                format!("第 {start_page}–{end_page} 页")
            }
        }
        ScopeDraft::FullDocument { .. } => "全文".to_string(),
        ScopeDraft::Sources { sources } => format!("{} 个来源", sources.len()),
    }
}

// —— 来源解析(冻结与预览共享)——

/// 解析后的来源:文本快照 + 服务端推导的层级。
struct ResolvedSource {
    source_type: ContextSourceType,
    source_id: String,
    source_version: Option<String>,
    source_role: SourceRole,
    display_name: String,
    text_snapshot: String,
    locator_snapshot_json: Option<String>,
    content_hash: String,
    /// content_block/selection 来源同时进入显式块集合。
    block_id: Option<String>,
}

impl ResolvedSource {
    fn canonical_json(&self) -> serde_json::Value {
        serde_json::json!({
            "type": self.source_type.as_str(),
            "id": self.source_id,
            "role": self.source_role.as_str(),
            "version": self.source_version,
            "contentHash": self.content_hash,
        })
    }
}

fn role_of_note_origin(origin: &NoteOrigin) -> SourceRole {
    match origin {
        // AI 草稿即使保存为笔记仍保持 ai_draft 层级;
        // 用户明确编辑(user_edited_ai)后归 user_note,界面经 M06 origin 保留 AI 说明。
        NoteOrigin::AiDraft => SourceRole::AiDraft,
        NoteOrigin::User | NoteOrigin::UserEditedAi => SourceRole::UserNote,
    }
}

/// 解析单条来源草案(只读);selection 的按需块由调用方在事务外确保后
/// 经 `ensured_selection`(blockId, text, locatorJson)传入。
fn resolve_source_tx(
    conn: &Connection,
    document_id: &str,
    version_id: &str,
    draft: &ContextSourceDraft,
    ensured_selection: Option<&(String, String, Option<String>)>,
) -> Result<ResolvedSource, AppError> {
    match draft.source_type {
        ContextSourceType::ContentBlock => {
            let block =
                require_block_in_version_tx(conn, document_id, version_id, &draft.source_id)?;
            Ok(ResolvedSource {
                source_type: ContextSourceType::ContentBlock,
                source_id: block.id.clone(),
                source_version: None,
                source_role: SourceRole::Original,
                display_name: format!("原文块 {}", truncate_chars(&block.block_key, 16)),
                text_snapshot: block.text.clone(),
                locator_snapshot_json: Some(block.locator_json.clone()),
                content_hash: sha256_str(&block.text),
                block_id: Some(block.id),
            })
        }
        ContextSourceType::Selection => {
            let (block_id, text, locator_json) = match ensured_selection {
                Some((bid, text, locator_json)) => {
                    // 冻结路径:复核确保结果与版本一致(跨文档块拒绝)。
                    require_block_in_version_tx(conn, document_id, version_id, bid)?;
                    (Some(bid.clone()), text.clone(), locator_json.clone())
                }
                None => {
                    // 预览路径:不创建块,按草案文本估算。
                    let text = draft
                        .text
                        .as_deref()
                        .filter(|t| !t.trim().is_empty())
                        .ok_or_else(|| {
                            err(CoreErrorCode::InvalidRequest)
                                .with_detail("reason", "selection_text_required".into())
                        })?
                        .to_string();
                    let locator_json = draft
                        .locator
                        .as_ref()
                        .and_then(|l| serde_json::to_string(l).ok());
                    (None, text, locator_json)
                }
            };
            Ok(ResolvedSource {
                source_type: ContextSourceType::Selection,
                source_id: block_id.clone().unwrap_or_else(|| draft.source_id.clone()),
                source_version: None,
                source_role: SourceRole::Original,
                display_name: format!("选区「{}」", truncate_chars(&text, NAME_MAX_CHARS)),
                text_snapshot: text.clone(),
                locator_snapshot_json: locator_json,
                content_hash: sha256_str(&text),
                block_id,
            })
        }
        ContextSourceType::Note => {
            // 笔记必须复制当时正文(《M07》§1/§4):body 快照 + revision 版本。
            let row: Option<(String, i64, String)> = conn
                .query_row(
                    "SELECT body, revision, origin FROM note WHERE id=?1 AND document_id=?2",
                    params![draft.source_id, document_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let (body, revision, origin) = row.ok_or_else(|| {
                err(CoreErrorCode::NotFound).with_detail("reason", "note_not_found".into())
            })?;
            let origin =
                NoteOrigin::parse_db(&origin).ok_or_else(|| err(CoreErrorCode::Internal))?;
            Ok(ResolvedSource {
                source_type: ContextSourceType::Note,
                source_id: draft.source_id.clone(),
                source_version: Some(revision.to_string()),
                source_role: role_of_note_origin(&origin),
                display_name: format!("笔记「{}」", truncate_chars(&body, NAME_MAX_CHARS)),
                text_snapshot: body,
                locator_snapshot_json: None,
                content_hash: sha256_str(&format!("{}#{}", draft.source_id, revision)),
                block_id: None,
            })
        }
        ContextSourceType::Message => {
            let row: Option<(String, String)> = conn
                .query_row(
                    "SELECT m.content_json, m.role FROM message m \
                     JOIN conversation c ON c.id = m.conversation_id \
                     WHERE m.id=?1 AND c.document_id=?2",
                    params![draft.source_id, document_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let (content_json, role) = row.ok_or_else(|| {
                err(CoreErrorCode::NotFound).with_detail("reason", "message_not_found".into())
            })?;
            let text = extract_message_text(&content_json);
            let role = if role == "assistant" {
                SourceRole::AiDraft
            } else {
                SourceRole::UserNote
            };
            Ok(ResolvedSource {
                source_type: ContextSourceType::Message,
                source_id: draft.source_id.clone(),
                source_version: None,
                source_role: role,
                display_name: format!("消息「{}」", truncate_chars(&text, NAME_MAX_CHARS)),
                text_snapshot: text,
                locator_snapshot_json: None,
                content_hash: sha256_str(&format!("{}#{}", draft.source_id, content_json)),
                block_id: None,
            })
        }
        // artifact_version 来源:冻结产物版本的正文快照。
        // - 允许引用「总资料库」里其他文档产生的资料(用户显式选择);
        //   产物正文作为 AI 草稿来源进入本轮快照,不冒充原文事实;
        // - 冻结后快照保存文本与版本号,产物删除/升版后旧引用仍可读,
        //   但 `source_available` 会变 false,由 UI 明确标注失效;
        // - 正文不写普通日志(与其它来源一致)。
        ContextSourceType::ArtifactVersion => {
            let row: Option<(String, i64, String, String)> = conn
                .query_row(
                    "SELECT v.artifact_id, v.version_no, v.schema_version, v.content_json \
                     FROM artifact_version v JOIN artifact a ON a.id = v.artifact_id \
                     WHERE v.id=?1",
                    params![draft.source_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let (artifact_id, version_no, schema_version, content_json) = row.ok_or_else(|| {
                err(CoreErrorCode::NotFound)
                    .with_detail("reason", "artifact_version_not_found".into())
            })?;
            let text =
                crate::artifacts::models::artifact_content_text(&schema_version, &content_json);
            if text.trim().is_empty() {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "artifact_version_empty".into()));
            }
            let title: String = conn
                .query_row(
                    "SELECT title FROM artifact WHERE id=?1",
                    params![artifact_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?
                .unwrap_or_else(|| "资料".to_string());
            Ok(ResolvedSource {
                source_type: ContextSourceType::ArtifactVersion,
                source_id: draft.source_id.clone(),
                // 版本号进快照:旧引用可定位到具体版本,不随产物升版漂移。
                source_version: Some(version_no.to_string()),
                // 产物正文是 AI 生成内容,不是原文事实。
                source_role: SourceRole::AiDraft,
                display_name: format!(
                    "资料「{}」(第 {} 版)",
                    truncate_chars(&title, NAME_MAX_CHARS),
                    version_no
                ),
                text_snapshot: text,
                locator_snapshot_json: None,
                content_hash: sha256_str(&format!(
                    "{}#{}#{}",
                    draft.source_id, version_no, content_json
                )),
                block_id: None,
            })
        }
        ContextSourceType::Annotation => {
            let (note_text, exact_text, locator_json): (String, String, String) = conn
                .query_row(
                    "SELECT a.note_text, an.exact_text, an.primary_locator_json                      FROM annotation a JOIN anchor an ON an.id = a.anchor_id                      WHERE a.id=?1 AND a.document_id=?2",
                    [&draft.source_id, document_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?
                .ok_or_else(|| {
                    err(CoreErrorCode::NotFound)
                        .with_detail("reason", "annotation_not_found".into())
                })?;
            let text = if note_text.trim().is_empty() {
                format!("[标注原文] {exact_text}")
            } else {
                format!(
                    "[标注原文] {exact_text}
[标注内容] {note_text}"
                )
            };
            Ok(ResolvedSource {
                source_type: ContextSourceType::Annotation,
                source_id: draft.source_id.clone(),
                source_version: None,
                // 标注/笔记是用户内容,不是原文事实。
                source_role: SourceRole::UserNote,
                display_name: "标注".to_string(),
                text_snapshot: text.clone(),
                locator_snapshot_json: Some(locator_json),
                content_hash: sha256_str(&text),
                block_id: None,
            })
        }
    }
}

/// 从 contentJson 提取文本快照(用户文本或回答段落拼接)。
pub fn extract_message_text(content_json: &str) -> String {
    match serde_json::from_str::<MessageContent>(content_json) {
        Ok(MessageContent::UserMessage { text }) => text,
        Ok(MessageContent::Answer { content }) => content
            .segments
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn source_exists_tx(conn: &Connection, source_type: &ContextSourceType, source_id: &str) -> bool {
    let sql = match source_type {
        ContextSourceType::ContentBlock | ContextSourceType::Selection => {
            "SELECT 1 FROM content_block WHERE id=?1"
        }
        ContextSourceType::Note => "SELECT 1 FROM note WHERE id=?1",
        ContextSourceType::Message => "SELECT 1 FROM message WHERE id=?1",
        ContextSourceType::ArtifactVersion => "SELECT 1 FROM artifact_version WHERE id=?1",
        ContextSourceType::Annotation => "SELECT 1 FROM annotation WHERE id=?1",
    };
    conn.query_row(sql, [source_id], |_| Ok(()))
        .optional()
        .is_ok_and(|x| x.is_some())
}

// —— 代次块(章节/页面解析用)——

struct GenBlock {
    id: String,
    ordinal: Option<i64>,
    text: String,
    heading_path: Vec<String>,
    page: Option<i64>,
}

fn load_generation_blocks_tx(
    conn: &Connection,
    generation_id: &str,
) -> Result<Vec<GenBlock>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, ordinal, text, heading_path_json, locator_json FROM content_block \
             WHERE parse_generation_id=?1 AND origin IN ('parser','ocr')",
        )
        .map_err(|e| db_app_error(&e))?;
    let rows: Vec<(String, Option<i64>, String, String, String)> = stmt
        .query_map([generation_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_app_error(&e))?;
    drop(stmt);
    let mut blocks: Vec<GenBlock> = rows
        .into_iter()
        .map(|(id, ordinal, text, heading_json, locator_json)| GenBlock {
            heading_path: serde_json::from_str(&heading_json).unwrap_or_default(),
            page: serde_json::from_str::<SourceLocator>(&locator_json)
                .ok()
                .and_then(|l| l.pdf_page()),
            id,
            ordinal,
            text,
        })
        .collect();
    blocks.sort_by(|a, b| a.ordinal.cmp(&b.ordinal).then_with(|| a.id.cmp(&b.id)));
    Ok(blocks)
}

/// 页区域状态:region_key 尾部数字 → 状态。
fn page_region_status_tx(
    conn: &Connection,
    generation_id: &str,
) -> Result<HashMap<i64, String>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT region_key, status FROM parse_region \
             WHERE generation_id=?1 AND region_kind='page'",
        )
        .map_err(|e| db_app_error(&e))?;
    let rows: Vec<(String, String)> = stmt
        .query_map([generation_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_app_error(&e))?;
    drop(stmt);
    let mut map = HashMap::new();
    for (key, status) in rows {
        if let Some(no) = key.rsplit(':').next().and_then(|s| s.parse::<i64>().ok()) {
            map.insert(no, status);
        }
    }
    Ok(map)
}

// —— 范围解析 ——

struct ResolvedScope {
    block_ids: Vec<String>,
    sources: Vec<ResolvedSource>,
    parse_generation_id: Option<String>,
    block_set_hash: Option<String>,
    /// 全文冻结前置条件(代次完整且 blockSetHash 已封存)。
    full_document_ready: Option<bool>,
}

/// 冻结/预览解析输入。
struct ScopeInput<'a> {
    document_id: &'a str,
    version_id: &'a str,
    scope: &'a ScopeDraft,
    /// 冻结语义(确认全文、空范围报错);预览宽松(允许未确认全文以展示覆盖)。
    strict: bool,
    /// 幂等键(仅冻结路径使用;预览传空)。
    idempotency_key: &'a str,
    /// 事务外已确保的按需块:键 → blockId。
    /// selection 范围键 "sel:{blockOrder}";sources 范围键 "draft:{sourceId}"。
    ensured: Vec<(String, String)>,
}

/// 解析范围到显式块与来源(冻结与预览共用;只读,不创建块)。
/// 返回 (解析结果, 文本列表) —— 文本列表用于预算估算。
fn resolve_scope_tx(
    conn: &Connection,
    input: &ScopeInput<'_>,
) -> Result<(ResolvedScope, Vec<String>), AppError> {
    let gen = current_generation_tx(conn, input.version_id)?;
    let mut resolved = ResolvedScope {
        block_ids: Vec::new(),
        sources: Vec::new(),
        parse_generation_id: gen.as_ref().map(|g| g.id.clone()),
        block_set_hash: None,
        full_document_ready: None,
    };
    let mut texts: Vec<String> = Vec::new();

    let push_block = |resolved: &mut ResolvedScope, texts: &mut Vec<String>, block: BlockRef| {
        if !resolved.block_ids.contains(&block.id) {
            resolved.block_ids.push(block.id.clone());
            texts.push(block.text.clone());
        }
    };

    match input.scope {
        ScopeDraft::Selection { raw_selection } => {
            let raw = raw_selection.as_ref();
            if raw.blocks.is_empty() {
                return Err(
                    err(CoreErrorCode::InvalidRequest).with_detail("reason", "empty_scope".into())
                );
            }
            let mut ordered: Vec<&crate::evidence::models::RawSelectionBlockInput> =
                raw.blocks.iter().collect();
            ordered.sort_by_key(|b| b.block_order);
            for b in &ordered {
                match input
                    .ensured
                    .iter()
                    .find(|(k, _)| k == &format!("sel:{}", b.block_order))
                {
                    Some((_, block_id)) => {
                        let block = require_block_in_version_tx(
                            conn,
                            input.document_id,
                            input.version_id,
                            block_id,
                        )?;
                        push_block(&mut resolved, &mut texts, block);
                    }
                    // 预览路径:按草案文本估算,不创建块。
                    None => texts.push(b.text.clone()),
                }
            }
        }
        ScopeDraft::CurrentPosition { block_id, .. } => {
            let block =
                require_block_in_version_tx(conn, input.document_id, input.version_id, block_id)?;
            push_block(&mut resolved, &mut texts, block);
        }
        ScopeDraft::Section {
            heading_path,
            block_ids,
        } => {
            if heading_path.is_empty() && block_ids.as_deref().unwrap_or(&[]).is_empty() {
                return Err(
                    err(CoreErrorCode::InvalidRequest).with_detail("reason", "empty_scope".into())
                );
            }
            match block_ids {
                Some(ids) if !ids.is_empty() => {
                    let mut refs = Vec::new();
                    for id in ids {
                        let block = require_block_in_version_tx(
                            conn,
                            input.document_id,
                            input.version_id,
                            id,
                        )?;
                        refs.push((block.ordinal, block));
                    }
                    refs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
                    for (_, block) in refs {
                        push_block(&mut resolved, &mut texts, block);
                    }
                }
                _ => {
                    let Some(g) = &gen else {
                        return Err(err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "no_parsed_blocks".into()));
                    };
                    let blocks = load_generation_blocks_tx(conn, &g.id)?;
                    let n = heading_path.len();
                    for b in blocks {
                        if b.heading_path.len() >= n
                            && b.heading_path[..n] == heading_path[..]
                            && !resolved.block_ids.contains(&b.id)
                        {
                            resolved.block_ids.push(b.id.clone());
                            texts.push(b.text.clone());
                        }
                    }
                }
            }
            if resolved.block_ids.is_empty() {
                return Err(
                    err(CoreErrorCode::InvalidRequest).with_detail("reason", "empty_scope".into())
                );
            }
        }
        ScopeDraft::Pages {
            start_page,
            end_page,
        } => {
            if *start_page < 1 || *end_page < *start_page {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "invalid_page_range".into()));
            }
            let Some(g) = &gen else {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "no_parsed_blocks".into()));
            };
            for b in load_generation_blocks_tx(conn, &g.id)? {
                if b.page.is_some_and(|p| p >= *start_page && p <= *end_page)
                    && !resolved.block_ids.contains(&b.id)
                {
                    resolved.block_ids.push(b.id.clone());
                    texts.push(b.text.clone());
                }
            }
        }
        ScopeDraft::FullDocument { confirmed } => {
            if input.strict && !*confirmed {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "full_document_requires_confirmation".into()));
            }
            let ready = match &gen {
                Some(g) => g.status == "complete" && g.block_set_hash.is_some(),
                None => false,
            };
            resolved.full_document_ready = Some(ready);
            if ready {
                let g = gen.as_ref().expect("ready implies generation");
                resolved.parse_generation_id = Some(g.id.clone());
                resolved.block_set_hash = g.block_set_hash.clone();
            }
        }
        ScopeDraft::Sources { sources } => {
            if sources.is_empty() {
                return Err(
                    err(CoreErrorCode::InvalidRequest).with_detail("reason", "empty_scope".into())
                );
            }
            if sources.len() > MAX_SOURCES_PER_SNAPSHOT {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "source_limit".into())
                    .with_detail("max", (MAX_SOURCES_PER_SNAPSHOT as i64).into()));
            }
            let mut seen: Vec<(ContextSourceType, String)> = Vec::new();
            for draft in sources {
                // 去重但保留来源层级:同一 (type,id) 只留一份;
                // 同一文本既是原文又是笔记时不合并、不提升层级。
                let key = (draft.source_type, draft.source_id.clone());
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                let ensured_selection: Option<(String, String, Option<String>)> =
                    if draft.source_type == ContextSourceType::Selection {
                        input
                            .ensured
                            .iter()
                            .find(|(k, _)| k == &format!("draft:{}", draft.source_id))
                            .map(|(_, block_id)| {
                                let text = draft.text.clone().unwrap_or_default();
                                let locator_json = draft
                                    .locator
                                    .as_ref()
                                    .and_then(|l| serde_json::to_string(l).ok());
                                (block_id.clone(), text, locator_json)
                            })
                    } else {
                        None
                    };
                let src = resolve_source_tx(
                    conn,
                    input.document_id,
                    input.version_id,
                    draft,
                    ensured_selection.as_ref(),
                )?;
                texts.push(src.text_snapshot.clone());
                if let Some(bid) = &src.block_id {
                    if !resolved.block_ids.contains(bid) {
                        resolved.block_ids.push(bid.clone());
                    }
                }
                resolved.sources.push(src);
            }
            if resolved.block_ids.is_empty() && resolved.sources.is_empty() {
                return Err(
                    err(CoreErrorCode::InvalidRequest).with_detail("reason", "empty_scope".into())
                );
            }
        }
    }

    // 局部快照的代次按实际块归属推导:全部块同一代次 → 该代次;
    // 混合(含按需块)→ 留空,不伪造一致性。
    if !resolved.block_ids.is_empty() && resolved.block_set_hash.is_none() {
        let mut gens: Vec<Option<String>> = Vec::new();
        for bid in &resolved.block_ids {
            if let Some(b) = load_block_tx(conn, bid)? {
                if !gens.contains(&b.parse_generation_id) {
                    gens.push(b.parse_generation_id);
                }
            }
        }
        resolved.parse_generation_id = if gens.len() == 1 {
            gens[0].clone()
        } else {
            None
        };
    }
    Ok((resolved, texts))
}

fn scope_label_of(scope: &ScopeDraft) -> String {
    match scope {
        ScopeDraft::Selection { .. } => "选区".into(),
        ScopeDraft::CurrentPosition { .. } => "当前位置".into(),
        ScopeDraft::Section { heading_path, .. } => heading_path
            .last()
            .map(|heading| format!("本节 · {heading}"))
            .unwrap_or_else(|| "本节".into()),
        ScopeDraft::Pages {
            start_page,
            end_page,
        } => {
            if start_page == end_page {
                format!("第 {start_page} 页")
            } else {
                format!("第 {start_page}-{end_page} 页")
            }
        }
        ScopeDraft::FullDocument { .. } => "全文".into(),
        ScopeDraft::Sources { .. } => "所选来源".into(),
    }
}

fn extract_sentences(text: &str, max: usize) -> Vec<String> {
    text.split(['。', '！', '？', '!', '?', '\n', ';', '；'])
        .map(str::trim)
        .filter(|sentence| {
            let len = sentence.chars().count();
            (8..=240).contains(&len)
        })
        .take(max)
        .map(|sentence| format!("{sentence}。"))
        .collect()
}

// —— 覆盖摘要 ——

fn build_coverage_tx(
    conn: &Connection,
    gen: Option<&GenerationBrief>,
    scope: &ScopeDraft,
    _resolved: &ResolvedScope,
) -> Result<CoverageSummary, AppError> {
    let parse_state = parse_state_of(gen);
    let mut coverage = CoverageSummary::empty(parse_state);
    match scope {
        ScopeDraft::FullDocument { .. } => {
            let Some(g) = gen else {
                coverage.notes.push("no_generation".into());
                return Ok(coverage);
            };
            let mut stmt = conn
                .prepare("SELECT region_key, status FROM parse_region WHERE generation_id=?1")
                .map_err(|e| db_app_error(&e))?;
            let rows: Vec<(String, String)> = stmt
                .query_map([&g.id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            for (key, status) in rows {
                match status.as_str() {
                    "complete" => coverage.completed_regions.push(key),
                    "partial" => coverage.partial_regions.push(key),
                    _ => coverage.missing_regions.push(key),
                }
            }
            coverage.scope_covered = g.status == "complete" && coverage.missing_regions.is_empty();
            if g.block_set_hash.is_none() {
                coverage.notes.push("generation_unsealed".into());
            }
        }
        ScopeDraft::Pages {
            start_page,
            end_page,
        } => {
            let statuses = match gen {
                Some(g) => page_region_status_tx(conn, &g.id)?,
                None => HashMap::new(),
            };
            for page in *start_page..=*end_page {
                match statuses.get(&page).map(|s| s.as_str()) {
                    Some("complete") => coverage.completed_regions.push(format!("page:{page}")),
                    Some("partial") => {
                        coverage.partial_regions.push(format!("page:{page}"));
                        coverage.missing_regions.push(format!("page:{page}"));
                    }
                    _ => coverage.missing_regions.push(format!("page:{page}")),
                }
            }
            coverage.scope_covered = coverage.missing_regions.is_empty();
        }
        // 显式块与来源已逐一校验存在且版本一致,范围内完整。
        _ => {
            coverage.scope_covered = true;
        }
    }
    Ok(coverage)
}

// —— canonical hash ——

fn canonical_hash_of(
    document_id: &str,
    version_id: &str,
    gen_id: Option<&str>,
    scope: &ScopeDraft,
    resolved: &ResolvedScope,
    coverage: &CoverageSummary,
    token_estimate: i64,
) -> String {
    // serde_json 默认按 key 排序序列化,规范串确定。
    let canonical = serde_json::json!({
        "schemaVersion": "context_canonical.v1",
        "documentId": document_id,
        "documentVersionId": version_id,
        "parseGenerationId": gen_id,
        "scopeKind": scope.kind().as_str(),
        "scope": serde_json::to_value(scope).unwrap_or(serde_json::Value::Null),
        "blocks": resolved.block_ids,
        "blockSetHash": resolved.block_set_hash,
        "sources": resolved.sources.iter().map(|s| s.canonical_json()).collect::<Vec<_>>(),
        "coverage": serde_json::to_value(coverage).unwrap_or(serde_json::Value::Null),
        "tokenEstimate": token_estimate,
    });
    sha256_str(&canonical.to_string())
}

// —— 快照保护器(《M07》§8;与 evidence 属主校验器同一注册模式)——

/// M10 等模块注册"快照仍被引用(产物)"裁决;输入候选快照 ID,返回受保护子集。
pub type SnapshotProtectFn = fn(&Connection, &[String]) -> Result<Vec<String>, AppError>;

#[derive(Default, Clone)]
pub struct SnapshotProtectors {
    checks: Vec<(&'static str, SnapshotProtectFn)>,
}

impl SnapshotProtectors {
    pub fn register(&mut self, module: &'static str, check: SnapshotProtectFn) {
        self.checks.push((module, check));
    }

    pub fn protected_ids(
        &self,
        conn: &Connection,
        candidates: &[String],
    ) -> Result<Vec<String>, AppError> {
        let mut out = Vec::new();
        for (_, check) in &self.checks {
            for id in check(conn, candidates)? {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        Ok(out)
    }
}

// —— 服务 ——

pub struct ContextService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    events: Arc<dyn EventSink>,
    parse: Arc<ParseIndexService>,
    protectors: RwLock<SnapshotProtectors>,
    token_budget: i64,
}

impl ContextService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        events: Arc<dyn EventSink>,
        parse: Arc<ParseIndexService>,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            events,
            parse,
            protectors: RwLock::new(SnapshotProtectors::default()),
            token_budget: DEFAULT_CONTEXT_TOKEN_BUDGET,
        }
    }

    /// 测试与 M0 实测调参用:覆盖默认 Token 预算。
    pub fn with_token_budget(mut self, budget: i64) -> Self {
        self.token_budget = budget;
        self
    }

    pub fn token_budget(&self) -> i64 {
        self.token_budget
    }

    /// 注册快照保护器(M13 组装时接线 M10 产物引用;测试注入替身)。
    pub fn register_snapshot_protector(&mut self, module: &'static str, check: SnapshotProtectFn) {
        self.protectors
            .write()
            .expect("context protector registry lock")
            .register(module, check);
    }

    fn protectors_snapshot(&self) -> SnapshotProtectors {
        self.protectors
            .read()
            .expect("context protector registry lock")
            .clone()
    }

    fn emit_conversation(
        &self,
        conversation_id: &str,
        message_id: Option<&str>,
        task_id: Option<&str>,
        action: &str,
        document_id: &str,
        version: i64,
    ) {
        // 事件只在 COMMIT 成功后由调用点触发;此处只封装信封。
        let mut payload = serde_json::json!({
            "conversationId": conversation_id,
            "action": action,
        });
        if let Some(mid) = message_id {
            payload["messageId"] = serde_json::Value::String(mid.to_string());
        }
        if let Some(tid) = task_id {
            payload["taskId"] = serde_json::Value::String(tid.to_string());
        }
        let event = DomainEvent::new(
            DomainEventType::ConversationChanged,
            conversation_id.to_string(),
            version,
            self.clock.now_ms(),
            payload,
        )
        .with_document(Some(document_id.to_string()), None);
        self.events.emit(event);
    }

    // —— 范围建议(《M07》§2)——

    /// `context_suggest`:按 明确选区 > 视口块位置 > 当前节 > 全文 的固定
    /// 顺序给出候选;低置信度要求确认,不发送。
    pub fn suggest(
        &self,
        document_id: &str,
        version_id: &str,
        raw_selection: Option<&RawSelectionInput>,
        viewport_block_id: Option<&str>,
    ) -> Result<ContextSuggestion, AppError> {
        self.suggest_with_page(
            document_id,
            version_id,
            raw_selection,
            viewport_block_id,
            None,
        )
    }

    /// 带阅读器页码的范围建议:当前页 ±N 优先于“视口块”,但仍让明确选区优先。
    pub fn suggest_with_page(
        &self,
        document_id: &str,
        version_id: &str,
        raw_selection: Option<&RawSelectionInput>,
        viewport_block_id: Option<&str>,
        page: Option<PageViewport>,
    ) -> Result<ContextSuggestion, AppError> {
        let did = document_id.to_string();
        let vid = version_id.to_string();
        let raw = raw_selection.cloned();
        let vbid = viewport_block_id.map(|s| s.to_string());
        let out = self.db.read(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            validate_version_tx(conn, &did, &vid)?;
            let mut candidates: Vec<ScopeSuggestion> = Vec::new();
            let mut rank = 1i64;
            let mut had_signal = false;

            if let Some(sel) = &raw {
                if sel.document_id != did || sel.document_version_id != vid {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "selection_version_mismatch".into()));
                }
                had_signal = true;
                candidates.push(ScopeSuggestion {
                    rank,
                    scope: ScopeDraft::Selection {
                        raw_selection: Box::new(sel.clone()),
                    },
                    confidence: SuggestionConfidence::High,
                    requires_confirmation: false,
                    basis: "explicit_selection".to_string(),
                });
                rank += 1;
            }

            // 当前页 ±N(PDF):默认提问范围;明确选区仍然优先。
            if let Some(page) = page {
                if page.page_number >= 1 {
                    let nearby = page.nearby_pages.max(0);
                    let start = (page.page_number - nearby).max(1);
                    let end = page
                        .page_count
                        .map(|count| (page.page_number + nearby).min(count.max(1)))
                        .unwrap_or(page.page_number + nearby);
                    if end >= start {
                        had_signal = true;
                        candidates.push(ScopeSuggestion {
                            rank,
                            scope: ScopeDraft::Pages {
                                start_page: start,
                                end_page: end,
                            },
                            confidence: SuggestionConfidence::High,
                            requires_confirmation: false,
                            basis: "current_page".to_string(),
                        });
                        rank += 1;
                    }
                }
            }

            // 视口中心块(只读):解析块 → 位置 + 章节;按需块(无解析结构)
            // 低置信度,建议"当前视口段落后请确认"。
            let mut viewport = None;
            if let Some(bid) = &vbid {
                if let Ok(Some(block)) = load_block_tx(conn, bid) {
                    if block.document_version_id == vid {
                        viewport = Some(block);
                    }
                }
            }
            if let Some(block) = viewport {
                had_signal = true;
                let locator: SourceLocator =
                    serde_json::from_str(&block.locator_json).unwrap_or(SourceLocator::Composite {
                        parts: vec![],
                        locator_version: 1,
                    });
                let on_demand = block.origin == "on_demand";
                candidates.push(ScopeSuggestion {
                    rank,
                    scope: ScopeDraft::CurrentPosition {
                        block_id: block.id.clone(),
                        locator,
                    },
                    confidence: if on_demand {
                        SuggestionConfidence::Low
                    } else {
                        SuggestionConfidence::Medium
                    },
                    requires_confirmation: on_demand,
                    basis: "viewport_block".to_string(),
                });
                rank += 1;
                let heading: Vec<String> =
                    serde_json::from_str(&block.heading_path_json).unwrap_or_default();
                if !heading.is_empty() {
                    candidates.push(ScopeSuggestion {
                        rank,
                        scope: ScopeDraft::Section {
                            heading_path: heading,
                            block_ids: None,
                        },
                        confidence: SuggestionConfidence::Medium,
                        requires_confirmation: false,
                        basis: "viewport_section".to_string(),
                    });
                    rank += 1;
                }
            }

            candidates.push(ScopeSuggestion {
                rank,
                scope: ScopeDraft::FullDocument { confirmed: false },
                confidence: SuggestionConfidence::Low,
                requires_confirmation: true,
                basis: if had_signal {
                    "fallback_full".to_string()
                } else {
                    "no_signal".to_string()
                },
            });
            Ok(ContextSuggestion {
                document_id: did,
                version_id: vid,
                candidates,
            })
        })?;
        Ok(out)
    }

    // —— 预览(《M07》§3)——

    /// `context_preview`:名称、块数量、字符/Token 估算、来源清单、覆盖与
    /// 能力。预览不冻结、不创建块、不伪报检索候选;预算超限给出拆分提示。
    pub fn preview(
        &self,
        document_id: &str,
        scope: &ScopeDraft,
    ) -> Result<ContextPreview, AppError> {
        let did = document_id.to_string();
        let scope2 = scope.clone();
        let budget = self.token_budget;
        let out = self.db.read(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            let version_id = current_version_id_tx(conn, &did)?;
            if let ScopeDraft::Selection { raw_selection } = &scope2 {
                if raw_selection.document_id != did
                    || raw_selection.document_version_id != version_id
                {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "selection_version_mismatch".into()));
                }
            }
            validate_version_tx(conn, &did, &version_id)?;
            let input = ScopeInput {
                document_id: &did,
                version_id: &version_id,
                scope: &scope2,
                strict: false,
                idempotency_key: "",
                ensured: Vec::new(),
            };
            let (resolved, texts) = resolve_scope_tx(conn, &input)?;

            let (block_count, full_chars, full_tokens) = match &scope2 {
                ScopeDraft::FullDocument { .. } => {
                    let gen_id = resolved.parse_generation_id.clone().unwrap_or_default();
                    let (n, chars): (i64, i64) = conn
                        .query_row(
                            "SELECT COUNT(*), IFNULL(SUM(LENGTH(text)), 0) FROM content_block \
                             WHERE parse_generation_id=?1 AND origin IN ('parser','ocr')",
                            [&gen_id],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .map_err(|e| db_app_error(&e))?;
                    let tokens = if gen_id.is_empty() {
                        0
                    } else {
                        sum_generation_tokens_tx(conn, &gen_id)?
                    };
                    (n, chars, tokens)
                }
                // 选区预览不创建块:按草案块数计。
                ScopeDraft::Selection { raw_selection } => {
                    (raw_selection.blocks.len() as i64, 0, 0)
                }
                _ => (resolved.block_ids.len() as i64, 0, 0),
            };
            let mut char_count = full_chars;
            let mut token_estimate = full_tokens;
            for t in &texts {
                char_count += t.chars().count() as i64;
                token_estimate += estimate_tokens(t);
            }

            let mut source_previews = Vec::new();
            for (idx, s) in resolved.sources.iter().enumerate() {
                source_previews.push(ContextSourcePreview {
                    position: idx as i64 + 1,
                    source_type: s.source_type,
                    source_id: s.source_id.clone(),
                    source_role: s.source_role,
                    display_name: s.display_name.clone(),
                    char_count: s.text_snapshot.chars().count() as i64,
                    source_available: source_exists_tx(conn, &s.source_type, &s.source_id),
                });
            }

            let gen = current_generation_tx(conn, &version_id)?;
            let coverage = build_coverage_tx(conn, gen.as_ref(), &scope2, &resolved)?;
            let capability = if matches!(scope2, ScopeDraft::Selection { .. }) {
                "explicit_blocks".to_string()
            } else {
                capability_of_tx(conn, &scope2, &resolved)?
            };
            let mut warnings = Vec::new();
            if !coverage.scope_covered {
                warnings.push(format!(
                    "覆盖不足:{} 个区域未完成解析,不能视为已完整读取",
                    coverage.missing_regions.len()
                ));
                if matches!(scope2, ScopeDraft::FullDocument { .. }) {
                    warnings.push("部分解析不得显示“全文已读取”".to_string());
                }
            }
            if capability == "fts_only" {
                warnings.push("未配置嵌入模型,本次仅关键词检索(FTS-only)".to_string());
            }
            if capability == "unavailable" && !matches!(scope2, ScopeDraft::Selection { .. }) {
                warnings.push("当前版本无可用的全文检索能力".to_string());
            }
            let within_budget =
                matches!(scope2, ScopeDraft::FullDocument { .. }) || token_estimate <= budget;
            if !within_budget {
                warnings.push(format!(
                    "预估 {token_estimate} Token 超出预算 {budget};请缩小范围或改用分块任务"
                ));
            }
            Ok(ContextPreview {
                document_id: did.clone(),
                document_version_id: version_id,
                parse_generation_id: resolved.parse_generation_id,
                name: display_name_of(&scope2),
                block_count,
                char_count,
                token_estimate,
                token_budget: budget,
                within_budget,
                sources: source_previews,
                coverage,
                capability,
                warnings,
            })
        })?;
        Ok(out)
    }

    // —— 冻结(《M07》§4)——

    /// `context_freeze`:一个事务内校验版本、代次、块与覆盖,保存 Scope、
    /// Sources 与显式块集合,计算 canonicalHash;创建后禁止 UPDATE。
    /// 同 idempotencyKey 且内容一致幂等复用;同 key 不同范围视为修改
    /// 不可变快照而拒绝(复核在事务内,经 canonicalHash 比对)。
    pub fn freeze(
        &self,
        document_id: &str,
        scope: &ScopeDraft,
        idempotency_key: &str,
    ) -> Result<ContextSnapshotView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }

        let did = document_id.to_string();
        let version_for_ensure: String = self.db.read(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            current_version_id_tx(conn, &did)
        })?;

        // 选区按需块在事务外确保(M04 独立事务);冻结事务内只读复核。
        let mut ensured: Vec<(String, String)> = Vec::new();
        match scope {
            ScopeDraft::Selection { raw_selection } => {
                let raw = raw_selection.as_ref();
                if raw.document_id != document_id || raw.document_version_id != version_for_ensure {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "selection_version_mismatch".into()));
                }
                let mut ordered: Vec<&crate::evidence::models::RawSelectionBlockInput> =
                    raw.blocks.iter().collect();
                ordered.sort_by_key(|b| b.block_order);
                for b in &ordered {
                    let req = SelectionBlockRequest {
                        document_id: raw.document_id.clone(),
                        version_id: raw.document_version_id.clone(),
                        text: b.text.clone(),
                        locator: b.locator.clone(),
                        heading_path: heading_of(&b.locator),
                    };
                    let block = self.parse.ensure_selection_blocks(&req)?;
                    if block.text != b.text {
                        return Err(err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "selection_text_mismatch".into()));
                    }
                    ensured.push((format!("sel:{}", b.block_order), block.id));
                }
            }
            ScopeDraft::Sources { sources } => {
                for draft in sources {
                    if draft.source_type != ContextSourceType::Selection {
                        continue;
                    }
                    let text = draft
                        .text
                        .as_deref()
                        .filter(|t| !t.trim().is_empty())
                        .ok_or_else(|| {
                            err(CoreErrorCode::InvalidRequest)
                                .with_detail("reason", "selection_text_required".into())
                        })?;
                    let locator = draft.locator.clone().ok_or_else(|| {
                        err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "selection_locator_required".into())
                    })?;
                    let req = SelectionBlockRequest {
                        document_id: document_id.to_string(),
                        version_id: version_for_ensure.clone(),
                        text: text.to_string(),
                        heading_path: heading_of(&locator),
                        locator,
                    };
                    let block = self.parse.ensure_selection_blocks(&req)?;
                    ensured.push((format!("draft:{}", draft.source_id), block.id));
                }
            }
            _ => {}
        }

        let now = self.clock.now_ms();
        let snapshot_id = self.ids.new_entity_id();
        let source_ids: Vec<String> = (0..MAX_SOURCES_PER_SNAPSHOT)
            .map(|_| self.ids.new_entity_id())
            .collect();
        let did = document_id.to_string();
        let scope2 = scope.clone();
        let key = idempotency_key.to_string();
        let budget = self.token_budget;
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let out = freeze_tx(
                &tx,
                &ScopeInput {
                    document_id: &did,
                    version_id: &version_for_ensure,
                    scope: &scope2,
                    strict: true,
                    idempotency_key: &key,
                    ensured,
                },
                snapshot_id,
                &source_ids,
                now,
                budget,
            )?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(out)
        })?;
        Ok(view)
    }

    /// `context_get`:读取已冻结范围与来源(来源可用性读取时派生)。
    pub fn get_snapshot(&self, snapshot_id: &str) -> Result<Option<ContextSnapshotView>, AppError> {
        let sid = snapshot_id.to_string();
        self.db.read(move |conn| {
            Ok(load_snapshot_tx(conn, &sid)?
                .map(|row| snapshot_view_tx(conn, &row))
                .transpose())
        })?
    }

    /// 5 秒速答卡:从快照范围原文抽取句子(带页码),不调用模型。
    /// 明确标注为原文抽取;范围无文本时返回空列表,由 UI 显示真实阶段。
    pub fn quick_answer(
        &self,
        snapshot_id: &str,
        limit: usize,
    ) -> Result<QuickAnswerView, AppError> {
        let view = self
            .get_snapshot(snapshot_id)?
            .ok_or_else(|| err(CoreErrorCode::NotFound))?;
        let limit = limit.clamp(1, 5);
        let scope_label = scope_label_of(&view.scope);
        let block_ids = view.block_ids.clone();
        let generation_id = view.parse_generation_id.clone();
        let sentences = self.db.read(move |conn| {
            let mut candidates: Vec<(String, String, Option<i64>)> = Vec::new();
            if !block_ids.is_empty() {
                for block_id in block_ids.iter().take(40) {
                    if let Some(block) = load_block_tx(conn, block_id)? {
                        let page = serde_json::from_str::<SourceLocator>(&block.locator_json)
                            .ok()
                            .and_then(|locator| locator.pdf_page());
                        candidates.push((block.id, block.text, page));
                    }
                    if candidates.len() >= limit {
                        break;
                    }
                }
            } else if let Some(generation_id) = generation_id {
                for block in load_generation_blocks_tx(conn, &generation_id)?
                    .into_iter()
                    .take(40)
                {
                    candidates.push((block.id, block.text, block.page));
                    if candidates.len() >= limit {
                        break;
                    }
                }
            }
            let mut out: Vec<QuickAnswerSentence> = Vec::new();
            for (block_id, text, page) in candidates {
                for sentence in extract_sentences(&text, 3) {
                    if out.iter().any(|item| item.text == sentence) {
                        continue;
                    }
                    out.push(QuickAnswerSentence {
                        text: sentence,
                        page,
                        block_id: block_id.clone(),
                    });
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
            Ok(out)
        })?;
        Ok(QuickAnswerView {
            schema_version: "quick_answer.v1".into(),
            scope_label,
            note: if sentences.is_empty() {
                Some("当前范围内没有可用原文;仍在等待模型回答。".into())
            } else {
                Some("以下为原文抽取,不是模型结论;详细回答仍在生成中。".into())
            },
            sentences,
        })
    }

    // —— 会话(《M07》§6)——

    /// `conversation_create`:会话始终属原文档;可选初始活动快照。
    pub fn conversation_create(
        &self,
        document_id: &str,
        snapshot_id: Option<&str>,
        title: &str,
        idempotency_key: &str,
    ) -> Result<ConversationView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let did = document_id.to_string();
        let snap = snapshot_id.map(|s| s.to_string());
        let t = truncate_chars(title, 120);
        let key = idempotency_key.to_string();
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            if let Some(row) = load_conversation_by_key_tx(&tx, &key)? {
                return conversation_view_tx(&tx, &row);
            }
            if !document_exists_tx(&tx, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            if let Some(sid) = &snap {
                let s = load_snapshot_tx(&tx, sid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
                if s.document_id != did {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "snapshot_document_mismatch".into()));
                }
            }
            tx.execute(
                "INSERT INTO conversation(id, document_id, title, active_snapshot_id, \
                 idempotency_key, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?6)",
                params![id, did, t, snap, key, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let row =
                load_conversation_tx(&tx, &id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            let view = conversation_view_tx(&tx, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        self.emit_conversation(
            &view.id,
            None,
            None,
            "created",
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    /// `conversation_list`:按文档返回,最近更新在前。
    pub fn conversation_list(
        &self,
        document_id: &str,
        page: PageRequest,
    ) -> Result<ConversationPage, AppError> {
        let page = page.normalized();
        let did = document_id.to_string();
        let limit = page.limit;
        let cursor: Option<(i64, String)> = page.cursor.as_deref().and_then(decode_cursor);
        let out = self.db.read(move |conn| {
            let (mut sql, mut binds) = if did.trim().is_empty() {
                (
                    String::from(
                        "SELECT id, document_id, title, active_snapshot_id, pinned, created_at, updated_at \
                         FROM conversation WHERE 1=1",
                    ),
                    vec![],
                )
            } else {
                if !document_exists_tx(conn, &did)? {
                    return Err(err(CoreErrorCode::DocNotFound));
                }
                (
                    String::from(
                        "SELECT id, document_id, title, active_snapshot_id, pinned, created_at, updated_at \
                         FROM conversation WHERE document_id=?1",
                    ),
                    vec![SqlValue::Text(did.clone())],
                )
            };
            if let Some((ts, id)) = &cursor {
                binds.push(SqlValue::Integer(*ts));
                binds.push(SqlValue::Text(id.clone()));
                sql.push_str(&format!(
                    " AND (updated_at < ?{} OR (updated_at = ?{} AND id < ?{}))",
                    binds.len() - 1,
                    binds.len() - 1,
                    binds.len()
                ));
            }
            sql.push_str(&format!(
                " ORDER BY updated_at DESC, id DESC LIMIT ?{}",
                binds.len() + 1
            ));
            binds.push(SqlValue::Integer(limit + 1));
            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                    ConversationRow::try_from(r)
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            let items: Vec<ConversationView> = rows
                .iter()
                .take(limit as usize)
                .map(|row| conversation_view_tx(conn, row))
                .collect::<Result<_, _>>()?;
            Ok(Page::from_probe(items, limit, |c| {
                encode_cursor(&serde_json::json!({"ts": c.updated_at, "id": c.id}))
            }))
        })?;
        Ok(out)
    }

    /// `conversation_pin`:置顶/取消置顶(只改排序,不改消息)。
    pub fn set_conversation_pinned(
        &self,
        conversation_id: &str,
        pinned: bool,
    ) -> Result<ConversationView, AppError> {
        let cid = conversation_id.to_string();
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let _ = load_conversation_tx(&tx, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            tx.execute(
                "UPDATE conversation SET pinned=?2 WHERE id=?1",
                params![cid, if pinned { 1 } else { 0 }],
            )
            .map_err(|e| db_app_error(&e))?;
            let row =
                load_conversation_tx(&tx, &cid)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            let view = conversation_view_tx(&tx, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        self.emit_conversation(
            &view.id,
            None,
            None,
            "pinned",
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    /// `conversation_branch`:复制到某条消息(含)为止的真实消息到新会话。
    /// 新消息的 task_id 置空(旧 Task 不外借),内容/快照/状态原样保留;
    /// 原会话不被修改。
    pub fn branch_conversation(
        &self,
        source_conversation_id: &str,
        up_to_message_id: Option<&str>,
        idempotency_key: &str,
    ) -> Result<ConversationView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let source = source_conversation_id.to_string();
        let up_to = up_to_message_id.map(|value| value.to_string());
        let key = idempotency_key.to_string();
        let new_id = self.ids.new_entity_id();
        let now = self.clock.now_ms();
        let ids = std::sync::Arc::clone(&self.ids);
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            if let Some(row) = load_conversation_by_key_tx(&tx, &key)? {
                return conversation_view_tx(&tx, &row);
            }
            let source_row =
                load_conversation_tx(&tx, &source)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let title = format!(
                "{} · 分支",
                if source_row.title.trim().is_empty() {
                    "未命名会话"
                } else {
                    source_row.title.trim()
                }
            );
            let title = truncate_chars(&title, 120);
            tx.execute(
                "INSERT INTO conversation(id, document_id, title, active_snapshot_id, pinned, \
                 idempotency_key, created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,0,?5,?6,?6)",
                params![
                    new_id,
                    source_row.document_id,
                    title,
                    source_row.active_snapshot_id,
                    key,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            type MessageRow = (
                String,
                String,
                String,
                Option<String>,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                i64,
            );
            let rows: Vec<MessageRow> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id, role, content_json, snapshot_id, answer_status, \
                         error_code, model_provider_id, model_id, created_at \
                         FROM message WHERE conversation_id=?1 \
                         ORDER BY created_at ASC, id ASC",
                    )
                    .map_err(|e| db_app_error(&e))?;
                let rows = stmt
                    .query_map([&source], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                        ))
                    })
                    .map_err(|e| db_app_error(&e))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| db_app_error(&e))?;
                rows
            };
            let mut copied = 0;
            for (
                original_id,
                role,
                content_json,
                snapshot_id,
                answer_status,
                error_code,
                provider,
                model,
                _created,
            ) in rows
            {
                let message_id = ids.new_entity_id();
                tx.execute(
                    "INSERT INTO message(id, conversation_id, role, snapshot_id, task_id, \
                     content_json, answer_status, model_provider_id, model_id, error_code, \
                     idempotency_key, created_at, updated_at) \
                     VALUES (?1,?2,?3,?4,NULL,?5,?6,?7,?8,?9,?10,?11,?11)",
                    params![
                        message_id,
                        new_id,
                        role,
                        snapshot_id,
                        content_json,
                        answer_status,
                        provider,
                        model,
                        error_code,
                        format!("branch:{new_id}:{original_id}"),
                        now
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
                copied += 1;
                if up_to.as_deref() == Some(original_id.as_str()) {
                    break;
                }
            }
            tx.execute(
                "UPDATE conversation SET updated_at=?2 WHERE id=?1",
                params![new_id, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let row =
                load_conversation_tx(&tx, &new_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            let view = conversation_view_tx(&tx, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            if copied == 0 {
                tracing::info!(conversation_id = %new_id, "branch conversation has no messages");
            }
            Ok(view)
        })?;
        self.emit_conversation(
            &view.id,
            None,
            None,
            "branched",
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    /// `conversation_get_messages`:按时间升序;含任务状态与快照摘要。
    pub fn conversation_messages(
        &self,
        conversation_id: &str,
        page: PageRequest,
    ) -> Result<MessagePage, AppError> {
        let page = page.normalized();
        let cid = conversation_id.to_string();
        let limit = page.limit;
        let cursor: Option<(i64, String)> = page.cursor.as_deref().and_then(decode_cursor);
        let out = self.db.read(move |conn| {
            let conv =
                load_conversation_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let mut sql = String::from(&format!(
                "SELECT {MESSAGE_COLS} FROM message WHERE conversation_id=?1"
            ));
            let mut binds: Vec<SqlValue> = vec![SqlValue::Text(cid.clone())];
            if let Some((ts, id)) = &cursor {
                binds.push(SqlValue::Integer(*ts));
                binds.push(SqlValue::Text(id.clone()));
                sql.push_str(&format!(
                    " AND (created_at > ?{} OR (created_at = ?{} AND id > ?{}))",
                    binds.len() - 1,
                    binds.len() - 1,
                    binds.len()
                ));
            }
            sql.push_str(&format!(
                " ORDER BY created_at, id LIMIT ?{}",
                binds.len() + 1
            ));
            binds.push(SqlValue::Integer(limit + 1));
            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                    MessageRow::try_from(r)
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            let mut items = Vec::new();
            for row in rows.into_iter().take(limit as usize) {
                items.push(message_view_tx(conn, &conv.document_id, &row)?);
            }
            Ok(Page::from_probe(items, limit, |m| {
                encode_cursor(&serde_json::json!({"ts": m.created_at, "id": m.id}))
            }))
        })?;
        Ok(out)
    }

    /// 设置活动快照:只影响下一次追问建议;旧消息引用不变。
    /// 快照必须与会话同文档(跨文档拒绝)。
    pub fn set_active_snapshot(
        &self,
        conversation_id: &str,
        snapshot_id: &str,
    ) -> Result<ConversationView, AppError> {
        let cid = conversation_id.to_string();
        let sid = snapshot_id.to_string();
        let now = self.clock.now_ms();
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let conv =
                load_conversation_tx(&tx, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let snap = load_snapshot_tx(&tx, &sid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if snap.document_id != conv.document_id {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "snapshot_document_mismatch".into()));
            }
            tx.execute(
                "UPDATE conversation SET active_snapshot_id=?2, updated_at=?3 WHERE id=?1",
                params![cid, sid, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let row =
                load_conversation_tx(&tx, &cid)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            let view = conversation_view_tx(&tx, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        self.emit_conversation(
            &view.id,
            None,
            None,
            "active_snapshot_changed",
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    /// 会话标题(首问生成或用户编辑;M09/M13 接线)。
    pub fn update_conversation_title(
        &self,
        conversation_id: &str,
        title: &str,
    ) -> Result<ConversationView, AppError> {
        let cid = conversation_id.to_string();
        let t = truncate_chars(title, 120);
        let now = self.clock.now_ms();
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let _ = load_conversation_tx(&tx, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            tx.execute(
                "UPDATE conversation SET title=?2, updated_at=?3 WHERE id=?1",
                params![cid, t, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let row =
                load_conversation_tx(&tx, &cid)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            let view = conversation_view_tx(&tx, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        self.emit_conversation(
            &view.id,
            None,
            None,
            "renamed",
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    // —— 消息(《M07》§6;tx 版供 M09 任务事务复用)——

    /// 用户消息独立入口;M09 在 Task 创建事务内调用 create_user_message_tx。
    pub fn append_user_message(&self, input: NewUserMessage<'_>) -> Result<MessageView, AppError> {
        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let (cid, sid, tid) = (
            input.conversation_id.to_string(),
            input.snapshot_id.map(|s| s.to_string()),
            input.task_id.map(|s| s.to_string()),
        );
        let text = input.text.to_string();
        let key = input.idempotency_key.to_string();
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let (row, doc) = create_user_message_tx(
                &tx,
                &NewUserMessage {
                    conversation_id: &cid,
                    snapshot_id: sid.as_deref(),
                    task_id: tid.as_deref(),
                    text: &text,
                    idempotency_key: &key,
                },
                &id,
                now,
            )?;
            let view = message_view_tx(&tx, &doc, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        self.emit_conversation(
            &view.conversation_id,
            Some(&view.id),
            view.task_id.as_deref(),
            "message_appended",
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    /// 助手占位消息独立入口(模型调用前);M09 在任务事务内调用
    /// create_assistant_placeholder_tx。
    pub fn append_assistant_placeholder(
        &self,
        input: NewAssistantMessage<'_>,
    ) -> Result<MessageView, AppError> {
        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let (cid, sid, tid) = (
            input.conversation_id.to_string(),
            input.snapshot_id.map(|s| s.to_string()),
            input.task_id.map(|s| s.to_string()),
        );
        let key = input.idempotency_key.to_string();
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let (row, doc) = create_assistant_placeholder_tx(
                &tx,
                &NewAssistantMessage {
                    conversation_id: &cid,
                    snapshot_id: sid.as_deref(),
                    task_id: tid.as_deref(),
                    idempotency_key: &key,
                },
                &id,
                now,
            )?;
            let view = message_view_tx(&tx, &doc, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        Ok(view)
    }

    /// 幂等完成助手消息(独立入口;M09 在 commit_result 事务内调用
    /// complete_assistant_message_tx)。只保存通过校验的 AnswerContent。
    pub fn complete_assistant_message(
        &self,
        message_id: &str,
        answer: &AnswerContent,
        model_provider_id: Option<&str>,
        model_id: Option<&str>,
    ) -> Result<MessageView, AppError> {
        let now = self.clock.now_ms();
        let mid = message_id.to_string();
        let ans = answer.clone();
        let provider = model_provider_id.map(|s| s.to_string());
        let model = model_id.map(|s| s.to_string());
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let (row, doc) = complete_assistant_message_tx(
                &tx,
                &mid,
                &ans,
                provider.as_deref(),
                model.as_deref(),
                now,
            )?;
            let view = message_view_tx(&tx, &doc, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        self.emit_conversation(
            &view.conversation_id,
            Some(&view.id),
            view.task_id.as_deref(),
            "message_completed",
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    /// 失败/取消:保留问题、快照与错误原因,不保存伪答案。
    pub fn fail_assistant_message(
        &self,
        message_id: &str,
        error_code: &str,
        canceled: bool,
    ) -> Result<MessageView, AppError> {
        let now = self.clock.now_ms();
        let mid = message_id.to_string();
        let code = error_code.to_string();
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let (row, doc) = fail_assistant_message_tx(&tx, &mid, &code, canceled, now)?;
            let view = message_view_tx(&tx, &doc, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        self.emit_conversation(
            &view.conversation_id,
            Some(&view.id),
            view.task_id.as_deref(),
            if canceled {
                "message_canceled"
            } else {
                "message_failed"
            },
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    /// 失败/取消且必须可见:把占位内容替换为 `assistant_notice.v1`。
    /// 生命周期仍为 failed/canceled,不冒充已验证回答。
    pub fn fail_assistant_message_with_notice(
        &self,
        message_id: &str,
        error_code: &str,
        canceled: bool,
        notice: &crate::context::models::AssistantNoticeV1,
    ) -> Result<MessageView, AppError> {
        let now = self.clock.now_ms();
        let mid = message_id.to_string();
        let code = error_code.to_string();
        let notice = notice.clone();
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let (row, doc) =
                fail_assistant_message_with_notice_tx(&tx, &mid, &code, canceled, &notice, now)?;
            let view = message_view_tx(&tx, &doc, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        self.emit_conversation(
            &view.conversation_id,
            Some(&view.id),
            view.task_id.as_deref(),
            if canceled {
                "message_canceled"
            } else {
                "message_failed"
            },
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    /// 回填助手消息的 task_id(占位消息先于任务创建时使用)。
    /// 同值重放幂等;改绑到另一个任务属于契约错误(不静默改写历史)。
    pub fn bind_message_task(
        &self,
        message_id: &str,
        task_id: &str,
    ) -> Result<MessageView, AppError> {
        let now = self.clock.now_ms();
        let mid = message_id.to_string();
        let tid = task_id.to_string();
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let row = load_message_tx(&tx, &mid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let conv = load_conversation_tx(&tx, &row.conversation_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            match row.task_id.as_deref() {
                Some(existing) if existing != tid => {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "message_task_conflict".into()));
                }
                Some(_) => {}
                None => {
                    tx.execute(
                        "UPDATE message SET task_id=?2, updated_at=?3 WHERE id=?1",
                        params![mid, tid, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                }
            }
            let updated =
                load_message_tx(&tx, &mid)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            let view = message_view_tx(&tx, &conv.document_id, &updated)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        Ok(view)
    }

    /// 完成 Agent 助手消息:写入 agent.v1(最终答复 + 步骤快照)。
    /// 未通过 answer.v1 校验的答复(verified=false)按自由文本保存,
    /// 由 UI 明确标注“未校验”,不冒充已验证内容。
    pub fn complete_agent_message(
        &self,
        message_id: &str,
        answer: &AgentAnswerV1,
        model_provider_id: Option<&str>,
        model_id: Option<&str>,
    ) -> Result<MessageView, AppError> {
        let now = self.clock.now_ms();
        let mid = message_id.to_string();
        let ans = answer.clone();
        let provider = model_provider_id.map(str::to_string);
        let model = model_id.map(str::to_string);
        let view = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let (row, doc) = complete_agent_message_tx(
                &tx,
                &mid,
                &ans,
                provider.as_deref(),
                model.as_deref(),
                now,
            )?;
            let view = message_view_tx(&tx, &doc, &row)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(view)
        })?;
        self.emit_conversation(
            &view.conversation_id,
            Some(&view.id),
            view.task_id.as_deref(),
            "message_completed",
            &view.document_id,
            view.updated_at,
        );
        Ok(view)
    }

    /// 最近 N 条消息(时间升序返回):供多轮历史装填与摘要窗口使用。
    pub fn conversation_recent_messages(
        &self,
        conversation_id: &str,
        limit: i64,
    ) -> Result<Vec<MessageView>, AppError> {
        let cid = conversation_id.to_string();
        let limit = limit.clamp(1, 200);
        self.db.read(move |conn| {
            let conv =
                load_conversation_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {MESSAGE_COLS} FROM message WHERE conversation_id=?1 \
                     ORDER BY created_at DESC, id DESC LIMIT ?2"
                ))
                .map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map(params![cid, limit], |r| MessageRow::try_from(r))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            let mut items = Vec::with_capacity(rows.len());
            for row in rows.into_iter().rev() {
                items.push(message_view_tx(conn, &conv.document_id, &row)?);
            }
            Ok(items)
        })
    }

    /// 单条消息(任务中心「查看结果」定位用):含会话、文档与快照摘要。
    pub fn message_view(&self, message_id: &str) -> Result<Option<MessageView>, AppError> {
        let mid = message_id.to_string();
        self.db.read(move |conn| {
            let Some(row) = load_message_tx(conn, &mid)? else {
                return Ok(None);
            };
            let conv = load_conversation_tx(conn, &row.conversation_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            Ok(Some(message_view_tx(conn, &conv.document_id, &row)?))
        })
    }

    /// 任务重试来源:该任务的助手消息 + 之前最近的用户消息。
    /// 找不到消息/快照或用户消息时返回 None(不猜测内容)。
    pub fn task_retry_source(&self, task_id: &str) -> Result<Option<TaskRetrySource>, AppError> {
        let tid = task_id.to_string();
        self.db.read(move |conn| {
            let assistant: Option<(String, String, Option<String>, i64)> = conn
                .query_row(
                    "SELECT id, conversation_id, snapshot_id, created_at FROM message \
                     WHERE task_id=?1 AND role='assistant' \
                     ORDER BY created_at DESC, id DESC LIMIT 1",
                    [&tid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let Some((assistant_id, conversation_id, assistant_snapshot, created_at)) = assistant
            else {
                return Ok(None);
            };
            let conv = load_conversation_tx(conn, &conversation_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let user: Option<MessageRow> = conn
                .query_row(
                    &format!(
                        "SELECT {MESSAGE_COLS} FROM message \
                         WHERE conversation_id=?1 AND role='user' \
                         AND (created_at < ?2 OR (created_at = ?2 AND id < ?3)) \
                         ORDER BY created_at DESC, id DESC LIMIT 1"
                    ),
                    params![&conversation_id, created_at, &assistant_id],
                    |r| MessageRow::try_from(r),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let Some(user) = user else {
                return Ok(None);
            };
            let question = match serde_json::from_str::<MessageContent>(&user.content_json) {
                Ok(MessageContent::UserMessage { text }) => text,
                _ => return Ok(None),
            };
            let snapshot_id = assistant_snapshot.or(user.snapshot_id.clone());
            let Some(snapshot_id) = snapshot_id else {
                return Ok(None);
            };
            Ok(Some(TaskRetrySource {
                document_id: conv.document_id,
                conversation_id: user.conversation_id,
                snapshot_id,
                question,
            }))
        })
    }

    /// 已保存的会话摘要(不存在返回 None)。
    pub fn conversation_summary(
        &self,
        conversation_id: &str,
    ) -> Result<Option<ConversationSummaryRow>, AppError> {
        let cid = conversation_id.to_string();
        self.db.read(move |conn| load_summary_tx(conn, &cid))
    }

    /// 摘要计划:已有摘要 + 下一批待压缩消息(窗口决策归 M07)。
    pub fn conversation_summary_plan(
        &self,
        conversation_id: &str,
    ) -> Result<SummaryPlanRow, AppError> {
        let cid = conversation_id.to_string();
        self.db.read(move |conn| build_plan_tx(conn, &cid))
    }

    /// 保存成功摘要并推进覆盖边界(失败不覆盖旧文本)。
    pub fn save_conversation_summary(
        &self,
        conversation_id: &str,
        up_to_message_id: &str,
        summary_text: &str,
        token_count: i64,
    ) -> Result<(), AppError> {
        let now = self.clock.now_ms();
        let cid = conversation_id.to_string();
        let up_to = up_to_message_id.to_string();
        let text = summary_text.to_string();
        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            save_summary_tx(&tx, &cid, &up_to, &text, token_count, now)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }

    /// 记录摘要失败:保留上次成功摘要,只更新状态与错误码。
    pub fn record_conversation_summary_failure(
        &self,
        conversation_id: &str,
        error_code: &str,
    ) -> Result<(), AppError> {
        let now = self.clock.now_ms();
        let cid = conversation_id.to_string();
        let code = error_code.to_string();
        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            record_failure_tx(&tx, &cid, &code, now)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }

    /// 删除会话及其消息、附件行、引用与 Agent 运行记录(单事务)。
    /// 快照不随会话删除:快照可能仍被产物引用,清理由 M08 清除流程裁决。
    pub fn delete_conversation(&self, conversation_id: &str) -> Result<DeleteResult, AppError> {
        let cid = conversation_id.to_string();
        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let exists: Option<i64> = tx
                .query_row("SELECT 1 FROM conversation WHERE id=?1", [&cid], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if exists.is_none() {
                return Err(err(CoreErrorCode::NotFound));
            }
            // 引用与反馈(属主 ID 为 message:{id})。
            tx.execute(
                "DELETE FROM evidence_feedback WHERE owner_type='message' AND owner_id IN \
                 (SELECT 'message:' || id FROM message WHERE conversation_id=?1)",
                [&cid],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "DELETE FROM citation WHERE owner_type='message' AND owner_id IN \
                 (SELECT 'message:' || id FROM message WHERE conversation_id=?1)",
                [&cid],
            )
            .map_err(|e| db_app_error(&e))?;
            // 附件行(文件字节由命令层按 storage_key 清理)。
            tx.execute("DELETE FROM attachment WHERE conversation_id=?1", [&cid])
                .map_err(|e| db_app_error(&e))?;
            // Agent 运行记录(步骤随外键级联)。
            tx.execute("DELETE FROM agent_run WHERE conversation_id=?1", [&cid])
                .map_err(|e| db_app_error(&e))?;
            tx.execute("DELETE FROM message WHERE conversation_id=?1", [&cid])
                .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "DELETE FROM conversation_summary WHERE conversation_id=?1",
                [&cid],
            )
            .map_err(|e| db_app_error(&e))?;
            let removed = tx
                .execute("DELETE FROM conversation WHERE id=?1", [&cid])
                .map_err(|e| db_app_error(&e))?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(DeleteResult {
                deleted: removed > 0,
                id: cid,
            })
        })
    }

    /// 会话导出数据:会话 + 全部消息(升序);附件由命令层补齐。
    pub fn conversation_export_data(
        &self,
        conversation_id: &str,
    ) -> Result<ConversationExportData, AppError> {
        let cid = conversation_id.to_string();
        self.db.read(move |conn| {
            let row =
                load_conversation_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let conversation = conversation_view_tx(conn, &row)?;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {MESSAGE_COLS} FROM message WHERE conversation_id=?1 \
                     ORDER BY created_at, id"
                ))
                .map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map([&cid], |r| MessageRow::try_from(r))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            let mut messages = Vec::with_capacity(rows.len());
            for row in rows {
                messages.push(message_view_tx(conn, &conversation.document_id, &row)?);
            }
            Ok(ConversationExportData {
                conversation,
                messages,
                attachments: Vec::new(),
            })
        })
    }

    // —— 清除贡献(《M07》§8;只产出计划,不执行删除)——
    /// M08 清除流程调用:预览将删除的会话、消息、快照与仍受保护的快照。
    pub fn clear_contribution_preview(
        &self,
        document_id: &str,
    ) -> Result<ContextClearPreview, AppError> {
        let did = document_id.to_string();
        let protectors = self.protectors_snapshot();
        let out = self.db.read(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            let conversation_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM conversation WHERE document_id=?1",
                    [&did],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            let message_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM message m JOIN conversation c ON c.id=m.conversation_id \
                     WHERE c.document_id=?1",
                    [&did],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            let snapshot_total: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM context_snapshot WHERE document_id=?1",
                    [&did],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;

            let mut stmt = conn
                .prepare("SELECT id FROM context_snapshot WHERE document_id=?1")
                .map_err(|e| db_app_error(&e))?;
            let snapshots: Vec<String> = stmt
                .query_map([&did], |r| r.get(0))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);

            let mut protected = Vec::new();
            let mut candidates = Vec::new();
            for sid in &snapshots {
                let referenced: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM message WHERE snapshot_id=?1",
                        [sid],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let active: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM conversation WHERE active_snapshot_id=?1",
                        [sid],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                if referenced > 0 || active > 0 {
                    protected.push(ProtectedSnapshotView {
                        snapshot_id: sid.clone(),
                        reason: if referenced > 0 {
                            "message_reference".to_string()
                        } else {
                            "active_snapshot".to_string()
                        },
                    });
                } else {
                    candidates.push(sid.clone());
                }
            }
            // 注册保护器(产物引用等):候选仍被引用 ⇒ 保留,
            // 不让删除会话导致历史 Artifact 失去证据。
            let extra = protectors.protected_ids(conn, &candidates)?;
            let deletable: Vec<String> = candidates
                .into_iter()
                .filter(|sid| {
                    if extra.contains(sid) {
                        protected.push(ProtectedSnapshotView {
                            snapshot_id: sid.clone(),
                            reason: "artifact_reference".to_string(),
                        });
                        false
                    } else {
                        true
                    }
                })
                .collect();

            let conversations_clearing_active: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM conversation WHERE document_id=?1 \
                     AND active_snapshot_id IS NOT NULL",
                    [&did],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;

            // 同事务按序执行步骤(M01 消费;M07 不执行删除)。
            let plan = vec![
                ClearPlanStep {
                    step: 1,
                    action: "delete".to_string(),
                    table: "message".to_string(),
                    qualifier: "conversation_id IN (该文档会话集合)".to_string(),
                },
                ClearPlanStep {
                    step: 2,
                    action: "update_null".to_string(),
                    table: "conversation.active_snapshot_id".to_string(),
                    qualifier: "document_id = 目标文档".to_string(),
                },
                ClearPlanStep {
                    step: 3,
                    action: "delete".to_string(),
                    table: "conversation".to_string(),
                    qualifier: "document_id = 目标文档".to_string(),
                },
                ClearPlanStep {
                    step: 4,
                    action: "delete".to_string(),
                    table: "context_source".to_string(),
                    qualifier: "snapshot_id IN (可删除快照集合)".to_string(),
                },
                ClearPlanStep {
                    step: 5,
                    action: "delete".to_string(),
                    table: "snapshot_block".to_string(),
                    qualifier: "snapshot_id IN (可删除快照集合)".to_string(),
                },
                ClearPlanStep {
                    step: 6,
                    action: "delete".to_string(),
                    table: "context_snapshot".to_string(),
                    qualifier: "id IN (可删除快照集合)".to_string(),
                },
            ];

            Ok(ContextClearPreview {
                document_id: did,
                conversation_count,
                message_count,
                snapshot_total,
                deletable_snapshot_ids: deletable,
                protected_snapshots: protected,
                conversations_clearing_active,
                plan,
            })
        })?;
        Ok(out)
    }
}

// —— 能力映射(《01》§4.6 capabilityMode)——

fn capability_of_tx(
    conn: &Connection,
    scope: &ScopeDraft,
    resolved: &ResolvedScope,
) -> Result<String, AppError> {
    match scope {
        ScopeDraft::FullDocument { .. } => {
            let Some(gen) = &resolved.parse_generation_id else {
                return Ok("unavailable".to_string());
            };
            if resolved.block_set_hash.is_none() {
                return Ok("unavailable".to_string());
            }
            let fts: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM content_block_fts WHERE parse_generation_id=?1",
                    [gen],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            let vec: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM vector_embedding ve \
                     JOIN embedding_generation eg ON eg.id = ve.generation_id \
                     WHERE eg.parse_generation_id=?1",
                    [gen],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            if vec > 0 {
                Ok("hybrid".to_string())
            } else if fts > 0 {
                Ok("fts_only".to_string())
            } else {
                Ok("unavailable".to_string())
            }
        }
        _ => {
            if resolved.block_ids.is_empty() && resolved.sources.is_empty() {
                Ok("unavailable".to_string())
            } else {
                Ok("explicit_blocks".to_string())
            }
        }
    }
}

fn heading_of(locator: &SourceLocator) -> Vec<String> {
    match locator {
        SourceLocator::Markdown { heading_path, .. } => heading_path.clone(),
        SourceLocator::Docx { heading_path, .. } => heading_path.clone(),
        _ => Vec::new(),
    }
}

/// 全文 Token 估算:按块聚合。
fn sum_generation_tokens_tx(conn: &Connection, generation_id: &str) -> Result<i64, AppError> {
    let mut stmt = conn
        .prepare("SELECT text FROM content_block WHERE parse_generation_id=?1 AND origin IN ('parser','ocr')")
        .map_err(|e| db_app_error(&e))?;
    let tokens: i64 = stmt
        .query_map([generation_id], |r| r.get::<_, String>(0))
        .map_err(|e| db_app_error(&e))?
        .map(|t| estimate_tokens(&t.unwrap_or_default()))
        .sum();
    Ok(tokens)
}

// —— 冻结事务核心 ——

fn freeze_tx(
    conn: &Connection,
    input: &ScopeInput<'_>,
    snapshot_id: String,
    source_ids: &[String],
    now: i64,
    budget: i64,
) -> Result<ContextSnapshotView, AppError> {
    // 版本归属与指纹状态。
    validate_version_tx(conn, input.document_id, input.version_id)?;
    let (resolved, texts) = resolve_scope_tx(conn, input)?;

    // 全文前置条件:代次完整且 blockSetHash 已封存,否则覆盖不足
    // (部分解析不得冻结全文、不得自动全文回答)。
    if matches!(input.scope, ScopeDraft::FullDocument { .. })
        && resolved.full_document_ready != Some(true)
    {
        let gen = current_generation_tx(conn, input.version_id)?;
        return Err(cerr(ContextErrorCode::CoverageIncomplete)
            .with_detail("reason", "generation_not_sealed".into())
            .with_detail("parseState", parse_state_of(gen.as_ref()).into()));
    }

    // 显式块上限(防滥用;真实预算约束在下方)。
    if resolved.block_ids.len() > MAX_SNAPSHOT_BLOCKS {
        return Err(err(CoreErrorCode::InvalidRequest)
            .with_detail("reason", "snapshot_block_limit".into())
            .with_detail("max", (MAX_SNAPSHOT_BLOCKS as i64).into()));
    }

    // 预算:不静默截断,超限拒绝并要求缩小范围或分块。
    let token_estimate = if matches!(input.scope, ScopeDraft::FullDocument { .. }) {
        sum_generation_tokens_tx(
            conn,
            &resolved.parse_generation_id.clone().unwrap_or_default(),
        )?
    } else {
        texts.iter().map(|t| estimate_tokens(t)).sum()
    };
    // 显式选区/切片必须严格在预算内; 全文快照使用检索(FTS/Vector)按 Prompt 预算动态装填,不在此粗暴拦截
    if !matches!(input.scope, ScopeDraft::FullDocument { .. }) && token_estimate > budget {
        return Err(cerr(ContextErrorCode::BudgetExceeded)
            .with_detail("reason", "token_budget_exceeded".into())
            .with_detail("estimatedTokens", token_estimate.into())
            .with_detail("budget", budget.into()));
    }

    // 覆盖与 canonical hash。
    let gen = current_generation_tx(conn, input.version_id)?;
    let coverage = build_coverage_tx(conn, gen.as_ref(), input.scope, &resolved)?;
    let canonical_hash = canonical_hash_of(
        input.document_id,
        input.version_id,
        resolved.parse_generation_id.as_deref(),
        input.scope,
        &resolved,
        &coverage,
        token_estimate,
    );

    // 幂等复核(并发兜底):同 key 已存在 → 内容一致复用;
    // 不一致视为修改不可变快照(CONTEXT_SNAPSHOT_IMMUTABLE)。
    if let Some(existing) = load_snapshot_by_key_tx(conn, input.idempotency_key)? {
        if existing.canonical_hash == canonical_hash {
            return snapshot_view_tx(conn, &existing);
        }
        return Err(cerr(ContextErrorCode::SnapshotImmutable)
            .with_detail("reason", "canonical_hash_mismatch".into()));
    }

    let scope_json =
        serde_json::to_string(input.scope).map_err(|_| err(CoreErrorCode::Internal))?;
    let coverage_json =
        serde_json::to_string(&coverage).map_err(|_| err(CoreErrorCode::Internal))?;
    conn.execute(
        "INSERT INTO context_snapshot(id, document_id, document_version_id, \
         parse_generation_id, scope_kind, scope_json, coverage_json, block_set_hash, \
         canonical_hash, idempotency_key, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            snapshot_id,
            input.document_id,
            input.version_id,
            resolved.parse_generation_id,
            input.scope.kind().as_str(),
            scope_json,
            coverage_json,
            resolved.block_set_hash,
            canonical_hash,
            input.idempotency_key,
            now,
        ],
    )
    .map_err(|e| db_app_error(&e))?;

    for (position, src) in resolved.sources.iter().enumerate() {
        let sid = source_ids
            .get(position)
            .cloned()
            .unwrap_or_else(crate::shared::new_trace_id);
        conn.execute(
            "INSERT INTO context_source(id, snapshot_id, position, source_type, source_id, \
             source_version, source_role, display_name, text_snapshot, locator_snapshot_json, \
             content_hash, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                sid,
                snapshot_id,
                position as i64,
                src.source_type.as_str(),
                src.source_id,
                src.source_version,
                src.source_role.as_str(),
                src.display_name,
                src.text_snapshot,
                src.locator_snapshot_json,
                src.content_hash,
                now,
            ],
        )
        .map_err(|e| db_app_error(&e))?;
    }
    for (position, block_id) in resolved.block_ids.iter().enumerate() {
        conn.execute(
            "INSERT INTO snapshot_block(snapshot_id, block_id, position) VALUES (?1,?2,?3)",
            params![snapshot_id, block_id, position as i64],
        )
        .map_err(|e| db_app_error(&e))?;
    }

    let row = load_snapshot_tx(conn, &snapshot_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
    snapshot_view_tx(conn, &row)
}

// —— 视图组装 ——

fn load_snapshot_tx(conn: &Connection, id: &str) -> Result<Option<SnapshotRow>, AppError> {
    conn.query_row(
        &format!("SELECT {SNAPSHOT_COLS} FROM context_snapshot WHERE id=?1"),
        [id],
        |r| SnapshotRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_snapshot_by_key_tx(conn: &Connection, key: &str) -> Result<Option<SnapshotRow>, AppError> {
    conn.query_row(
        &format!("SELECT {SNAPSHOT_COLS} FROM context_snapshot WHERE idempotency_key=?1"),
        [key],
        |r| SnapshotRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn snapshot_brief_tx(
    conn: &Connection,
    snapshot_id: &str,
) -> Result<Option<SnapshotBrief>, AppError> {
    let Some(row) = load_snapshot_tx(conn, snapshot_id)? else {
        return Ok(None);
    };
    let scope: ScopeDraft =
        serde_json::from_str(&row.scope_json).unwrap_or(ScopeDraft::Sources { sources: vec![] });
    let coverage = serde_json::from_str::<CoverageSummary>(&row.coverage_json).ok();
    Ok(Some(SnapshotBrief {
        snapshot_id: row.id,
        scope_kind: row.scope_kind,
        name: display_name_of(&scope),
        coverage,
    }))
}

/// context_source 查询行(结构复杂,具名元组降低签名噪声)。
type SourceQueryRow = (
    String,         // source_type
    String,         // source_id
    Option<String>, // source_version
    String,         // source_role
    String,         // display_name
    String,         // text_snapshot
    String,         // content_hash
);

fn snapshot_view_tx(conn: &Connection, row: &SnapshotRow) -> Result<ContextSnapshotView, AppError> {
    let scope: ScopeDraft =
        serde_json::from_str(&row.scope_json).unwrap_or(ScopeDraft::Sources { sources: vec![] });
    let coverage: CoverageSummary = serde_json::from_str(&row.coverage_json)
        .unwrap_or_else(|_| CoverageSummary::empty("failed"));

    let mut stmt = conn
        .prepare(
            "SELECT source_type, source_id, source_version, source_role, display_name, \
             text_snapshot, content_hash FROM context_source WHERE snapshot_id=?1 \
             ORDER BY position",
        )
        .map_err(|e| db_app_error(&e))?;
    let source_rows: Vec<SourceQueryRow> = stmt
        .query_map([&row.id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_app_error(&e))?;
    drop(stmt);

    let mut block_stmt = conn
        .prepare("SELECT block_id FROM snapshot_block WHERE snapshot_id=?1 ORDER BY position")
        .map_err(|e| db_app_error(&e))?;
    let block_ids: Vec<String> = block_stmt
        .query_map([&row.id], |r| r.get(0))
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_app_error(&e))?;
    drop(block_stmt);

    let mut sources = Vec::new();
    let mut char_count = 0i64;
    let mut token_estimate = 0i64;
    for (position, (type_s, id_s, version_s, role_s, name_s, text_s, hash_s)) in
        source_rows.into_iter().enumerate()
    {
        let source_type = ContextSourceType::parse_db(&type_s).unwrap_or(ContextSourceType::Note);
        let role = SourceRole::parse_db(&role_s).unwrap_or(SourceRole::Original);
        char_count += text_s.chars().count() as i64;
        token_estimate += estimate_tokens(&text_s);
        sources.push(ContextSourceView {
            position: position as i64,
            source_type,
            source_id: id_s.clone(),
            source_role: role,
            source_version: version_s,
            display_name: name_s,
            text_snapshot: text_s.clone(),
            char_count: text_s.chars().count() as i64,
            content_hash: hash_s,
            source_available: source_exists_tx(conn, &source_type, &id_s),
        });
    }

    // 块文本计入摘要(全文按代次聚合)。
    if row.scope_kind == ScopeKind::FullDocument {
        if let Some(gen) = &row.parse_generation_id {
            let chars: i64 = conn
                .query_row(
                    "SELECT IFNULL(SUM(LENGTH(text)), 0) FROM content_block \
                     WHERE parse_generation_id=?1 AND origin IN ('parser','ocr')",
                    [gen],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            char_count += chars;
            token_estimate += sum_generation_tokens_tx(conn, gen)?;
        }
    } else {
        for bid in &block_ids {
            if let Some(b) = load_block_tx(conn, bid)? {
                char_count += b.text.chars().count() as i64;
                token_estimate += estimate_tokens(&b.text);
            }
        }
    }

    Ok(ContextSnapshotView {
        id: row.id.clone(),
        document_id: row.document_id.clone(),
        document_version_id: row.document_version_id.clone(),
        parse_generation_id: row.parse_generation_id.clone(),
        scope_kind: row.scope_kind,
        scope,
        summary: SnapshotSummary {
            name: display_name_of(
                &serde_json::from_str(&row.scope_json)
                    .unwrap_or(ScopeDraft::Sources { sources: vec![] }),
            ),
            block_count: block_ids.len() as i64,
            source_count: sources.len() as i64,
            char_count,
            token_estimate,
        },
        coverage,
        block_ids,
        block_set_hash: row.block_set_hash.clone(),
        canonical_hash: row.canonical_hash.clone(),
        sources,
        created_at: row.created_at,
    })
}

fn load_conversation_tx(conn: &Connection, id: &str) -> Result<Option<ConversationRow>, AppError> {
    conn.query_row(
        &format!("SELECT {CONVERSATION_COLS} FROM conversation WHERE id=?1"),
        [id],
        |r| ConversationRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_conversation_by_key_tx(
    conn: &Connection,
    key: &str,
) -> Result<Option<ConversationRow>, AppError> {
    conn.query_row(
        &format!("SELECT {CONVERSATION_COLS} FROM conversation WHERE idempotency_key=?1"),
        [key],
        |r| ConversationRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn conversation_view_tx(
    conn: &Connection,
    row: &ConversationRow,
) -> Result<ConversationView, AppError> {
    let message_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM message WHERE conversation_id=?1",
            [&row.id],
            |r| r.get(0),
        )
        .map_err(|e| db_app_error(&e))?;
    let active_snapshot = match &row.active_snapshot_id {
        Some(sid) => snapshot_brief_tx(conn, sid)?,
        None => None,
    };
    Ok(ConversationView {
        id: row.id.clone(),
        document_id: row.document_id.clone(),
        title: row.title.clone(),
        active_snapshot_id: row.active_snapshot_id.clone(),
        active_snapshot,
        message_count,
        pinned: row.pinned,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn message_view_tx(
    conn: &Connection,
    document_id: &str,
    row: &MessageRow,
) -> Result<MessageView, AppError> {
    let task_status: Option<String> = match &row.task_id {
        Some(tid) => conn
            .query_row("SELECT status FROM task WHERE id=?1", [tid], |r| r.get(0))
            .optional()
            .map_err(|e| db_app_error(&e))?,
        None => None,
    };
    let snapshot = match &row.snapshot_id {
        Some(sid) => snapshot_brief_tx(conn, sid)?,
        None => None,
    };
    let content: MessageContent =
        serde_json::from_str(&row.content_json).unwrap_or(MessageContent::AssistantPlaceholder {});
    // 本轮上下文说明来自任务结果(M09 写入 contextNote;不含正文/密钥/路径)。
    let context_note = match &row.task_id {
        Some(tid) => task_context_note_tx(conn, tid)?,
        None => None,
    };
    // 可选思考记录:表未创建(旧库/模块单测)或没有记录时均为 None。
    let reasoning = conn
        .query_row(
            "SELECT text FROM agent_reasoning WHERE message_id=?1",
            [&row.id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten();
    Ok(MessageView {
        id: row.id.clone(),
        conversation_id: row.conversation_id.clone(),
        document_id: document_id.to_string(),
        role: row.role,
        snapshot_id: row.snapshot_id.clone(),
        snapshot,
        task_id: row.task_id.clone(),
        task_status,
        content,
        answer_status: row.answer_status,
        model_provider_id: row.model_provider_id.clone(),
        model_id: row.model_id.clone(),
        error_code: row.error_code.clone(),
        reasoning,
        context_note,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// 从任务结果读取 contextNote(缺省/不可解析一律 None,不猜测内容)。
fn task_context_note_tx(conn: &Connection, task_id: &str) -> Result<Option<String>, AppError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT result_json FROM task_result WHERE task_id=?1",
            [task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Ok(None);
    };
    Ok(value
        .get("contextNote")
        .and_then(|note| note.as_str())
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(str::to_string))
}

// —— 消息事务核心(供 M09 在任务事务内调用;《01》§9"创建问答")——

/// 用户消息输入。
pub struct NewUserMessage<'a> {
    pub conversation_id: &'a str,
    pub snapshot_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub text: &'a str,
    pub idempotency_key: &'a str,
}

/// 助手占位消息输入。
pub struct NewAssistantMessage<'a> {
    pub conversation_id: &'a str,
    pub snapshot_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub idempotency_key: &'a str,
}

/// 校验消息与会话/快照/任务同文档(跨文档一律拒绝)。
fn validate_message_binding_tx(
    conn: &Connection,
    conversation: &ConversationRow,
    snapshot_id: Option<&str>,
    task_id: Option<&str>,
) -> Result<(), AppError> {
    if let Some(sid) = snapshot_id {
        let snap = load_snapshot_tx(conn, sid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
        if snap.document_id != conversation.document_id {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "snapshot_document_mismatch".into()));
        }
    }
    if let Some(tid) = task_id {
        let task_doc: Option<Option<String>> = conn
            .query_row("SELECT document_id FROM task WHERE id=?1", [tid], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| db_app_error(&e))?;
        match task_doc {
            None => {
                return Err(
                    err(CoreErrorCode::NotFound).with_detail("reason", "task_not_found".into())
                );
            }
            Some(Some(doc)) if doc != conversation.document_id => {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "cross_document_task".into()));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// 在任务创建事务内写入用户消息(与 Task 同事务;《01》§9)。
/// 返回 (消息行, 文档 ID)。
pub fn create_user_message_tx(
    conn: &Connection,
    input: &NewUserMessage<'_>,
    new_id: &str,
    now: i64,
) -> Result<(MessageRow, String), AppError> {
    if input.idempotency_key.trim().is_empty() {
        return Err(err(CoreErrorCode::InvalidRequest));
    }
    if let Some(existing) = load_message_by_key_tx(conn, input.idempotency_key)? {
        let conv = load_conversation_tx(conn, &existing.conversation_id)?
            .ok_or_else(|| err(CoreErrorCode::Internal))?;
        return Ok((existing, conv.document_id));
    }
    let conv = load_conversation_tx(conn, input.conversation_id)?
        .ok_or_else(|| err(CoreErrorCode::NotFound))?;
    validate_message_binding_tx(conn, &conv, input.snapshot_id, input.task_id)?;
    let content = MessageContent::UserMessage {
        text: input.text.to_string(),
    };
    conn.execute(
        "INSERT INTO message(id, conversation_id, role, snapshot_id, task_id, content_json, \
         answer_status, idempotency_key, created_at, updated_at) \
         VALUES (?1,?2,'user',?3,?4,?5,'completed',?6,?7,?7)",
        params![
            new_id,
            input.conversation_id,
            input.snapshot_id,
            input.task_id,
            serde_json::to_string(&content).unwrap_or_default(),
            input.idempotency_key,
            now,
        ],
    )
    .map_err(|e| db_app_error(&e))?;
    conn.execute(
        "UPDATE conversation SET updated_at=?2 WHERE id=?1",
        params![input.conversation_id, now],
    )
    .map_err(|e| db_app_error(&e))?;
    let row = load_message_tx(conn, new_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
    Ok((row, conv.document_id))
}

/// 在模型调用前创建助手占位消息(task_id 唯一索引 ⇒ 天然幂等)。
pub fn create_assistant_placeholder_tx(
    conn: &Connection,
    input: &NewAssistantMessage<'_>,
    new_id: &str,
    now: i64,
) -> Result<(MessageRow, String), AppError> {
    if input.idempotency_key.trim().is_empty() {
        return Err(err(CoreErrorCode::InvalidRequest));
    }
    if let Some(existing) = load_message_by_key_tx(conn, input.idempotency_key)? {
        let conv = load_conversation_tx(conn, &existing.conversation_id)?
            .ok_or_else(|| err(CoreErrorCode::Internal))?;
        return Ok((existing, conv.document_id));
    }
    let conv = load_conversation_tx(conn, input.conversation_id)?
        .ok_or_else(|| err(CoreErrorCode::NotFound))?;
    validate_message_binding_tx(conn, &conv, input.snapshot_id, input.task_id)?;
    let content = MessageContent::AssistantPlaceholder {};
    conn.execute(
        "INSERT INTO message(id, conversation_id, role, snapshot_id, task_id, content_json, \
         answer_status, idempotency_key, created_at, updated_at) \
         VALUES (?1,?2,'assistant',?3,?4,?5,'pending',?6,?7,?7)",
        params![
            new_id,
            input.conversation_id,
            input.snapshot_id,
            input.task_id,
            serde_json::to_string(&content).unwrap_or_default(),
            input.idempotency_key,
            now,
        ],
    )
    .map_err(|e| db_app_error(&e))?;
    conn.execute(
        "UPDATE conversation SET updated_at=?2 WHERE id=?1",
        params![input.conversation_id, now],
    )
    .map_err(|e| db_app_error(&e))?;
    let row = load_message_tx(conn, new_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
    Ok((row, conv.document_id))
}

/// 校验 AnswerContent 形状(《01》§5.2):
/// 段落 ID 唯一;document_fact 段标 structurally_valid 时至少一条引文;
/// 引文 ID 双向引用一致。未通过校验的 JSON 不入库。
///
/// 注意:`insufficient_evidence`/`conflict`/`out_of_scope` 按契约本就**不带正文**
/// (M09 会清空 segments),它们的可见信息在 `unverified_notes` 里。因此
/// “段为空”只在 `answered` 状态下视为非法,否则会把“证据不足”这条
/// 合法结论误判成 INVALID_REQUEST,用户只看到错误码。
pub fn validate_answer_content(answer: &AnswerContent) -> Result<(), AppError> {
    if answer.segments.is_empty()
        && matches!(
            answer.status,
            crate::retrieval_ai::models::AnswerStatus::Answered
        )
    {
        return Err(err(CoreErrorCode::InvalidRequest)
            .with_detail("reason", "answer_segments_empty".into()));
    }
    let mut segment_ids = Vec::with_capacity(answer.segments.len());
    for seg in &answer.segments {
        if seg.id.trim().is_empty() {
            return Err(
                err(CoreErrorCode::InvalidRequest).with_detail("reason", "segment_id_empty".into())
            );
        }
        if segment_ids.contains(&seg.id) {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "segment_id_duplicate".into()));
        }
        segment_ids.push(seg.id.clone());
        if seg.kind == SegmentKind::DocumentFact
            && seg.verification_status == "structurally_valid"
            && seg.citation_ids.is_empty()
        {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "document_fact_requires_citation".into()));
        }
    }
    let citation_ids: Vec<&str> = answer.citations.iter().map(|c| c.id.as_str()).collect();
    for seg in &answer.segments {
        for cid in &seg.citation_ids {
            if !citation_ids.contains(&cid.as_str()) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "citation_reference_missing".into()));
            }
        }
    }
    for c in &answer.citations {
        if c.block_id.trim().is_empty() || c.quoted_text.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "citation_incomplete".into()));
        }
        for sid in &c.claim_segment_ids {
            if !segment_ids.contains(sid) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "claim_segment_missing".into()));
            }
        }
    }
    Ok(())
}

/// 在 M01 commit_result 事务内幂等完成助手消息(替换占位内容)。
/// completed 重复提交返回现状,零额外写入;failed/canceled 拒绝改写。
pub fn complete_assistant_message_tx(
    conn: &Connection,
    message_id: &str,
    answer: &AnswerContent,
    model_provider_id: Option<&str>,
    model_id: Option<&str>,
    now: i64,
) -> Result<(MessageRow, String), AppError> {
    let row = load_message_tx(conn, message_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
    let conv = load_conversation_tx(conn, &row.conversation_id)?
        .ok_or_else(|| err(CoreErrorCode::Internal))?;
    match row.answer_status {
        AnswerLifecycle::Completed => return Ok((row, conv.document_id)),
        AnswerLifecycle::Pending => {}
        AnswerLifecycle::Failed | AnswerLifecycle::Canceled => {
            return Err(err(CoreErrorCode::TaskNotRunning));
        }
    }
    validate_answer_content(answer)?;
    let content = MessageContent::Answer {
        content: answer.clone(),
    };
    conn.execute(
        "UPDATE message SET content_json=?2, answer_status='completed', \
         model_provider_id=?3, model_id=?4, error_code=NULL, updated_at=?5 WHERE id=?1",
        params![
            message_id,
            serde_json::to_string(&content).unwrap_or_default(),
            model_provider_id,
            model_id,
            now,
        ],
    )
    .map_err(|e| db_app_error(&e))?;
    conn.execute(
        "UPDATE conversation SET updated_at=?2 WHERE id=?1",
        params![row.conversation_id, now],
    )
    .map_err(|e| db_app_error(&e))?;
    let updated = load_message_tx(conn, message_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
    Ok((updated, conv.document_id))
}

/// 完成 Agent 助手消息(agent.v1):与 answer.v1 完成路径同语义,
/// 但保存最终答复 + 步骤快照;verified=false 的自由文本也允许保存。
pub fn complete_agent_message_tx(
    conn: &Connection,
    message_id: &str,
    answer: &AgentAnswerV1,
    model_provider_id: Option<&str>,
    model_id: Option<&str>,
    now: i64,
) -> Result<(MessageRow, String), AppError> {
    let row = load_message_tx(conn, message_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
    let conv = load_conversation_tx(conn, &row.conversation_id)?
        .ok_or_else(|| err(CoreErrorCode::Internal))?;
    match row.answer_status {
        AnswerLifecycle::Completed => return Ok((row, conv.document_id)),
        AnswerLifecycle::Pending => {}
        AnswerLifecycle::Failed | AnswerLifecycle::Canceled => {
            return Err(err(CoreErrorCode::TaskNotRunning));
        }
    }
    if answer.verified {
        validate_answer_content(&answer.answer)?;
    }
    let content = MessageContent::AgentAnswer {
        content: answer.clone(),
    };
    conn.execute(
        "UPDATE message SET content_json=?2, answer_status='completed', \
         model_provider_id=?3, model_id=?4, error_code=NULL, updated_at=?5 WHERE id=?1",
        params![
            message_id,
            serde_json::to_string(&content).unwrap_or_default(),
            model_provider_id,
            model_id,
            now,
        ],
    )
    .map_err(|e| db_app_error(&e))?;
    conn.execute(
        "UPDATE conversation SET updated_at=?2 WHERE id=?1",
        params![row.conversation_id, now],
    )
    .map_err(|e| db_app_error(&e))?;
    let updated = load_message_tx(conn, message_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
    Ok((updated, conv.document_id))
}

/// 失败/取消:保留问题、快照与错误原因,不保存伪答案。
pub fn fail_assistant_message_tx(
    conn: &Connection,
    message_id: &str,
    error_code: &str,
    canceled: bool,
    now: i64,
) -> Result<(MessageRow, String), AppError> {
    let row = load_message_tx(conn, message_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
    let conv = load_conversation_tx(conn, &row.conversation_id)?
        .ok_or_else(|| err(CoreErrorCode::Internal))?;
    if row.answer_status != AnswerLifecycle::Pending {
        return Ok((row, conv.document_id));
    }
    conn.execute(
        "UPDATE message SET answer_status=?2, error_code=?3, updated_at=?4 WHERE id=?1",
        params![
            message_id,
            if canceled { "canceled" } else { "failed" },
            error_code,
            now,
        ],
    )
    .map_err(|e| db_app_error(&e))?;
    conn.execute(
        "UPDATE conversation SET updated_at=?2 WHERE id=?1",
        params![row.conversation_id, now],
    )
    .map_err(|e| db_app_error(&e))?;
    let updated = load_message_tx(conn, message_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
    Ok((updated, conv.document_id))
}

/// 失败/取消并写入可见通知:仅当消息仍为 pending 时生效;重复调用幂等。
pub fn fail_assistant_message_with_notice_tx(
    conn: &Connection,
    message_id: &str,
    error_code: &str,
    canceled: bool,
    notice: &crate::context::models::AssistantNoticeV1,
    now: i64,
) -> Result<(MessageRow, String), AppError> {
    let row = load_message_tx(conn, message_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
    let conv = load_conversation_tx(conn, &row.conversation_id)?
        .ok_or_else(|| err(CoreErrorCode::Internal))?;
    if row.answer_status != AnswerLifecycle::Pending {
        return Ok((row, conv.document_id));
    }
    let content = MessageContent::AssistantNotice {
        notice: notice.clone(),
    };
    conn.execute(
        "UPDATE message SET content_json=?2, answer_status=?3, error_code=?4, updated_at=?5 \
         WHERE id=?1",
        params![
            message_id,
            serde_json::to_string(&content).unwrap_or_default(),
            if canceled { "canceled" } else { "failed" },
            error_code,
            now,
        ],
    )
    .map_err(|e| db_app_error(&e))?;
    conn.execute(
        "UPDATE conversation SET updated_at=?2 WHERE id=?1",
        params![row.conversation_id, now],
    )
    .map_err(|e| db_app_error(&e))?;
    let updated = load_message_tx(conn, message_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
    Ok((updated, conv.document_id))
}

/// Citation 属主校验(message):M13 组装时注册到 evidence。
/// 调用方(M09/M16)使用 `message:{id}` 作为属主 ID,这里同时接受裸消息 ID,
/// 避免两套约定导致 owner_not_found。
pub fn message_owner_exists_tx(
    conn: &Connection,
    message_id: &str,
    document_id: &str,
) -> Result<bool, AppError> {
    let id = message_id
        .strip_prefix("message:")
        .unwrap_or(message_id)
        .to_string();
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM message m JOIN conversation c ON c.id = m.conversation_id \
             WHERE m.id=?1 AND c.document_id=?2",
            params![id, document_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    Ok(exists.is_some())
}

fn load_message_tx(conn: &Connection, id: &str) -> Result<Option<MessageRow>, AppError> {
    conn.query_row(
        &format!("SELECT {MESSAGE_COLS} FROM message WHERE id=?1"),
        [id],
        |r| MessageRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_message_by_key_tx(conn: &Connection, key: &str) -> Result<Option<MessageRow>, AppError> {
    conn.query_row(
        &format!("SELECT {MESSAGE_COLS} FROM message WHERE idempotency_key=?1"),
        [key],
        |r| MessageRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

// —— 单元测试(纯函数;集成见 service_tests.rs)——

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate_mixed_cjk_latin() {
        // 4 CJK + 4 latin → 4 + 1 = 5
        assert_eq!(estimate_tokens("知识图谱abcd"), 5);
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("知识"), 2);
        // 5 latin → ceil(5/4) = 2
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn context_error_codes_match_contract() {
        let e = ContextErrorCode::CoverageIncomplete.to_app_error();
        assert_eq!(e.code, "CONTEXT_COVERAGE_INCOMPLETE");
        assert_eq!(e.user_action, crate::shared::UserAction::WaitForIndex);
        assert!(!e.retryable);
        let e = ContextErrorCode::BudgetExceeded.to_app_error();
        assert_eq!(e.user_action, crate::shared::UserAction::ChooseScope);
        let e = ContextErrorCode::SnapshotImmutable.to_app_error();
        assert_eq!(e.code, "CONTEXT_SNAPSHOT_IMMUTABLE");
        assert_eq!(e.user_action, crate::shared::UserAction::None);
    }

    #[test]
    fn answer_content_validation_rejects_unverified_fact() {
        let bad = AnswerContent {
            schema_version: "answer.v1".into(),
            status: AnswerStatus::Answered,
            segments: vec![AnswerSegment {
                id: "s1".into(),
                kind: SegmentKind::DocumentFact,
                text: "结论".into(),
                verification_status: "structurally_valid".into(),
                citation_ids: vec![],
            }],
            citations: vec![],
            conflicts: vec![],
            suggestions: vec![],
            unverified_notes: vec![],
        };
        assert!(validate_answer_content(&bad).is_err());
    }

    #[test]
    fn scope_draft_serde_roundtrip() {
        let scope = ScopeDraft::Pages {
            start_page: 2,
            end_page: 5,
        };
        let json = serde_json::to_string(&scope).unwrap();
        assert!(json.contains("\"kind\":\"pages\""));
        let back: ScopeDraft = serde_json::from_str(&json).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&scope).unwrap()
        );
    }
}
