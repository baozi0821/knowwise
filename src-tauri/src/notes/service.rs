//! M06 标注、书签、笔记与待澄清服务(《M06》§2–§8)。
//!
//! 关键不变量:
//! - 用户资产优先:笔记/标注/待澄清保存走 M01 写线程独立事务,不等待后台任务。
//! - 模块只使用 M05 创建的 Anchor(经 `evidence::anchor_view_by_id_tx` /
//!   `anchor_summary_tx` 读取),不自行计算页码或块身份。
//! - 锚点状态由 Anchor 派生:active 可回跳、orphaned 需重新确认、invalid 显示快照;
//!   文件变化后不自动迁移。
//! - `origin` 区分用户笔记与 AI 草稿;AI 草稿编辑后转 user_edited_ai 仍保留来源。
//! - 自动保存真实性:只有 Rust 事务成功返回后才算"已保存";冲突保留本地编辑并
//!   返回 CONFLICT_REVISION_MISMATCH,不静默覆盖。
//! - 笔记/待澄清删除时在同一事务清理其拥有的 Citation(M05 §9 归属级联)。
//! - 事件(userdata.changed)只在 COMMIT 成功后发出。

use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};

use crate::evidence::models::{AnchorStatus, CitationOwnerType};
use crate::evidence::service::{
    anchor_summary_tx, anchor_view_by_id_tx, delete_citations_for_owner_tx,
};
use crate::persistence::{db_app_error, Db};
use crate::shared::events::{DomainEvent, DomainEventType, EventSink};
use crate::shared::locator::SourceLocator;
use crate::shared::paging::{encode_cursor, PageRequest};
use crate::shared::{AppError, Clock, CoreErrorCode, IdGenerator};

use super::models::*;

/// 笔记正文上限(UI 显示剩余字数;超限拒绝,不静默截断)。
pub const MAX_NOTE_BODY_CHARS: usize = 100_000;
/// 每条笔记标签数上限。
pub const MAX_NOTE_TAGS: usize = 20;
/// 单个标签长度上限(字符)。
pub const MAX_TAG_CHARS: usize = 64;
/// 标注备注上限。
pub const MAX_ANNOTATION_TEXT_CHARS: usize = 2_000;
/// 待澄清问题文本上限。
pub const MAX_CLARIFICATION_CHARS: usize = 10_000;

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

// —— 属主校验器(M05 Citation owner 注册;纯函数,事务内执行)——

/// M05 Citation 属主 = note 时的存在性校验。
pub fn note_owner_exists_tx(
    conn: &Connection,
    note_id: &str,
    document_id: &str,
) -> Result<bool, AppError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM note WHERE id=?1 AND document_id=?2",
            params![note_id, document_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    Ok(exists.is_some())
}

/// M05 Citation 属主 = clarification 时的存在性校验。
pub fn clarification_owner_exists_tx(
    conn: &Connection,
    clarification_id: &str,
    document_id: &str,
) -> Result<bool, AppError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM clarification_item WHERE id=?1 AND document_id=?2",
            params![clarification_id, document_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    Ok(exists.is_some())
}

// —— 正文与标签策略 ——

/// 笔记正文策略:长度上限 + 安全 Markdown 子集的禁用内容
/// (渲染端仍以 DOMPurify 兜底;此处拒绝保存,不静默截断)。
pub fn validate_note_body(body: &str) -> Result<(), AppError> {
    if body.chars().count() > MAX_NOTE_BODY_CHARS {
        return Err(err(CoreErrorCode::InvalidRequest)
            .with_detail("reason", "note_body_too_long".into())
            .with_detail("maxChars", (MAX_NOTE_BODY_CHARS as i64).into()));
    }
    let lower = body.to_lowercase();
    for banned in ["<script", "javascript:", "data:text/html"] {
        if lower.contains(banned) {
            return Err(
                err(CoreErrorCode::InvalidRequest).with_detail("reason", "note_body_unsafe".into())
            );
        }
    }
    Ok(())
}

/// 标签规范化:折叠空白 + 小写(唯一性);显示名保留大小写。
pub fn normalize_tag_name(name: &str) -> Option<(String, String)> {
    let collapsed: String = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() || collapsed.chars().count() > MAX_TAG_CHARS {
        return None;
    }
    Some((collapsed.to_lowercase(), collapsed))
}

// —— 行加载 ——

fn load_note_tx(conn: &Connection, note_id: &str) -> Result<Option<NoteRow>, AppError> {
    conn.query_row(
        &format!("SELECT {NOTE_COLS} FROM note WHERE id=?1"),
        [note_id],
        |r| NoteRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_note_by_key_tx(conn: &Connection, key: &str) -> Result<Option<NoteRow>, AppError> {
    conn.query_row(
        &format!("SELECT {NOTE_COLS} FROM note WHERE idempotency_key=?1"),
        [key],
        |r| NoteRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_annotation_tx(conn: &Connection, id: &str) -> Result<Option<AnnotationRow>, AppError> {
    conn.query_row(
        "SELECT id, document_id, anchor_id, color, note_text, remark, revision, created_at, \
         updated_at FROM annotation WHERE id=?1",
        [id],
        |r| {
            Ok(AnnotationRow {
                id: r.get(0)?,
                document_id: r.get(1)?,
                anchor_id: r.get(2)?,
                color: AnnotationColor::parse_db(&r.get::<_, String>(3)?)
                    .unwrap_or(AnnotationColor::Yellow),
                note_text: r.get(4)?,
                remark: r.get(5)?,
                revision: r.get(6)?,
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_clarification_tx(
    conn: &Connection,
    id: &str,
) -> Result<Option<ClarificationRow>, AppError> {
    conn.query_row(
        &format!("SELECT {CLARIFICATION_COLS} FROM clarification_item WHERE id=?1"),
        [id],
        |r| ClarificationRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn document_exists_tx(conn: &Connection, document_id: &str) -> Result<bool, AppError> {
    let exists: Option<i64> = conn
        .query_row("SELECT 1 FROM document WHERE id=?1", [document_id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| db_app_error(&e))?;
    Ok(exists.is_some())
}

fn current_version_id_tx(conn: &Connection, document_id: &str) -> Result<Option<String>, AppError> {
    conn.query_row(
        "SELECT current_version_id FROM document WHERE id=?1",
        [document_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| db_app_error(&e))?
    .ok_or_else(|| err(CoreErrorCode::DocNotFound))
}

fn note_tags_tx(conn: &Connection, note_id: &str) -> Result<Vec<String>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT t.display_name FROM note_tag nt JOIN tag t ON t.id = nt.tag_id \
             WHERE nt.note_id=?1 ORDER BY t.display_name",
        )
        .map_err(|e| db_app_error(&e))?;
    let tags = stmt
        .query_map([note_id], |r| r.get(0))
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<Vec<String>, _>>()
        .map_err(|e| db_app_error(&e))?;
    Ok(tags)
}

/// 来源引用当前是否仍存在(读取时校验;删除后保留快照并提示不可用)。
fn source_ref_available_tx(conn: &Connection, r: &SourceReference) -> bool {
    match r.source_type {
        SourceRefType::Note => conn
            .query_row("SELECT 1 FROM note WHERE id=?1", [&r.source_id], |_| Ok(()))
            .optional()
            .is_ok_and(|x| x.is_some()),
        SourceRefType::ContentBlock => conn
            .query_row(
                "SELECT 1 FROM content_block WHERE id=?1",
                [&r.source_id],
                |_| Ok(()),
            )
            .optional()
            .is_ok_and(|x| x.is_some()),
        // message / artifact_version / selection:M07/M10 前向引用,快照已保存。
        SourceRefType::Message | SourceRefType::ArtifactVersion | SourceRefType::Selection => true,
        SourceRefType::Annotation => conn
            .query_row(
                "SELECT 1 FROM annotation WHERE id=?1",
                [&r.source_id],
                |_| Ok(()),
            )
            .optional()
            .is_ok_and(|x| x.is_some()),
    }
}

fn note_view_tx(conn: &Connection, row: &NoteRow) -> Result<NoteView, AppError> {
    let anchor_status = match &row.anchor_id {
        Some(aid) => anchor_summary_tx(conn, aid)?.map(|a| a.status),
        None => None,
    };
    let source_refs: Vec<SourceReference> =
        serde_json::from_str(&row.source_refs_json).unwrap_or_default();
    let availability = source_refs
        .iter()
        .map(|r| source_ref_available_tx(conn, r))
        .collect();
    let locator = match &row.anchor_id {
        Some(anchor_id) => conn
            .query_row(
                "SELECT primary_locator_json FROM anchor WHERE id=?1",
                [anchor_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok()),
        None => None,
    };
    Ok(NoteView {
        id: row.id.clone(),
        document_id: row.document_id.clone(),
        document_version_id: row.document_version_id.clone(),
        anchor_id: row.anchor_id.clone(),
        anchor_status,
        origin: row.origin,
        body: row.body.clone(),
        tags: note_tags_tx(conn, &row.id)?,
        status: row.status,
        source_refs,
        source_refs_available: availability,
        locator,
        revision: row.revision,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn annotation_view_tx(conn: &Connection, row: &AnnotationRow) -> Result<AnnotationView, AppError> {
    let anchor = anchor_view_by_id_tx(conn, &row.anchor_id)?;
    let (status, hint, quote, locator, page) = match anchor {
        Some(a) => {
            let first_quote = a
                .blocks
                .first()
                .map(|b| b.quote_text.clone())
                .unwrap_or_default();
            let hint = match a.status {
                AnchorStatus::Active => "可回跳原文".to_string(),
                AnchorStatus::Orphaned => "原文位置需重新确认".to_string(),
                AnchorStatus::Relocated => "已重定位到新版本".to_string(),
                AnchorStatus::Invalid => "原文证据不匹配,显示快照".to_string(),
            };
            (
                a.status,
                hint,
                first_quote,
                Some(a.primary_locator.clone()),
                a.overlay_pages.first().copied(),
            )
        }
        None => (
            AnchorStatus::Invalid,
            "原文证据不匹配,显示快照".to_string(),
            String::new(),
            None,
            None,
        ),
    };
    Ok(AnnotationView {
        id: row.id.clone(),
        document_id: row.document_id.clone(),
        anchor_id: row.anchor_id.clone(),
        color: row.color,
        note_text: row.note_text.clone(),
        remark: row.remark.clone(),
        anchor_status: status,
        status_hint: hint,
        quote_text: quote,
        locator,
        page_number: page,
        revision: row.revision,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn bookmark_view_tx(row: &BookmarkRow) -> Option<BookmarkView> {
    let locator: SourceLocator = serde_json::from_str(&row.locator_json).ok()?;
    Some(BookmarkView {
        id: row.id.clone(),
        document_id: row.document_id.clone(),
        document_version_id: row.document_version_id.clone(),
        anchor_id: row.anchor_id.clone(),
        label: row.label.clone(),
        note: row.note.clone(),
        page_number: locator.pdf_page(),
        locator,
        created_at: row.created_at,
    })
}

fn clarification_source_available_tx(
    conn: &Connection,
    source_type: Option<ClarificationSourceType>,
    source_id: Option<&str>,
    document_id: &str,
) -> bool {
    let Some(st) = source_type else {
        return false;
    };
    let Some(sid) = source_id else {
        return false;
    };
    match st {
        ClarificationSourceType::Annotation => conn
            .query_row(
                "SELECT 1 FROM annotation WHERE id=?1 AND document_id=?2",
                params![sid, document_id],
                |_| Ok(()),
            )
            .optional()
            .is_ok_and(|x| x.is_some()),
        ClarificationSourceType::Note => conn
            .query_row(
                "SELECT 1 FROM note WHERE id=?1 AND document_id=?2",
                params![sid, document_id],
                |_| Ok(()),
            )
            .optional()
            .is_ok_and(|x| x.is_some()),
        // question/answer/quiz:M07/M11 前向引用,以快照为准。
        ClarificationSourceType::Question
        | ClarificationSourceType::Answer
        | ClarificationSourceType::Quiz => true,
    }
}

fn clarification_view_tx(
    conn: &Connection,
    row: &ClarificationRow,
) -> Result<ClarificationView, AppError> {
    Ok(ClarificationView {
        id: row.id.clone(),
        document_id: row.document_id.clone(),
        question_text: row.question_text.clone(),
        remark: row.remark.clone(),
        source_type: row.source_type,
        source_id: row.source_id.clone(),
        source_snapshot: serde_json::from_str(&row.source_snapshot_json).unwrap_or_default(),
        source_available: clarification_source_available_tx(
            conn,
            row.source_type,
            row.source_id.as_deref(),
            &row.document_id,
        ),
        status: row.status,
        resolution_ref: row
            .resolution_ref_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok()),
        resolved_at: row.resolved_at,
        revision: row.revision,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

// —— 服务 ——

pub struct NotesService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    events: Arc<dyn EventSink>,
}

impl NotesService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            events,
        }
    }

    fn emit_userdata(
        &self,
        entity_type: &str,
        entity_id: &str,
        action: &str,
        revision: i64,
        document_id: &str,
    ) {
        // 事件只在 COMMIT 成功后由调用点触发;此处只负责封装信封。
        let event = DomainEvent::new(
            DomainEventType::UserdataChanged,
            entity_id.to_string(),
            revision,
            self.clock.now_ms(),
            serde_json::json!({
                "entityType": entity_type,
                "entityId": entity_id,
                "action": action,
                "revision": revision,
            }),
        )
        .with_document(Some(document_id.to_string()), None);
        self.events.emit(event);
    }

    // —— 标注(《M06》§2)——

    /// `annotation_create`:文档版本由 Anchor 固定;幂等。
    pub fn create_annotation(
        &self,
        input: &AnnotationCreateInput,
        idempotency_key: &str,
    ) -> Result<AnnotationView, AppError> {
        if input.note_text.chars().count() > MAX_ANNOTATION_TEXT_CHARS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "annotation_text_too_long".into()));
        }
        if input.remark.as_deref().unwrap_or("").chars().count() > MAX_ANNOTATION_TEXT_CHARS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "annotation_remark_too_long".into()));
        }
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let input2 = input.clone();
        let key = idempotency_key.to_string();
        let view = self.db.write(move |conn| {
            // 幂等(事务内复核,先于校验)。
            if let Some(row) = load_annotation_by_key_tx(conn, &key)? {
                return annotation_view_tx(conn, &row);
            }
            if !document_exists_tx(conn, &input2.document_id)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            // 只使用 M05 创建的 Anchor:存在且属于本文档。
            let anchor = anchor_summary_tx(conn, &input2.anchor_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if anchor.document_id != input2.document_id {
                return Err(err(CoreErrorCode::NotFound));
            }
            conn.execute(
                "INSERT INTO annotation(id, document_id, anchor_id, color, note_text, remark, \
                 revision, idempotency_key, created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,1,?7,?8,?8)",
                params![
                    id,
                    input2.document_id,
                    input2.anchor_id,
                    input2.color.as_str(),
                    input2.note_text,
                    input2.remark.unwrap_or_default(),
                    key,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            let row = load_annotation_tx(conn, &id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            annotation_view_tx(conn, &row)
        })?;
        self.emit_userdata(
            "annotation",
            &view.id,
            "created",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// `annotation_update`:只允许颜色、备注等非锚点字段。
    pub fn update_annotation(
        &self,
        input: &AnnotationUpdateInput,
    ) -> Result<AnnotationView, AppError> {
        if input.note_text.chars().count() > MAX_ANNOTATION_TEXT_CHARS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "annotation_text_too_long".into()));
        }
        if input.remark.as_deref().unwrap_or("").chars().count() > MAX_ANNOTATION_TEXT_CHARS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "annotation_remark_too_long".into()));
        }
        let now = self.clock.now_ms();
        let aid = input.annotation_id.clone();
        let color = input.color.as_str().to_string();
        let text = input.note_text.clone();
        let remark = input.remark.clone();
        let expected = input.expected_revision;
        let view = self.db.write(move |conn| {
            let row =
                load_annotation_tx(conn, &aid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.revision != expected {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch));
            }
            conn.execute(
                "UPDATE annotation SET color=?2, note_text=?3, \
                 remark=CASE WHEN ?6 IS NULL THEN remark ELSE ?6 END, \
                 revision=revision+1, updated_at=?4 \
                 WHERE id=?1 AND revision=?5",
                params![aid, color, text, now, expected, remark],
            )
            .map_err(|e| db_app_error(&e))?;
            let updated =
                load_annotation_tx(conn, &aid)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            annotation_view_tx(conn, &updated)
        })?;
        self.emit_userdata(
            "annotation",
            &view.id,
            "updated",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// `annotation_delete`:明确删除,不级联删笔记。
    pub fn delete_annotation(
        &self,
        annotation_id: &str,
        expected_revision: i64,
    ) -> Result<crate::shared::dto::DeleteResult, AppError> {
        let aid = annotation_id.to_string();
        let (deleted, document_id) = self.db.write(move |conn| {
            let row =
                load_annotation_tx(conn, &aid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.revision != expected_revision {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch));
            }
            conn.execute("DELETE FROM annotation WHERE id=?1", params![aid])
                .map_err(|e| db_app_error(&e))?;
            Ok((true, row.document_id))
        })?;
        self.emit_userdata("annotation", annotation_id, "deleted", 0, &document_id);
        Ok(crate::shared::dto::DeleteResult {
            deleted,
            id: annotation_id.to_string(),
        })
    }

    pub fn list_annotations(&self, document_id: &str) -> Result<Vec<AnnotationView>, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, document_id, anchor_id, color, note_text, remark, revision, \
                     created_at, updated_at FROM annotation WHERE document_id=?1 \
                     ORDER BY created_at, id",
                )
                .map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map([&did], |r| {
                    Ok(AnnotationRow {
                        id: r.get(0)?,
                        document_id: r.get(1)?,
                        anchor_id: r.get(2)?,
                        color: AnnotationColor::parse_db(&r.get::<_, String>(3)?)
                            .unwrap_or(AnnotationColor::Yellow),
                        note_text: r.get(4)?,
                        remark: r.get(5)?,
                        revision: r.get(6)?,
                        created_at: r.get(7)?,
                        updated_at: r.get(8)?,
                    })
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            rows.iter()
                .map(|row| annotation_view_tx(conn, row))
                .collect()
        })
    }

    // —— 书签(《M06》§2)——

    /// `bookmark_toggle`:期望状态语义,双击不产生两个相反结果。
    pub fn toggle_bookmark(
        &self,
        document_id: &str,
        anchor_id: Option<&str>,
        locator: &SourceLocator,
        label: &str,
        expected_state: bool,
    ) -> Result<BookmarkResult, AppError> {
        if document_id.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let toggle_key = match anchor_id {
            Some(a) => format!("anchor:{a}"),
            None => format!("locator:{}", locator.canonical_string()),
        };
        let now = self.clock.now_ms();
        let did = document_id.to_string();
        let aid = anchor_id.map(|s| s.to_string());
        let loc_json = serde_json::to_string(locator).unwrap_or_default();
        let label2 = label.to_string();
        let key = toggle_key.clone();
        let (result, affected_id) = self.db.write(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            let existing: Option<BookmarkExisting> = conn
                .query_row(
                    "SELECT id, document_version_id, anchor_id, label, note, created_at \
                     FROM bookmark WHERE document_id=?1 AND toggle_key=?2",
                    params![did, key],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if expected_state {
                if let Some((id, version, a, lbl, note, created)) = existing {
                    // 已存在:幂等返回,不重复创建(双击安全)。
                    let row = BookmarkRow {
                        id: id.clone(),
                        document_id: did.clone(),
                        document_version_id: version,
                        anchor_id: a,
                        toggle_key: key.clone(),
                        label: lbl,
                        note,
                        locator_json: loc_json.clone(),
                        created_at: created,
                    };
                    return Ok((
                        BookmarkResult {
                            document_id: did.clone(),
                            state: true,
                            bookmark: bookmark_view_tx(&row),
                        },
                        id,
                    ));
                }
                // 新建:锚点存在则版本由锚点固定,否则用当前版本。
                let version = if let Some(a) = &aid {
                    let anchor =
                        anchor_summary_tx(conn, a)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
                    if anchor.document_id != did {
                        return Err(err(CoreErrorCode::NotFound));
                    }
                    Some(anchor.document_version_id)
                } else {
                    current_version_id_tx(conn, &did)?
                };
                let id = crate::shared::new_trace_id();
                conn.execute(
                    "INSERT INTO bookmark(id, document_id, document_version_id, anchor_id, \
                     toggle_key, locator_json, label, note, created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,'',?8)",
                    params![id, did, version, aid, key, loc_json, label2, now],
                )
                .map_err(|e| db_app_error(&e))?;
                let row = BookmarkRow {
                    id: id.clone(),
                    document_id: did.clone(),
                    document_version_id: version,
                    anchor_id: aid,
                    toggle_key: key,
                    label: label2,
                    note: String::new(),
                    locator_json: loc_json,
                    created_at: now,
                };
                Ok((
                    BookmarkResult {
                        document_id: did.clone(),
                        state: true,
                        bookmark: bookmark_view_tx(&row),
                    },
                    id,
                ))
            } else {
                if let Some((id, ..)) = existing {
                    conn.execute(
                        "DELETE FROM bookmark WHERE id=?1 AND document_id=?2",
                        params![id, did],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    return Ok((
                        BookmarkResult {
                            document_id: did.clone(),
                            state: false,
                            bookmark: None,
                        },
                        id,
                    ));
                }
                Ok((
                    BookmarkResult {
                        document_id: did.clone(),
                        state: false,
                        bookmark: None,
                    },
                    String::new(),
                ))
            }
        })?;
        if !affected_id.is_empty() {
            self.emit_userdata("bookmark", &affected_id, "toggled", 1, document_id);
        }
        Ok(result)
    }

    pub fn list_bookmarks(&self, document_id: &str) -> Result<Vec<BookmarkView>, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, document_id, document_version_id, anchor_id, toggle_key, label, \
                     note, locator_json, created_at FROM bookmark WHERE document_id=?1 \
                     ORDER BY created_at, id",
                )
                .map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map([&did], |r| {
                    Ok(BookmarkRow {
                        id: r.get(0)?,
                        document_id: r.get(1)?,
                        document_version_id: r.get(2)?,
                        anchor_id: r.get(3)?,
                        toggle_key: r.get(4)?,
                        label: r.get(5)?,
                        note: r.get(6)?,
                        locator_json: r.get(7)?,
                        created_at: r.get(8)?,
                    })
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(rows.iter().filter_map(bookmark_view_tx).collect())
        })
    }

    /// 重命名书签(只改显示标签;locator/anchor 不变)。
    pub fn rename_bookmark(
        &self,
        document_id: &str,
        bookmark_id: &str,
        label: &str,
        note: Option<&str>,
    ) -> Result<BookmarkView, AppError> {
        const MAX_BOOKMARK_LABEL_CHARS: usize = 200;
        const MAX_BOOKMARK_NOTE_CHARS: usize = 1000;
        let label = label.trim();
        if document_id.trim().is_empty() || bookmark_id.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        if label.chars().count() > MAX_BOOKMARK_LABEL_CHARS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "bookmark_label_too_long".into())
                .with_detail("maxChars", (MAX_BOOKMARK_LABEL_CHARS as i64).into()));
        }
        if let Some(note) = note {
            if note.trim().chars().count() > MAX_BOOKMARK_NOTE_CHARS {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "bookmark_note_too_long".into())
                    .with_detail("maxChars", (MAX_BOOKMARK_NOTE_CHARS as i64).into()));
            }
        }
        let did = document_id.to_string();
        let bid = bookmark_id.to_string();
        let label = label.to_string();
        let note = note.map(|value| value.trim().to_string());
        let row = self.db.write(move |conn| {
            let updated = match &note {
                Some(note) => conn.execute(
                    "UPDATE bookmark SET label=?1, note=?2 WHERE id=?3 AND document_id=?4",
                    params![&label, note, &bid, &did],
                ),
                None => conn.execute(
                    "UPDATE bookmark SET label=?1 WHERE id=?2 AND document_id=?3",
                    params![&label, &bid, &did],
                ),
            }
            .map_err(|e| db_app_error(&e))?;
            if updated == 0 {
                return Err(err(CoreErrorCode::NotFound));
            }
            conn.query_row(
                "SELECT id, document_id, document_version_id, anchor_id, toggle_key, label, \
                 locator_json, created_at FROM bookmark WHERE id=?1",
                [&bid],
                |r| {
                    Ok(BookmarkRow {
                        id: r.get(0)?,
                        document_id: r.get(1)?,
                        document_version_id: r.get(2)?,
                        anchor_id: r.get(3)?,
                        toggle_key: r.get(4)?,
                        label: r.get(5)?,
                        note: r.get(6)?,
                        locator_json: r.get(7)?,
                        created_at: r.get(8)?,
                    })
                },
            )
            .map_err(|e| db_app_error(&e))
        })?;
        self.emit_userdata("bookmark", &row.id, "renamed", 1, document_id);
        bookmark_view_tx(&row).ok_or_else(|| err(CoreErrorCode::Internal))
    }

    // —— 笔记(《M06》§3–§5)——

    /// `note_save`:创建或更新;提交成功才算已保存。笔记与标签同一事务。
    pub fn save_note(
        &self,
        input: &NoteSaveInput,
        idempotency_key: &str,
    ) -> Result<NoteView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        if !input.origin.allowed_on_save() {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "origin_not_allowed_on_save".into()));
        }
        validate_note_body(&input.body)?;
        if input.tags.len() > MAX_NOTE_TAGS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "note_tag_limit".into())
                .with_detail("max", (MAX_NOTE_TAGS as i64).into()));
        }
        let mut normalized_tags: Vec<(String, String)> = Vec::new();
        for t in &input.tags {
            let (norm, display) = normalize_tag_name(t).ok_or_else(|| {
                err(CoreErrorCode::InvalidRequest).with_detail("reason", "tag_invalid".into())
            })?;
            if !normalized_tags.iter().any(|(n, _)| n == &norm) {
                normalized_tags.push((norm, display));
            }
        }

        let now = self.clock.now_ms();
        let note_id = self.ids.new_entity_id();
        let key = idempotency_key.to_string();
        let input2 = input.clone();
        let tags2 = normalized_tags;
        let view = self.db.write(move |conn| {
            // 幂等:同一保存请求重复送达 ⇒ 返回当前状态(先于一切校验)。
            if let Some(row) = load_note_by_key_tx(conn, &key)? {
                return note_view_tx(conn, &row);
            }
            if !document_exists_tx(conn, &input2.document_id)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            // 锚定笔记必须使用本文档的 Anchor(存在即用,状态派生展示)。
            let mut anchor_version: Option<String> = None;
            if let Some(aid) = &input2.anchor_id {
                let anchor =
                    anchor_summary_tx(conn, aid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
                if anchor.document_id != input2.document_id {
                    return Err(err(CoreErrorCode::NotFound));
                }
                anchor_version = Some(anchor.document_version_id);
            }
            match &input2.note_id {
                None => {
                    let version = anchor_version.or_else(|| {
                        current_version_id_tx(conn, &input2.document_id)
                            .ok()
                            .flatten()
                    });
                    conn.execute(
                        "INSERT INTO note(id, document_id, document_version_id, anchor_id, \
                         origin, body, status, source_refs_json, revision, idempotency_key, \
                         created_at, updated_at) \
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,?9,?10,?10)",
                        params![
                            note_id,
                            input2.document_id,
                            version,
                            input2.anchor_id,
                            input2.origin.as_str(),
                            input2.body,
                            input2.status.as_str(),
                            serde_json::to_string(&input2.source_refs)
                                .unwrap_or_else(|_| "[]".into()),
                            key,
                            now
                        ],
                    )
                    .map_err(|e| db_app_error(&e))?;
                }
                Some(nid) => {
                    let row =
                        load_note_tx(conn, nid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
                    if row.revision != input2.expected_revision {
                        return Err(err(CoreErrorCode::ConflictRevisionMismatch));
                    }
                    // 锚点创建后不可变(与标注一致);孤立锚点仍可查看原文快照。
                    if row.anchor_id.is_some() && input2.anchor_id != row.anchor_id {
                        return Err(err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "anchor_immutable".into()));
                    }
                    // origin 转移:ai_draft 经用户编辑 ⇒ user_edited_ai(仍显示 AI 来源);
                    // 用户笔记不得伪装为 AI 草稿。
                    let new_origin = match (row.origin, input2.origin) {
                        (NoteOrigin::User, NoteOrigin::User) => NoteOrigin::User,
                        (NoteOrigin::AiDraft, NoteOrigin::AiDraft) => NoteOrigin::AiDraft,
                        (NoteOrigin::AiDraft, NoteOrigin::User) => NoteOrigin::UserEditedAi,
                        (NoteOrigin::UserEditedAi, _) => NoteOrigin::UserEditedAi,
                        (_, NoteOrigin::UserEditedAi) => {
                            // 入口已拒绝 user_edited_ai 请求;防御重复校验。
                            return Err(err(CoreErrorCode::InvalidRequest)
                                .with_detail("reason", "origin_not_allowed_on_save".into()));
                        }
                        (NoteOrigin::User, NoteOrigin::AiDraft) => {
                            return Err(err(CoreErrorCode::InvalidRequest)
                                .with_detail("reason", "origin_transition_invalid".into()));
                        }
                    };
                    // 来源引用只增不减:编辑不抹掉创建时的来源层级。
                    let mut refs: Vec<SourceReference> =
                        serde_json::from_str(&row.source_refs_json).unwrap_or_default();
                    for r in &input2.source_refs {
                        if !refs
                            .iter()
                            .any(|x| x.source_type == r.source_type && x.source_id == r.source_id)
                        {
                            refs.push(r.clone());
                        }
                    }
                    conn.execute(
                        "UPDATE note SET body=?2, status=?3, origin=?4, source_refs_json=?5, \
                         revision=revision+1, updated_at=?6, idempotency_key=?7 \
                         WHERE id=?1 AND revision=?8",
                        params![
                            nid,
                            input2.body,
                            input2.status.as_str(),
                            new_origin.as_str(),
                            serde_json::to_string(&refs).unwrap_or_else(|_| "[]".into()),
                            now,
                            key,
                            input2.expected_revision
                        ],
                    )
                    .map_err(|e| db_app_error(&e))?;
                }
            }
            // 标签:upsert + 全量替换关联(同一事务)。
            let target_note = input2.note_id.clone().unwrap_or_else(|| note_id.clone());
            let mut tag_ids: Vec<String> = Vec::new();
            for (norm, display) in &tags2 {
                conn.execute(
                    "INSERT OR IGNORE INTO tag(id, document_id, normalized_name, display_name) \
                     VALUES (?1,?2,?3,?4)",
                    params![
                        crate::shared::new_trace_id(),
                        input2.document_id,
                        norm,
                        display
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
                let tag_id: String = conn
                    .query_row(
                        "SELECT id FROM tag WHERE document_id=?1 AND normalized_name=?2",
                        params![input2.document_id, norm],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                tag_ids.push(tag_id);
            }
            conn.execute(
                "DELETE FROM note_tag WHERE note_id=?1",
                params![target_note],
            )
            .map_err(|e| db_app_error(&e))?;
            for tid in &tag_ids {
                conn.execute(
                    "INSERT OR IGNORE INTO note_tag(note_id, tag_id) VALUES (?1,?2)",
                    params![target_note, tid],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            let row =
                load_note_tx(conn, &target_note)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            note_view_tx(conn, &row)
        })?;
        self.emit_userdata(
            "note",
            &view.id,
            if input.note_id.is_some() {
                "updated"
            } else {
                "created"
            },
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// `note_get`:含来源层级与锚点状态。
    pub fn get_note(&self, note_id: &str) -> Result<NoteView, AppError> {
        let nid = note_id.to_string();
        self.db.read(move |conn| {
            let row = load_note_tx(conn, &nid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            note_view_tx(conn, &row)
        })
    }

    /// `note_list`:按标签、来源、状态筛选;游标分页。
    pub fn list_notes(
        &self,
        document_id: &str,
        filter: &NoteListFilter,
        page: PageRequest,
    ) -> Result<NotePage, AppError> {
        let page = page.normalized();
        let did = document_id.to_string();
        let tag_norm = filter.tag.as_deref().map(|t| t.trim().to_lowercase());
        let status = filter.status.map(|s| s.as_str().to_string());
        let origin = filter.origin.map(|o| o.as_str().to_string());
        let anchor = filter.anchor_id.clone();
        let limit = page.limit;
        let cursor: Option<(i64, String)> = page
            .cursor
            .as_deref()
            .and_then(crate::shared::paging::decode_cursor);
        self.db.read(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            let mut sql = String::from(&format!(
                "SELECT {NOTE_COLS} FROM note WHERE document_id=?1"
            ));
            let mut binds: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::Text(did.clone())];
            if let Some(s) = &status {
                binds.push(rusqlite::types::Value::Text(s.clone()));
                sql.push_str(&format!(" AND status=?{}", binds.len()));
            }
            if let Some(o) = &origin {
                binds.push(rusqlite::types::Value::Text(o.clone()));
                sql.push_str(&format!(" AND origin=?{}", binds.len()));
            }
            if let Some(a) = &anchor {
                binds.push(rusqlite::types::Value::Text(a.clone()));
                sql.push_str(&format!(" AND anchor_id=?{}", binds.len()));
            }
            if let Some(t) = &tag_norm {
                binds.push(rusqlite::types::Value::Text(t.clone()));
                sql.push_str(&format!(
                    " AND id IN (SELECT n.id FROM note n JOIN note_tag nt ON nt.note_id=n.id \
                     JOIN tag tg ON tg.id=nt.tag_id WHERE n.document_id=?1 AND tg.normalized_name=?{})",
                    binds.len()
                ));
            }
            if let Some((ts, id)) = &cursor {
                binds.push(rusqlite::types::Value::Integer(*ts));
                binds.push(rusqlite::types::Value::Text(id.clone()));
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
            binds.push(rusqlite::types::Value::Integer(limit + 1));
            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                    NoteRow::try_from(r)
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            let mut items = Vec::new();
            for row in &rows {
                items.push(note_view_tx(conn, row)?);
            }
            let next_cursor = if (items.len() as i64) > limit {
                items.truncate(limit as usize);
                rows.get(limit as usize).map(|r| {
                    encode_cursor(&serde_json::json!({"ts": r.updated_at, "id": r.id}))
                })
            } else {
                None
            };
            Ok(NotePage {
                items,
                next_cursor,
                total: None,
            })
        })
    }

    /// `note_delete`:同一事务删除笔记、标签关联与其拥有的 Citation;
    /// 引用它的历史 Message/Artifact 保留 ID(由其读取方显示来源已删除)。
    pub fn delete_note(
        &self,
        note_id: &str,
        expected_revision: i64,
    ) -> Result<crate::shared::dto::DeleteResult, AppError> {
        let nid = note_id.to_string();
        let (deleted, document_id) = self.db.write(move |conn| {
            let row = load_note_tx(conn, &nid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.revision != expected_revision {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch));
            }
            conn.execute("DELETE FROM note_tag WHERE note_id=?1", params![nid])
                .map_err(|e| db_app_error(&e))?;
            delete_citations_for_owner_tx(conn, CitationOwnerType::Note, &nid)?;
            conn.execute("DELETE FROM note WHERE id=?1", params![nid])
                .map_err(|e| db_app_error(&e))?;
            Ok((true, row.document_id))
        })?;
        self.emit_userdata("note", note_id, "deleted", 0, &document_id);
        Ok(crate::shared::dto::DeleteResult {
            deleted,
            id: note_id.to_string(),
        })
    }

    // —— 待澄清(《M06》§6)——

    /// `clarification_create`:来源可为问题、回答、标注、笔记或错题;
    /// 创建时保存来源快照与可回跳引用。
    pub fn create_clarification(
        &self,
        input: &ClarificationCreateInput,
        idempotency_key: &str,
    ) -> Result<ClarificationView, AppError> {
        if input.question_text.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "clarification_empty".into()));
        }
        if input.question_text.chars().count() > MAX_CLARIFICATION_CHARS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "clarification_too_long".into()));
        }
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let key = idempotency_key.to_string();
        let input2 = input.clone();
        let view = self.db.write(move |conn| {
            if !document_exists_tx(conn, &input2.document_id)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            // 存量来源类型必须指向本文档内实体;快照保存可回跳引用。
            if let Some(st) = input2.source_type {
                if st.requires_existing_source() {
                    let sid = input2.source_id.as_deref().ok_or_else(|| {
                        err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "source_id_required".into())
                    })?;
                    let ok = match st {
                        ClarificationSourceType::Annotation => conn
                            .query_row(
                                "SELECT 1 FROM annotation WHERE id=?1 AND document_id=?2",
                                params![sid, input2.document_id],
                                |_| Ok(()),
                            )
                            .optional()
                            .is_ok_and(|x| x.is_some()),
                        ClarificationSourceType::Note => {
                            note_owner_exists_tx(conn, sid, &input2.document_id)?
                        }
                        _ => false,
                    };
                    if !ok {
                        return Err(err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "source_not_found".into()));
                    }
                }
            }
            if let Some(aid) = &input2.source_snapshot.anchor_id {
                let anchor =
                    anchor_summary_tx(conn, aid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
                if anchor.document_id != input2.document_id {
                    return Err(err(CoreErrorCode::NotFound));
                }
            }
            if let Some(row) = load_clarification_by_key_tx(conn, &key)? {
                return clarification_view_tx(conn, &row);
            }
            conn.execute(
                "INSERT INTO clarification_item(id, document_id, question_text, source_type, \
                 source_id, source_snapshot_json, status, revision, idempotency_key, \
                 created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,'pending',1,?7,?8,?8)",
                params![
                    id,
                    input2.document_id,
                    input2.question_text,
                    input2.source_type.map(|t| t.as_str()),
                    input2.source_id,
                    serde_json::to_string(&input2.source_snapshot).unwrap_or_else(|_| "{}".into()),
                    key,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            let row =
                load_clarification_tx(conn, &id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            clarification_view_tx(conn, &row)
        })?;
        self.emit_userdata(
            "clarification",
            &view.id,
            "created",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// `clarification_update`:状态与解决结果;resolved 必须关联同文档对象,
    /// 解决状态不自动修改笔记状态(用户显式关联)。
    pub fn update_clarification(
        &self,
        input: &ClarificationUpdateInput,
    ) -> Result<ClarificationView, AppError> {
        let now = self.clock.now_ms();
        let cid = input.clarification_id.clone();
        let status = input.status.as_str().to_string();
        let input_status = input.status;
        let input_ref = input.resolution_ref.clone();
        let expected = input.expected_revision;
        let view = self.db.write(move |conn| {
            let row =
                load_clarification_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.revision != expected {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch));
            }
            // resolved 必须携带解决结果引用;note 引用需同文档;
            // 离开 resolved 时清空解决结果与时间。
            let resolution_json = match input_status {
                ClarificationStatus::Resolved => {
                    let r = input_ref.as_ref().ok_or_else(|| {
                        err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "resolution_ref_required".into())
                    })?;
                    if r.ref_type == ResolutionRefType::Note {
                        let same_doc = note_owner_exists_tx(conn, &r.ref_id, &row.document_id)?;
                        if !same_doc {
                            return Err(err(CoreErrorCode::InvalidRequest)
                                .with_detail("reason", "resolution_cross_document".into()));
                        }
                    }
                    Some(serde_json::to_string(r).unwrap_or_default())
                }
                ClarificationStatus::Pending | ClarificationStatus::Deferred => None,
            };
            let resolved_at = (input_status == ClarificationStatus::Resolved).then_some(now);
            conn.execute(
                "UPDATE clarification_item SET status=?2, resolution_ref_json=?3, resolved_at=?4, \
                 revision=revision+1, updated_at=?5 WHERE id=?1 AND revision=?6",
                params![cid, status, resolution_json, resolved_at, now, expected],
            )
            .map_err(|e| db_app_error(&e))?;
            let updated =
                load_clarification_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            clarification_view_tx(conn, &updated)
        })?;
        self.emit_userdata(
            "clarification",
            &view.id,
            "updated",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// `clarification_set_remark`:只改备注,不动问题、来源、状态与解决引用。
    pub fn set_clarification_remark(
        &self,
        input: &ClarificationRemarkInput,
    ) -> Result<ClarificationView, AppError> {
        if input.remark.chars().count() > MAX_CLARIFICATION_CHARS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "clarification_remark_too_long".into()));
        }
        let now = self.clock.now_ms();
        let cid = input.clarification_id.clone();
        let remark = input.remark.clone();
        let expected = input.expected_revision;
        let view = self.db.write(move |conn| {
            let row =
                load_clarification_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.revision != expected {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch));
            }
            conn.execute(
                "UPDATE clarification_item SET remark=?2, revision=revision+1, updated_at=?3 \
                 WHERE id=?1 AND revision=?4",
                params![cid, remark, now, expected],
            )
            .map_err(|e| db_app_error(&e))?;
            let updated =
                load_clarification_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            clarification_view_tx(conn, &updated)
        })?;
        self.emit_userdata(
            "clarification",
            &view.id,
            "updated",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// `clarification_delete`:明确删除;同事务清理其拥有的 Citation。
    pub fn delete_clarification(
        &self,
        clarification_id: &str,
        expected_revision: i64,
    ) -> Result<crate::shared::dto::DeleteResult, AppError> {
        let cid = clarification_id.to_string();
        let (deleted, document_id) = self.db.write(move |conn| {
            let row =
                load_clarification_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.revision != expected_revision {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch));
            }
            delete_citations_for_owner_tx(conn, CitationOwnerType::Clarification, &cid)?;
            conn.execute("DELETE FROM clarification_item WHERE id=?1", params![cid])
                .map_err(|e| db_app_error(&e))?;
            Ok((true, row.document_id))
        })?;
        self.emit_userdata(
            "clarification",
            clarification_id,
            "deleted",
            0,
            &document_id,
        );
        Ok(crate::shared::dto::DeleteResult {
            deleted,
            id: clarification_id.to_string(),
        })
    }

    /// `clarification_list`:按状态筛选,游标分页。
    pub fn list_clarifications(
        &self,
        document_id: &str,
        status: Option<ClarificationStatus>,
        source_type: Option<ClarificationSourceType>,
        page: PageRequest,
    ) -> Result<ClarificationPage, AppError> {
        let page = page.normalized();
        let did = document_id.to_string();
        let st = status.map(|s| s.as_str().to_string());
        let srct = source_type.map(|s| s.as_str().to_string());
        let limit = page.limit;
        let cursor: Option<(i64, String)> = page
            .cursor
            .as_deref()
            .and_then(crate::shared::paging::decode_cursor);
        self.db.read(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            let mut sql = String::from(&format!(
                "SELECT {CLARIFICATION_COLS} FROM clarification_item WHERE document_id=?1"
            ));
            let mut binds: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::Text(did.clone())];
            if let Some(s) = &st {
                binds.push(rusqlite::types::Value::Text(s.clone()));
                sql.push_str(&format!(" AND status=?{}", binds.len()));
            }
            if let Some(s) = &srct {
                binds.push(rusqlite::types::Value::Text(s.clone()));
                sql.push_str(&format!(" AND source_type=?{}", binds.len()));
            }
            if let Some((ts, id)) = &cursor {
                binds.push(rusqlite::types::Value::Integer(*ts));
                binds.push(rusqlite::types::Value::Text(id.clone()));
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
            binds.push(rusqlite::types::Value::Integer(limit + 1));
            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                    ClarificationRow::try_from(r)
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            let mut items = Vec::new();
            for row in &rows {
                items.push(clarification_view_tx(conn, row)?);
            }
            let next_cursor = if (items.len() as i64) > limit {
                items.truncate(limit as usize);
                rows.get(limit as usize)
                    .map(|r| encode_cursor(&serde_json::json!({"ts": r.updated_at, "id": r.id})))
            } else {
                None
            };
            Ok(ClarificationPage {
                items,
                next_cursor,
                total: None,
            })
        })
    }

    // —— 数据清理(《M06》§8)——

    /// 预览:各类别数量以及会影响的引用(M08 DataClearContributor 输入)。
    pub fn preview_clear(
        &self,
        document_id: &str,
        categories: &[UserdataClearCategory],
    ) -> Result<UserdataClearPreview, AppError> {
        let did = document_id.to_string();
        let cats: Vec<String> = categories.iter().map(|c| c.as_str().to_string()).collect();
        self.db.read(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            let mut out = Vec::new();
            for c in &cats {
                let category = UserdataClearCategory::parse_db(c)
                    .ok_or_else(|| err(CoreErrorCode::InvalidRequest))?;
                let (count, affected) = match category {
                    UserdataClearCategory::Annotations => (
                        count_where(conn, "annotation", &did)?,
                        0, // 标注不是 Citation 属主
                    ),
                    UserdataClearCategory::Bookmarks => (count_where(conn, "bookmark", &did)?, 0),
                    UserdataClearCategory::Notes => {
                        let n = count_where(conn, "note", &did)?;
                        let c = count_citations_for_owner_type(conn, "note", &did)?;
                        (n, c)
                    }
                    UserdataClearCategory::Clarifications => {
                        let n = count_where(conn, "clarification_item", &did)?;
                        let c = count_citations_for_owner_type(conn, "clarification", &did)?;
                        (n, c)
                    }
                    UserdataClearCategory::Tags => {
                        let n = count_where(conn, "tag", &did)?;
                        (n, 0)
                    }
                };
                out.push(ClearCategoryCount {
                    category,
                    count,
                    affected_citations: affected,
                });
            }
            Ok(UserdataClearPreview {
                document_id: did,
                categories: out,
            })
        })
    }

    /// 执行:一个事务删除所选类别(标签只解除关联);不删除原文件、
    /// 其他文档或包含其文字的产物。M08 负责二次确认与计划校验。
    pub fn execute_clear(
        &self,
        document_id: &str,
        categories: &[UserdataClearCategory],
    ) -> Result<UserdataClearResult, AppError> {
        let did = document_id.to_string();
        let cats: Vec<String> = categories.iter().map(|c| c.as_str().to_string()).collect();
        let result = self.db.write(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(err(CoreErrorCode::DocNotFound));
            }
            let mut deleted = Vec::new();
            for c in &cats {
                let category = UserdataClearCategory::parse_db(c)
                    .ok_or_else(|| err(CoreErrorCode::InvalidRequest))?;
                let count = match category {
                    UserdataClearCategory::Annotations => {
                        conn.execute("DELETE FROM annotation WHERE document_id=?1", params![did])
                            .map_err(|e| db_app_error(&e))? as i64
                    }
                    UserdataClearCategory::Bookmarks => {
                        conn.execute("DELETE FROM bookmark WHERE document_id=?1", params![did])
                            .map_err(|e| db_app_error(&e))? as i64
                    }
                    UserdataClearCategory::Notes => {
                        // 先收集属主,逐条级联 Citation,再按 FK 顺序删除关联与笔记。
                        let ids: Vec<String> = {
                            let mut stmt = conn
                                .prepare("SELECT id FROM note WHERE document_id=?1")
                                .map_err(|e| db_app_error(&e))?;
                            let out = stmt
                                .query_map([&did], |r| r.get(0))
                                .map_err(|e| db_app_error(&e))?
                                .collect::<Result<_, _>>()
                                .map_err(|e| db_app_error(&e))?;
                            out
                        };
                        for nid in &ids {
                            let _ =
                                delete_citations_for_owner_tx(conn, CitationOwnerType::Note, nid)?;
                        }
                        conn.execute(
                            "DELETE FROM note_tag WHERE note_id IN \
                             (SELECT id FROM note WHERE document_id=?1)",
                            params![did],
                        )
                        .map_err(|e| db_app_error(&e))?;
                        conn.execute("DELETE FROM note WHERE document_id=?1", params![did])
                            .map_err(|e| db_app_error(&e))? as i64
                    }
                    UserdataClearCategory::Clarifications => {
                        let ids: Vec<String> = {
                            let mut stmt = conn
                                .prepare("SELECT id FROM clarification_item WHERE document_id=?1")
                                .map_err(|e| db_app_error(&e))?;
                            let out = stmt
                                .query_map([&did], |r| r.get(0))
                                .map_err(|e| db_app_error(&e))?
                                .collect::<Result<_, _>>()
                                .map_err(|e| db_app_error(&e))?;
                            out
                        };
                        for cid in &ids {
                            let _ = delete_citations_for_owner_tx(
                                conn,
                                CitationOwnerType::Clarification,
                                cid,
                            )?;
                        }
                        conn.execute(
                            "DELETE FROM clarification_item WHERE document_id=?1",
                            params![did],
                        )
                        .map_err(|e| db_app_error(&e))? as i64
                    }
                    UserdataClearCategory::Tags => {
                        // 标签删除只解除 NoteTag 关联,不删除笔记。
                        conn.execute(
                            "DELETE FROM note_tag WHERE tag_id IN \
                             (SELECT id FROM tag WHERE document_id=?1)",
                            params![did],
                        )
                        .map_err(|e| db_app_error(&e))?;
                        conn.execute("DELETE FROM tag WHERE document_id=?1", params![did])
                            .map_err(|e| db_app_error(&e))? as i64
                    }
                };
                deleted.push(ClearCategoryCount {
                    category,
                    count,
                    affected_citations: 0,
                });
            }
            Ok(UserdataClearResult {
                document_id: did.clone(),
                deleted,
            })
        })?;
        self.emit_userdata("userdata", document_id, "cleared", 0, document_id);
        Ok(result)
    }
}

fn load_annotation_by_key_tx(
    conn: &Connection,
    key: &str,
) -> Result<Option<AnnotationRow>, AppError> {
    conn.query_row(
        "SELECT id, document_id, anchor_id, color, note_text, remark, revision, created_at, \
         updated_at FROM annotation WHERE idempotency_key=?1",
        [key],
        |r| {
            Ok(AnnotationRow {
                id: r.get(0)?,
                document_id: r.get(1)?,
                anchor_id: r.get(2)?,
                color: AnnotationColor::parse_db(&r.get::<_, String>(3)?)
                    .unwrap_or(AnnotationColor::Yellow),
                note_text: r.get(4)?,
                remark: r.get(5)?,
                revision: r.get(6)?,
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_clarification_by_key_tx(
    conn: &Connection,
    key: &str,
) -> Result<Option<ClarificationRow>, AppError> {
    conn.query_row(
        &format!("SELECT {CLARIFICATION_COLS} FROM clarification_item WHERE idempotency_key=?1"),
        [key],
        |r| ClarificationRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

/// 书签幂等身份行:(id, 版本, 锚点, 标签, 备注, 创建时间)。
type BookmarkExisting = (String, Option<String>, Option<String>, String, String, i64);

fn count_where(conn: &Connection, table: &str, document_id: &str) -> Result<i64, AppError> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE document_id=?1"),
        [document_id],
        |r| r.get(0),
    )
    .map_err(|e| db_app_error(&e))
}

fn count_citations_for_owner_type(
    conn: &Connection,
    owner_type: &str,
    document_id: &str,
) -> Result<i64, AppError> {
    conn.query_row(
        "SELECT COUNT(*) FROM citation WHERE owner_type=?1 AND document_id=?2",
        params![owner_type, document_id],
        |r| r.get(0),
    )
    .map_err(|e| db_app_error(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_body_policy_rejects_oversize_and_unsafe() {
        assert!(validate_note_body("正常笔记").is_ok());
        let long = "字".repeat(MAX_NOTE_BODY_CHARS + 1);
        let e = validate_note_body(&long).unwrap_err();
        assert_eq!(e.code, "INVALID_REQUEST");
        assert!(validate_note_body("<SCRIPT>alert(1)</script>").is_err());
        assert!(validate_note_body("链接 [x](javascript:alert(1))").is_err());
        assert!(validate_note_body("data:text/html,hi").is_err());
    }

    #[test]
    fn tag_normalization_collapses_and_lowercases() {
        let (norm, display) = normalize_tag_name("  Key  Concept ").unwrap();
        assert_eq!(norm, "key concept");
        assert_eq!(display, "Key Concept");
        assert!(normalize_tag_name("   ").is_none());
        let too_long = "a".repeat(MAX_TAG_CHARS + 1);
        assert!(normalize_tag_name(&too_long).is_none());
    }

    #[test]
    fn origin_transitions_keep_ai_provenance() {
        // allowed_on_save:user/ai_draft;user_edited_ai 只能由编辑产生。
        assert!(NoteOrigin::User.allowed_on_save());
        assert!(NoteOrigin::AiDraft.allowed_on_save());
        assert!(!NoteOrigin::UserEditedAi.allowed_on_save());
    }

    // —— 备注与内容分离(M06 v3) ——

    use crate::shared::clock::TestClock;
    use crate::shared::events::NoopEventSink;
    use crate::shared::ids::TestIdGenerator;
    use crate::shared::locator::PageRotation;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kw-notes-{tag}-{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_service(tag: &str) -> (std::path::PathBuf, NotesService) {
        let dir = temp_dir(tag);
        let db_path = dir.join("test.sqlite3");
        let mut conn = crate::persistence::connection::open_write_connection(&db_path).unwrap();
        let runner = crate::persistence::MigrationRunner::new(dir.join("backups"));
        runner
            .run(
                &mut conn,
                &crate::app_shell::all_migrations(),
                &db_path,
                1_000,
            )
            .unwrap();
        conn.execute(
            "INSERT INTO document(id, display_name, format, current_version_id, created_at,              updated_at) VALUES('doc-1','测试文档','pdf',NULL,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO document_version(id, document_id, fingerprint_status, confirmed,              created_at) VALUES('ver-1','doc-1','ready',1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE document SET current_version_id='ver-1' WHERE id='doc-1'",
            [],
        )
        .unwrap();
        let locator =
            serde_json::to_string(&SourceLocator::pdf(1, vec![], PageRotation::R0)).unwrap();
        conn.execute(
            "INSERT INTO anchor(id, document_id, document_version_id, kind, status, exact_text,              prefix_text, suffix_text, snapshot_text, text_hash, primary_locator_json,              idempotency_key, created_at)              VALUES('anchor-1','doc-1','ver-1','page','active','原文','','','原文','hash-1',              ?1,'anchor-key-1',1)",
            params![locator],
        )
        .unwrap();
        drop(conn);
        let db = std::sync::Arc::new(Db::open(&db_path, 1).unwrap());
        let service = NotesService::new(
            db,
            std::sync::Arc::new(TestClock::new(1_000)),
            std::sync::Arc::new(TestIdGenerator::new()),
            std::sync::Arc::new(NoopEventSink),
        );
        (dir, service)
    }

    const INK_PAYLOAD: &str =
        "ink:v1:[{\"c\":\"#f04f9e\",\"w\":0.0035,\"p\":[[0.1,0.1],[0.2,0.2]]}]";

    #[test]
    fn annotation_remark_update_preserves_note_text_payload() {
        let (dir, service) = test_service("remark-ink");
        let created = service
            .create_annotation(
                &AnnotationCreateInput {
                    document_id: "doc-1".into(),
                    anchor_id: "anchor-1".into(),
                    color: AnnotationColor::Pink,
                    note_text: INK_PAYLOAD.into(),
                    remark: None,
                },
                "ann-key-1",
            )
            .unwrap();
        assert_eq!(created.remark, "");
        assert_eq!(created.note_text, INK_PAYLOAD);

        let updated = service
            .update_annotation(&AnnotationUpdateInput {
                annotation_id: created.id.clone(),
                color: AnnotationColor::Pink,
                note_text: INK_PAYLOAD.into(),
                remark: Some("这是一条可读备注".into()),
                expected_revision: created.revision,
            })
            .unwrap();
        assert_eq!(updated.note_text, INK_PAYLOAD, "笔迹载荷必须原样保留");
        assert_eq!(updated.remark, "这是一条可读备注");
        assert_eq!(updated.revision, created.revision + 1);

        // 只改颜色(不传 remark)时备注保持不变。
        let recolored = service
            .update_annotation(&AnnotationUpdateInput {
                annotation_id: created.id.clone(),
                color: AnnotationColor::Blue,
                note_text: INK_PAYLOAD.into(),
                remark: None,
                expected_revision: updated.revision,
            })
            .unwrap();
        assert_eq!(recolored.remark, "这是一条可读备注");
        assert_eq!(recolored.note_text, INK_PAYLOAD);

        let listed = service.list_annotations("doc-1").unwrap();
        let row = listed.iter().find(|a| a.id == created.id).unwrap();
        assert_eq!(row.remark, "这是一条可读备注");
        assert_eq!(row.note_text, INK_PAYLOAD);

        let stale = service.update_annotation(&AnnotationUpdateInput {
            annotation_id: created.id.clone(),
            color: AnnotationColor::Blue,
            note_text: INK_PAYLOAD.into(),
            remark: Some("过期修订".into()),
            expected_revision: created.revision,
        });
        assert_eq!(stale.unwrap_err().code, "CONFLICT_REVISION_MISMATCH");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clarification_remark_keeps_question_status_and_checks_revision() {
        let (dir, service) = test_service("remark-clarification");
        let created = service
            .create_clarification(
                &ClarificationCreateInput {
                    document_id: "doc-1".into(),
                    question_text: "这里为什么成立?".into(),
                    source_type: None,
                    source_id: None,
                    source_snapshot: Default::default(),
                },
                "clarify-key-1",
            )
            .unwrap();
        assert_eq!(created.remark, "");
        let updated = service
            .set_clarification_remark(&ClarificationRemarkInput {
                clarification_id: created.id.clone(),
                remark: "需要回看第 2 节".into(),
                expected_revision: created.revision,
            })
            .unwrap();
        assert_eq!(updated.remark, "需要回看第 2 节");
        assert_eq!(updated.question_text, "这里为什么成立?");
        assert_eq!(updated.status, ClarificationStatus::Pending);
        assert_eq!(updated.revision, created.revision + 1);

        let stale = service.set_clarification_remark(&ClarificationRemarkInput {
            clarification_id: created.id,
            remark: "过期".into(),
            expected_revision: created.revision,
        });
        assert_eq!(stale.unwrap_err().code, "CONFLICT_REVISION_MISMATCH");
        let _ = std::fs::remove_dir_all(dir);
    }
}
