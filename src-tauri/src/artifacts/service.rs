//! M10 产物服务(《M10》§2–§10)。
//!
//! 关键不变量:
//! - AI 首次生成创建 `ai_draft` 不可变版本并设为当前;数据库触发器与服务
//!   双重拒绝任何 ai_draft/sealed 更新(ARTIFACT_VERSION_IMMUTABLE)。
//! - 首次用户编辑从 AI/封存版本复制创建 `user_edit` 可变版本;自动保存只
//!   更新当前 user_edit,revision +1;封版后不可变,再编辑新建版本。
//! - 重新生成/恢复总是创建新版本;重新生成不切换用户已编辑的当前版本,
//!   除非用户明确选择(adopt_draft)。
//! - Schema 校验在写入前执行(注册 Codec);summary.v1 的 document_fact
//!   段必须携带 Citation,移除后必须降级为非事实段。
//! - 删除走预览 + 事务级联(版本/引用/导出记录),不触碰原文、笔记或
//!   其他产物;Task 与 ArtifactVersion 的 UNIQUE(task_id) 防止重复生成。
//! - 事件(artifact.changed)只在 COMMIT 成功后发出。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rusqlite::{params, Connection, OptionalExtension};

use crate::evidence::models::ClaimKind;
use crate::persistence::{db_app_error, Db};
use crate::shared::dto::DeleteResult;
use crate::shared::events::{DomainEvent, DomainEventType, EventSink};
use crate::shared::locator::SourceLocator;
use crate::shared::paging::{encode_cursor, Page, PageRequest};
use crate::shared::{AppError, Clock, CoreErrorCode, IdGenerator};

use super::models::*;

/// 产物标题上限。
pub const MAX_ARTIFACT_TITLE_CHARS: usize = 200;
/// 单个产物版本上限(防御性,防无限编辑派生)。
pub const MAX_VERSIONS_PER_ARTIFACT: i64 = 500;

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

/// 触发器 ABORT 转换:UPDATE artifact_version 命中不可变触发器时,
/// SQLITE_CONSTRAINT_TRIGGER 转为 ARTIFACT_VERSION_IMMUTABLE(其余仍走
/// db_app_error)。服务层在 UPDATE 前已做 origin/state 校验,此处是
/// 数据库层的第二重防线。
pub fn db_write_app_error(e: &rusqlite::Error) -> AppError {
    if let rusqlite::Error::SqliteFailure(se, msg) = e {
        if se.code == rusqlite::ErrorCode::ConstraintViolation
            && msg
                .as_deref()
                .is_some_and(|m| m.contains("artifact version is immutable"))
        {
            return err(CoreErrorCode::ArtifactVersionImmutable);
        }
    }
    db_app_error(e)
}

// —— 行加载 ——

struct ArtifactRow {
    id: String,
    document_id: String,
    kind: String,
    title: String,
    current_version_id: Option<String>,
    revision: i64,
    created_at: i64,
    updated_at: i64,
    #[allow(dead_code)] // 列读取保持完整;写入走显式列清单
    idempotency_key: String,
}

struct VersionRow {
    id: String,
    artifact_id: String,
    parent_version_id: Option<String>,
    version_no: i64,
    origin: ArtifactVersionOrigin,
    state: ArtifactVersionState,
    schema_version: String,
    content_json: String,
    snapshot_id: Option<String>,
    task_id: Option<String>,
    coverage_json: String,
    revision: i64,
    created_at: i64,
    updated_at: i64,
    sealed_at: Option<i64>,
}

const ARTIFACT_COLS: &str = "id, document_id, kind, title, current_version_id, revision, \
     created_at, updated_at, idempotency_key";

fn load_artifact_tx(conn: &Connection, id: &str) -> Result<Option<ArtifactRow>, AppError> {
    conn.query_row(
        &format!("SELECT {ARTIFACT_COLS} FROM artifact WHERE id=?1"),
        [id],
        |r| {
            Ok(ArtifactRow {
                id: r.get(0)?,
                document_id: r.get(1)?,
                kind: r.get(2)?,
                title: r.get(3)?,
                current_version_id: r.get(4)?,
                revision: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
                idempotency_key: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_artifact_by_key_tx(conn: &Connection, key: &str) -> Result<Option<ArtifactRow>, AppError> {
    conn.query_row(
        &format!("SELECT {ARTIFACT_COLS} FROM artifact WHERE idempotency_key=?1"),
        [key],
        |r| {
            Ok(ArtifactRow {
                id: r.get(0)?,
                document_id: r.get(1)?,
                kind: r.get(2)?,
                title: r.get(3)?,
                current_version_id: r.get(4)?,
                revision: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
                idempotency_key: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_version_tx(conn: &Connection, id: &str) -> Result<Option<VersionRow>, AppError> {
    conn.query_row(
        "SELECT id, artifact_id, parent_version_id, version_no, origin, state, schema_version, \
         content_json, snapshot_id, task_id, coverage_json, revision, created_at, updated_at, \
         sealed_at FROM artifact_version WHERE id=?1",
        [id],
        parse_version_row,
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn parse_version_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<VersionRow> {
    let origin = ArtifactVersionOrigin::parse_db(&r.get::<_, String>(4)?).ok_or(
        rusqlite::Error::InvalidColumnType(4, "origin".into(), rusqlite::types::Type::Text),
    )?;
    let state = ArtifactVersionState::parse_db(&r.get::<_, String>(5)?).ok_or(
        rusqlite::Error::InvalidColumnType(5, "state".into(), rusqlite::types::Type::Text),
    )?;
    Ok(VersionRow {
        id: r.get(0)?,
        artifact_id: r.get(1)?,
        parent_version_id: r.get(2)?,
        version_no: r.get(3)?,
        origin,
        state,
        schema_version: r.get(6)?,
        content_json: r.get(7)?,
        snapshot_id: r.get(8)?,
        task_id: r.get(9)?,
        coverage_json: r.get(10)?,
        revision: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
        sealed_at: r.get(14)?,
    })
}

fn document_exists_tx(conn: &Connection, id: &str) -> Result<bool, AppError> {
    let exists: Option<i64> = conn
        .query_row("SELECT 1 FROM document WHERE id=?1", [id], |r| r.get(0))
        .optional()
        .map_err(|e| db_app_error(&e))?;
    Ok(exists.is_some())
}

fn citation_count_tx(conn: &Connection, version_id: &str) -> Result<i64, AppError> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM citation WHERE owner_type='artifact_version' AND owner_id=?1",
            [version_id],
            |r| r.get(0),
        )
        .map_err(|e| db_app_error(&e))?;
    Ok(n)
}

fn find_version_by_task_tx(
    conn: &Connection,
    task_id: &str,
) -> Result<Option<VersionRow>, AppError> {
    conn.query_row(
        "SELECT id FROM artifact_version WHERE task_id=?1",
        [task_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| db_app_error(&e))?
    .map(|id| load_version_tx(conn, &id))
    .transpose()
    .map(Option::flatten)
}

fn next_version_no_tx(conn: &Connection, artifact_id: &str) -> Result<i64, AppError> {
    // COALESCE:空表时 MAX 为 NULL,直接取 0 起步。
    let n: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version_no), 0) FROM artifact_version WHERE artifact_id=?1",
            [artifact_id],
            |r| r.get(0),
        )
        .map_err(|e| db_app_error(&e))?;
    Ok(n + 1)
}

fn version_view(row: &VersionRow) -> Result<ArtifactVersionView, AppError> {
    let content: serde_json::Value = serde_json::from_str(&row.content_json)
        .map_err(|_| err(CoreErrorCode::Internal).with_detail("reason", "content_json".into()))?;
    let coverage: CoverageReport =
        serde_json::from_str(&row.coverage_json).unwrap_or(CoverageReport {
            complete: false,
            regions: vec![],
        });
    Ok(ArtifactVersionView {
        id: row.id.clone(),
        artifact_id: row.artifact_id.clone(),
        parent_version_id: row.parent_version_id.clone(),
        version_no: row.version_no,
        origin: row.origin,
        state: row.state,
        schema_version: row.schema_version.clone(),
        content,
        snapshot_id: row.snapshot_id.clone(),
        task_id: row.task_id.clone(),
        coverage,
        revision: row.revision,
        created_at: row.created_at,
        updated_at: row.updated_at,
        sealed_at: row.sealed_at,
    })
}

fn artifact_view_tx(
    conn: &Connection,
    row: &ArtifactRow,
    with_versions: bool,
) -> Result<ArtifactView, AppError> {
    let current = match &row.current_version_id {
        Some(vid) => match load_version_tx(conn, vid)? {
            Some(v) => Some(Box::new(version_view(&v)?)),
            None => None,
        },
        None => None,
    };
    let citation_count = row
        .current_version_id
        .as_deref()
        .map(|vid| citation_count_tx(conn, vid))
        .transpose()?
        .unwrap_or(0);
    let versions = if with_versions {
        let mut stmt = conn
            .prepare(
                "SELECT id, artifact_id, parent_version_id, version_no, origin, state, \
                 schema_version, content_json, snapshot_id, task_id, coverage_json, revision, \
                 created_at, updated_at, sealed_at \
                 FROM artifact_version WHERE artifact_id=?1 ORDER BY version_no",
            )
            .map_err(|e| db_app_error(&e))?;
        let rows = stmt
            .query_map([&row.id], parse_version_row)
            .map_err(|e| db_app_error(&e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| db_app_error(&e))?;
        let mut out = Vec::with_capacity(rows.len());
        for v in &rows {
            out.push(version_view(v)?);
        }
        out
    } else {
        vec![]
    };
    Ok(ArtifactView {
        id: row.id.clone(),
        document_id: row.document_id.clone(),
        kind: row.kind.clone(),
        title: row.title.clone(),
        current_version_id: row.current_version_id.clone(),
        current_version: current,
        versions,
        citation_count,
        revision: row.revision,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// 插入版本(创建 ai_draft/user_edit/copy/restore 统一入口)。
#[allow(clippy::too_many_arguments)]
fn insert_version_tx(
    conn: &Connection,
    id: &str,
    artifact: &ArtifactRow,
    parent_version_id: Option<&str>,
    origin: ArtifactVersionOrigin,
    schema_version: &str,
    content_json: &str,
    snapshot_id: Option<&str>,
    task_id: Option<&str>,
    coverage_json: &str,
    now: i64,
) -> Result<VersionRow, AppError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM artifact_version WHERE artifact_id=?1",
            [&artifact.id],
            |r| r.get(0),
        )
        .map_err(|e| db_app_error(&e))?;
    if count >= MAX_VERSIONS_PER_ARTIFACT {
        return Err(err(CoreErrorCode::InvalidRequest)
            .with_detail("reason", "artifact_version_limit".into()));
    }
    let version_no = next_version_no_tx(conn, &artifact.id)?;
    conn.execute(
        "INSERT INTO artifact_version(id, artifact_id, parent_version_id, version_no, origin, \
         state, schema_version, content_json, snapshot_id, task_id, coverage_json, revision, \
         created_at, updated_at, sealed_at) \
         VALUES (?1,?2,?3,?4,?5,'mutable',?6,?7,?8,?9,?10,1,?11,?11,NULL)",
        params![
            id,
            artifact.id,
            parent_version_id,
            version_no,
            origin.as_str(),
            schema_version,
            content_json,
            snapshot_id,
            task_id,
            coverage_json,
            now
        ],
    )
    .map_err(|e| db_app_error(&e))?;
    load_version_tx(conn, id)?.ok_or_else(|| err(CoreErrorCode::Internal))
}

// —— 服务 ——

pub struct ArtifactService {
    pub(crate) db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    events: Arc<dyn EventSink>,
    codecs: RwLock<HashMap<String, Arc<dyn ArtifactCodec>>>,
}

impl ArtifactService {
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
            codecs: RwLock::new(HashMap::new()),
        }
    }

    /// 注册 Codec(M13 组装根执行;M11/M12 各自注册)。
    pub fn register_codec(&self, codec: Arc<dyn ArtifactCodec>) {
        self.codecs
            .write()
            .unwrap()
            .insert(codec.kind().to_string(), codec);
    }

    fn codec_for(&self, kind: &str) -> Result<Arc<dyn ArtifactCodec>, AppError> {
        self.codecs
            .read()
            .unwrap()
            .get(kind)
            .cloned()
            .ok_or_else(|| {
                err(CoreErrorCode::ArtifactSchemaInvalid)
                    .with_detail("reason", "codec_not_registered".into())
                    .with_detail("kind", kind.into())
            })
    }

    /// 校验内容并返回规范化 JSON(写入前必经)。
    fn validate_content(
        &self,
        kind: &str,
        content: &serde_json::Value,
    ) -> Result<String, AppError> {
        let codec = self.codec_for(kind)?;
        if !codec.validate(content) {
            return Err(err(CoreErrorCode::ArtifactSchemaInvalid)
                .with_detail("reason", "schema_validation_failed".into())
                .with_detail("kind", kind.into()));
        }
        serde_json::to_string(content).map_err(|_| err(CoreErrorCode::ArtifactSchemaInvalid))
    }

    fn emit_artifact(
        &self,
        artifact_id: &str,
        version_id: Option<&str>,
        action: &str,
        revision: i64,
        document_id: &str,
    ) {
        let event = DomainEvent::new(
            DomainEventType::ArtifactChanged,
            artifact_id.to_string(),
            revision,
            self.clock.now_ms(),
            serde_json::json!({
                "artifactId": artifact_id,
                "versionId": version_id,
                "action": action,
                "revision": revision,
            }),
        )
        .with_document(Some(document_id.to_string()), None);
        self.events.emit(event);
    }

    // —— AI 首次生成(《M10》§3:创建 ai_draft 不可变版本,设为当前)——

    /// `artifact_generate` 的落库入口(M09 编排完成后调用;也可由
    /// 集成测试直接驱动)。同一 task_id 幂等:UNIQUE(task_id) 冲突时
    /// 返回已存在的版本,不重复创建产物。
    pub fn commit_ai_draft(
        &self,
        input: &AiDraftCommitInput,
        idempotency_key: &str,
    ) -> Result<ArtifactView, AppError> {
        if input.title.chars().count() > MAX_ARTIFACT_TITLE_CHARS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "artifact_title_too_long".into()));
        }
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let content_json = self.validate_content(&input.kind, &input.content)?;
        let coverage_json = serde_json::to_string(&input.coverage)
            .map_err(|_| err(CoreErrorCode::InvalidRequest))?;
        let now = self.clock.now_ms();
        let artifact_id = self.ids.new_entity_id();
        let version_id = self.ids.new_entity_id();
        let key = idempotency_key.to_string();
        // 校验已在事务外完成;事务内只需 kind 已注册这一事实。
        let kind_registered = self.codecs.read().unwrap().contains_key(&input.kind);
        let input = AiDraftCommitInput {
            document_id: input.document_id.clone(),
            kind: input.kind.clone(),
            title: input.title.clone(),
            content: input.content.clone(),
            coverage: input.coverage.clone(),
            snapshot_id: input.snapshot_id.clone(),
            task_id: input.task_id.clone(),
            citations: input.citations.clone(),
        };
        let view =
            self.db.write(move |conn| {
                // 幂等:同键产物已存在 ⇒ 复用。
                if let Some(row) = load_artifact_by_key_tx(conn, &key)? {
                    return artifact_view_tx(conn, &row, true);
                }
                // Task 幂等:同 Task 已产出版本 ⇒ 返回该产物(不重复创建)。
                if let Some(task_id) = &input.task_id {
                    let existing: Option<String> = conn
                        .query_row(
                            "SELECT artifact_id FROM artifact_version WHERE task_id=?1",
                            [task_id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(|e| db_app_error(&e))?;
                    if let Some(aid) = existing {
                        let row = load_artifact_tx(conn, &aid)?
                            .ok_or_else(|| err(CoreErrorCode::Internal))?;
                        return artifact_view_tx(conn, &row, true);
                    }
                }
                if !kind_registered {
                    return Err(err(CoreErrorCode::ArtifactSchemaInvalid)
                        .with_detail("reason", "codec_not_registered".into())
                        .with_detail("kind", input.kind.clone().into()));
                }
                if !document_exists_tx(conn, &input.document_id)? {
                    return Err(err(CoreErrorCode::DocNotFound));
                }
                conn.execute(
                "INSERT INTO artifact(id, document_id, kind, title, current_version_id, revision, \
                 idempotency_key, created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,NULL,1,?5,?6,?6)",
                params![artifact_id, input.document_id, input.kind, input.title, key, now],
            )
            .map_err(|e| db_app_error(&e))?;
                let artifact = load_artifact_tx(conn, &artifact_id)?
                    .ok_or_else(|| err(CoreErrorCode::Internal))?;
                let version = insert_version_tx(
                    conn,
                    &version_id,
                    &artifact,
                    None,
                    ArtifactVersionOrigin::AiDraft,
                    &codec_schema_version(&input.kind, &content_json),
                    &content_json,
                    input.snapshot_id.as_deref(),
                    input.task_id.as_deref(),
                    &coverage_json,
                    now,
                )?;
                // 设为当前版本(首次生成)。
                conn.execute(
                    "UPDATE artifact SET current_version_id=?1, updated_at=?2 WHERE id=?3",
                    params![version.id, now, artifact_id],
                )
                .map_err(|e| db_app_error(&e))?;
                let row = load_artifact_tx(conn, &artifact_id)?
                    .ok_or_else(|| err(CoreErrorCode::Internal))?;
                artifact_view_tx(conn, &row, true)
            })?;
        self.emit_artifact(
            &view.id,
            view.current_version_id.as_deref(),
            "ai_draft_committed",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// 在同一 artifact 下追加新的 ai_draft 版本(重新生成)。
    ///
    /// 规则:
    /// - 不新建 artifact,不改 kind/title/document_id;
    /// - 旧版本一律不改写(ai_draft 仍受数据库触发器保护);
    /// - 当前版本是 user_edit 时不切换当前版本,避免覆盖用户编辑;
    /// - 同一 task_id 幂等返回已产生的版本(UNIQUE(task_id) 兜底)。
    pub fn append_ai_draft(
        &self,
        artifact_id: &str,
        input: &RegeneratedDraftInput,
    ) -> Result<RegeneratedDraft, AppError> {
        let artifact_id = artifact_id.to_string();
        let kind = {
            let aid = artifact_id.clone();
            self.db.read(move |conn| {
                let row =
                    load_artifact_tx(conn, &aid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
                Ok(row.kind)
            })?
        };
        let content_json = self.validate_content(&kind, &input.content)?;
        let coverage_json = serde_json::to_string(&input.coverage)
            .map_err(|_| err(CoreErrorCode::InvalidRequest))?;
        let snapshot_id = input.snapshot_id.clone();
        let task_id = input.task_id.clone();
        let version_id = self.ids.new_entity_id();
        let now = self.clock.now_ms();
        let result = self.db.write(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if let Some(tid) = &task_id {
                if let Some(existing) = find_version_by_task_tx(conn, tid)? {
                    if existing.artifact_id != row.id {
                        return Err(err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "task_artifact_mismatch".into()));
                    }
                    return Ok(RegeneratedDraft {
                        artifact: artifact_view_tx(conn, &row, true)?,
                        version_id: existing.id,
                    });
                }
            }
            let current = match &row.current_version_id {
                Some(id) => {
                    Some(load_version_tx(conn, id)?.ok_or_else(|| err(CoreErrorCode::Internal))?)
                }
                None => None,
            };
            let version = insert_version_tx(
                conn,
                &version_id,
                &row,
                row.current_version_id.as_deref(),
                ArtifactVersionOrigin::AiDraft,
                &codec_schema_version(&row.kind, &content_json),
                &content_json,
                snapshot_id.as_deref(),
                task_id.as_deref(),
                &coverage_json,
                now,
            )?;
            // 用户正在编辑时保留其当前版本;用户可从版本列表主动选择新草稿。
            let switch_current = current
                .as_ref()
                .map(|v| v.origin != ArtifactVersionOrigin::UserEdit)
                .unwrap_or(true);
            if switch_current {
                conn.execute(
                    "UPDATE artifact SET current_version_id=?1, revision=revision+1, \
                     updated_at=?2 WHERE id=?3",
                    params![version.id, now, artifact_id],
                )
                .map_err(|e| db_app_error(&e))?;
            } else {
                conn.execute(
                    "UPDATE artifact SET revision=revision+1, updated_at=?1 WHERE id=?2",
                    params![now, artifact_id],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            Ok(RegeneratedDraft {
                artifact: artifact_view_tx(conn, &row, true)?,
                version_id: version.id,
            })
        })?;
        self.emit_artifact(
            &result.artifact.id,
            Some(&result.version_id),
            "ai_draft_regenerated",
            result.artifact.revision,
            &result.artifact.document_id,
        );
        Ok(result)
    }

    /// 幂等回放:同一 Task 在该 artifact 下已产生的版本。跨 artifact 的
    /// 幂等键复用返回明确错误,不把别的产物版本当成本次结果。
    pub fn find_regenerated_by_task(
        &self,
        task_id: &str,
        expected_artifact_id: &str,
    ) -> Result<Option<RegeneratedDraft>, AppError> {
        let task_id = task_id.to_string();
        let expected = expected_artifact_id.to_string();
        self.db.read(move |conn| {
            let Some(version) = find_version_by_task_tx(conn, &task_id)? else {
                return Ok(None);
            };
            if version.artifact_id != expected {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "task_artifact_mismatch".into()));
            }
            let row = load_artifact_tx(conn, &version.artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            Ok(Some(RegeneratedDraft {
                artifact: artifact_view_tx(conn, &row, true)?,
                version_id: version.id,
            }))
        })
    }

    // —— 产物中心(《M10》§7/§9)——

    /// `artifact_list`:按文档与类型筛选,updated_at 倒序分页。
    pub fn list_artifacts(
        &self,
        document_id: &str,
        kind: Option<&str>,
        page: PageRequest,
    ) -> Result<Page<ArtifactListItem>, AppError> {
        let page = page.normalized();
        let did = document_id.to_string();
        let kind = kind.map(str::to_string);
        let limit = page.limit;
        let cursor: Option<(i64, String)> = page
            .cursor
            .as_deref()
            .and_then(crate::shared::paging::decode_cursor);
        self.db.read(move |conn| {
            let (mut sql, mut binds) = if did.trim().is_empty() {
                (
                    String::from(
                        "SELECT a.id, a.document_id, a.kind, a.title, a.current_version_id, \
                         v.version_no, v.state, v.coverage_json, \
                         (SELECT COUNT(*) FROM citation c WHERE c.owner_type='artifact_version' \
                          AND c.owner_id = a.current_version_id), \
                         a.updated_at \
                         FROM artifact a LEFT JOIN artifact_version v ON v.id = a.current_version_id \
                         WHERE 1=1",
                    ),
                    vec![],
                )
            } else {
                if !document_exists_tx(conn, &did)? {
                    return Err(err(CoreErrorCode::DocNotFound));
                }
                (
                    String::from(
                        "SELECT a.id, a.document_id, a.kind, a.title, a.current_version_id, \
                         v.version_no, v.state, v.coverage_json, \
                         (SELECT COUNT(*) FROM citation c WHERE c.owner_type='artifact_version' \
                          AND c.owner_id = a.current_version_id), \
                         a.updated_at \
                         FROM artifact a LEFT JOIN artifact_version v ON v.id = a.current_version_id \
                         WHERE a.document_id=?1",
                    ),
                    vec![rusqlite::types::Value::Text(did.clone())],
                )
            };
            if let Some(k) = &kind {
                binds.push(rusqlite::types::Value::Text(k.clone()));
                sql.push_str(&format!(" AND a.kind=?{}", binds.len()));
            }
            if let Some((ts, id)) = &cursor {
                binds.push(rusqlite::types::Value::Integer(*ts));
                binds.push(rusqlite::types::Value::Text(id.clone()));
                sql.push_str(&format!(
                    " AND (a.updated_at < ?{} OR (a.updated_at = ?{} AND a.id < ?{}))",
                    binds.len() - 1,
                    binds.len() - 1,
                    binds.len()
                ));
            }
            sql.push_str(&format!(
                " ORDER BY a.updated_at DESC, a.id DESC LIMIT ?{}",
                binds.len() + 1
            ));
            binds.push(rusqlite::types::Value::Integer(limit + 1));
            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(binds), |r| {
                    let coverage_json: String = r.get(7)?;
                    let coverage: Option<CoverageReport> =
                        serde_json::from_str(&coverage_json).ok();
                    Ok(ArtifactListItem {
                        id: r.get(0)?,
                        document_id: r.get(1)?,
                        kind: r.get(2)?,
                        title: r.get(3)?,
                        current_version_id: r.get(4)?,
                        current_version_no: r.get(5)?,
                        current_version_state: r
                            .get::<_, Option<String>>(6)?
                            .as_deref()
                            .and_then(ArtifactVersionState::parse_db),
                        coverage_complete: coverage.map(|c| c.complete),
                        citation_count: r.get(8)?,
                        updated_at: r.get(9)?,
                    })
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(Page::from_probe(rows, limit, |r| {
                encode_cursor(&serde_json::json!({"ts": r.updated_at, "id": r.id}))
            }))
        })
    }

    /// 按版本 ID 反查(M11 QuizArtifactSource 等端口使用;只读投影)。
    pub fn find_version(&self, version_id: &str) -> Result<Option<VersionBrief>, AppError> {
        let version_id = version_id.to_string();
        self.db.read(move |conn| {
            let version = match load_version_tx(conn, &version_id)? {
                Some(v) => v,
                None => return Ok(None),
            };
            let row = load_artifact_tx(conn, &version.artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            Ok(Some(VersionBrief {
                version_id: version.id,
                artifact_id: row.id,
                document_id: row.document_id,
                kind: row.kind,
                origin: version.origin,
                schema_version: version.schema_version,
                content_json: version.content_json,
            }))
        })
    }

    /// 按 Task 反查产物(幂等重放:同一生成任务已产出时直接返回)。
    pub fn find_artifact_by_task(&self, task_id: &str) -> Result<Option<ArtifactView>, AppError> {
        let task_id = task_id.to_string();
        self.db.read(move |conn| {
            let artifact_id: Option<String> = conn
                .query_row(
                    "SELECT artifact_id FROM artifact_version WHERE task_id=?1",
                    [&task_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            match artifact_id {
                Some(id) => {
                    let row =
                        load_artifact_tx(conn, &id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
                    Ok(Some(artifact_view_tx(conn, &row, true)?))
                }
                None => Ok(None),
            }
        })
    }

    /// `artifact_get`:版本展开 + 来源摘要。
    pub fn get_artifact(
        &self,
        artifact_id: &str,
        version_id: Option<&str>,
    ) -> Result<ArtifactView, AppError> {
        let artifact_id = artifact_id.to_string();
        let version_id = version_id.map(str::to_string);
        self.db.read(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if let Some(vid) = &version_id {
                let v = load_version_tx(conn, vid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
                if v.artifact_id != row.id {
                    return Err(err(CoreErrorCode::NotFound));
                }
                // 指定历史版本:versions 只包含该版本,当前版本仍显示实际当前。
                let mut view = artifact_view_tx(conn, &row, false)?;
                view.versions = vec![version_view(&v)?];
                return Ok(view);
            }
            artifact_view_tx(conn, &row, true)
        })
    }

    /// `artifact_rename`:只改外壳标题,不新建版本。
    pub fn rename_artifact(
        &self,
        artifact_id: &str,
        title: &str,
        expected_revision: i64,
    ) -> Result<ArtifactView, AppError> {
        if title.trim().is_empty() || title.chars().count() > MAX_ARTIFACT_TITLE_CHARS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "artifact_title_invalid".into()));
        }
        let artifact_id = artifact_id.to_string();
        let title = title.to_string();
        let now = self.clock.now_ms();
        let view = self.db.write(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.revision != expected_revision {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch)
                    .with_detail("expected", expected_revision.into())
                    .with_detail("actual", row.revision.into()));
            }
            conn.execute(
                "UPDATE artifact SET title=?1, revision=revision+1, updated_at=?2 WHERE id=?3",
                params![title, now, artifact_id],
            )
            .map_err(|e| db_app_error(&e))?;
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            artifact_view_tx(conn, &row, true)
        })?;
        self.emit_artifact(
            &view.id,
            view.current_version_id.as_deref(),
            "renamed",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    // —— 编辑与版本保护(《M10》§3)——

    /// `artifact_begin_edit`:当前为 AI/封存版本 ⇒ 复制创建 user_edit 并设为
    /// 当前;已有可变 user_edit 且 base 为其祖先/相同 ⇒ 复用,不新建。
    pub fn begin_edit(
        &self,
        input: &ArtifactBeginEditInput,
        idempotency_key: &str,
    ) -> Result<ArtifactView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let artifact_id = input.artifact_id.clone();
        let base_version_id = input.base_version_id.clone();
        let key = idempotency_key.to_string();
        let version_id = self.ids.new_entity_id();
        let now = self.clock.now_ms();
        let view = self.db.write(move |conn| {
            // begin_edit 的幂等以 (artifact, base) 判定:若当前已是基于该 base
            // 的可变 user_edit,直接复用(避免每次调用新建版本)。
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let base = load_version_tx(conn, &base_version_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if base.artifact_id != row.id {
                return Err(err(CoreErrorCode::NotFound));
            }
            if let Some(cur_id) = &row.current_version_id {
                let cur =
                    load_version_tx(conn, cur_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
                if cur.origin == ArtifactVersionOrigin::UserEdit
                    && cur.state == ArtifactVersionState::Mutable
                {
                    // 已有可变用户版本:若编辑基座就是当前版本,复用;
                    // 若用户从历史版本重新开始编辑,仍新建(保留中间历史)。
                    if cur.id == base_version_id {
                        return artifact_view_tx(conn, &row, true);
                    }
                }
            }
            // 复制创建 user_edit(AI/封存/历史版本均可作为基座)。
            let version = insert_version_tx(
                conn,
                &version_id,
                &row,
                Some(&base_version_id),
                ArtifactVersionOrigin::UserEdit,
                &base.schema_version,
                &base.content_json,
                base.snapshot_id.as_deref(),
                None,
                &base.coverage_json,
                now,
            )?;
            conn.execute(
                "UPDATE artifact SET current_version_id=?1, updated_at=?2 WHERE id=?3",
                params![version.id, now, artifact_id],
            )
            .map_err(|e| db_app_error(&e))?;
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            artifact_view_tx(conn, &row, true)
        })?;
        let _ = key;
        self.emit_artifact(
            &view.id,
            view.current_version_id.as_deref(),
            "edit_begun",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// `artifact_save`:只有 user_edit + mutable 可保存。summary 的降级规则:
    /// document_fact 段引用全部移除后必须改为非事实 kind(校验拒绝)。
    pub fn save_version(
        &self,
        input: &ArtifactSaveInput,
        idempotency_key: &str,
    ) -> Result<ArtifactView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let artifact_id = input.artifact_id.clone();
        let version_id = input.version_id.clone();
        let expected = input.expected_revision;
        let content_json =
            self.validate_content_by_version_tx(&artifact_id, &version_id, &input.content)?;
        let now = self.clock.now_ms();
        let version_id_for_emit = version_id.clone();
        let view = self.db.write(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let version =
                load_version_tx(conn, &version_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if version.artifact_id != row.id {
                return Err(err(CoreErrorCode::NotFound));
            }
            // 服务端强制:AI 原版与封存版本不可保存(《M10》§3)。
            if version.origin != ArtifactVersionOrigin::UserEdit
                || version.state != ArtifactVersionState::Mutable
            {
                return Err(err(CoreErrorCode::ArtifactVersionImmutable)
                    .with_detail("origin", version.origin.as_str().into())
                    .with_detail("state", version.state.as_str().into()));
            }
            if version.revision != expected {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch)
                    .with_detail("expected", expected.into())
                    .with_detail("actual", version.revision.into()));
            }
            // 只有当前版本可保存;编辑历史版本走 begin_edit。
            if row.current_version_id.as_deref() != Some(version_id.as_str()) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "not_current_version".into()));
            }
            conn.execute(
                "UPDATE artifact_version SET content_json=?1, revision=revision+1, updated_at=?2 \
                 WHERE id=?3",
                params![content_json, now, version_id],
            )
            .map_err(|e| db_write_app_error(&e))?;
            conn.execute(
                "UPDATE artifact SET updated_at=?1 WHERE id=?2",
                params![now, artifact_id],
            )
            .map_err(|e| db_app_error(&e))?;
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            artifact_view_tx(conn, &row, true)
        })?;
        self.emit_artifact(
            &view.id,
            Some(&version_id_for_emit),
            "saved",
            view.current_version
                .as_ref()
                .map(|v| v.revision)
                .unwrap_or(1),
            &view.document_id,
        );
        Ok(view)
    }

    /// 事务外按版本所属 kind 校验内容(节省事务持锁)。
    fn validate_content_by_version_tx(
        &self,
        artifact_id: &str,
        version_id: &str,
        content: &serde_json::Value,
    ) -> Result<String, AppError> {
        let artifact_id = artifact_id.to_string();
        let version_id = version_id.to_string();
        let kind = self.db.read(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let version =
                load_version_tx(conn, &version_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if version.artifact_id != row.id {
                return Err(err(CoreErrorCode::NotFound));
            }
            Ok((row.kind, version.schema_version))
        })?;
        let codec = self.codec_for(&kind.0)?;
        if !codec.validate(content) {
            return Err(err(CoreErrorCode::ArtifactSchemaInvalid)
                .with_detail("reason", "schema_validation_failed".into())
                .with_detail("kind", kind.0.into()));
        }
        serde_json::to_string(content).map_err(|_| err(CoreErrorCode::ArtifactSchemaInvalid))
    }

    /// `artifact_seal`:封存当前用户版本;AI 原版无需封存。
    pub fn seal_version(
        &self,
        artifact_id: &str,
        version_id: &str,
        expected_revision: i64,
    ) -> Result<ArtifactView, AppError> {
        let artifact_id = artifact_id.to_string();
        let version_id = version_id.to_string();
        let expected = expected_revision;
        let now = self.clock.now_ms();
        let version_id_for_emit = version_id.clone();
        let view = self.db.write(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let version = load_version_tx(conn, &version_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if version.artifact_id != row.id {
                return Err(err(CoreErrorCode::NotFound));
            }
            if version.origin != ArtifactVersionOrigin::UserEdit {
                return Err(err(CoreErrorCode::ArtifactVersionImmutable)
                    .with_detail("reason", "only_user_edit_can_seal".into()));
            }
            if version.revision != expected {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch)
                    .with_detail("expected", expected.into())
                    .with_detail("actual", version.revision.into()));
            }
            if row.current_version_id.as_deref() != Some(version_id.as_str()) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "not_current_version".into()));
            }
            conn.execute(
                "UPDATE artifact_version SET state='sealed', sealed_at=?1, updated_at=?1 WHERE id=?2",
                params![now, version_id],
            )
            .map_err(|e| db_write_app_error(&e))?;
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            artifact_view_tx(conn, &row, true)
        })?;
        self.emit_artifact(
            &view.id,
            Some(&version_id_for_emit),
            "sealed",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// `artifact_restore`:从历史版本创建 restore 新版本并设为当前;
    /// 不修改任何历史版本。
    pub fn restore_version(
        &self,
        artifact_id: &str,
        source_version_id: &str,
        _idempotency_key: &str,
    ) -> Result<ArtifactView, AppError> {
        let artifact_id = artifact_id.to_string();
        let source_version_id = source_version_id.to_string();
        let version_id = self.ids.new_entity_id();
        let now = self.clock.now_ms();
        let view = self.db.write(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let source = load_version_tx(conn, &source_version_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if source.artifact_id != row.id {
                return Err(err(CoreErrorCode::NotFound));
            }
            // 恢复源本身可变(未封存的 user_edit)时拒绝:避免复制一个
            // 会继续被自动保存改写的基座,恢复语义要求内容确定。
            if source.origin == ArtifactVersionOrigin::UserEdit
                && source.state == ArtifactVersionState::Mutable
            {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "restore_source_mutable".into()));
            }
            let version = insert_version_tx(
                conn,
                &version_id,
                &row,
                Some(&source_version_id),
                ArtifactVersionOrigin::Restore,
                &source.schema_version,
                &source.content_json,
                source.snapshot_id.as_deref(),
                None,
                &source.coverage_json,
                now,
            )?;
            conn.execute(
                "UPDATE artifact SET current_version_id=?1, updated_at=?2 WHERE id=?3",
                params![version.id, now, artifact_id],
            )
            .map_err(|e| db_app_error(&e))?;
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            artifact_view_tx(conn, &row, true)
        })?;
        self.emit_artifact(
            &view.id,
            view.current_version_id.as_deref(),
            "restored",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    /// `artifact_copy`:复制为新产物(默认)或新版本(显式选项)。
    pub fn copy_artifact(
        &self,
        artifact_id: &str,
        version_id: &str,
        as_new_artifact: bool,
        idempotency_key: &str,
    ) -> Result<ArtifactView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let artifact_id = artifact_id.to_string();
        let version_id = version_id.to_string();
        let new_artifact_id = self.ids.new_entity_id();
        let new_version_id = self.ids.new_entity_id();
        let key = idempotency_key.to_string();
        let now = self.clock.now_ms();
        let result_artifact_id = self.db.write(move |conn| {
            if let Some(row) = load_artifact_by_key_tx(conn, &key)? {
                return Ok(row.id);
            }
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let source =
                load_version_tx(conn, &version_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if source.artifact_id != row.id {
                return Err(err(CoreErrorCode::NotFound));
            }
            if as_new_artifact {
                conn.execute(
                    "INSERT INTO artifact(id, document_id, kind, title, current_version_id, \
                     revision, idempotency_key, created_at, updated_at) \
                     VALUES (?1,?2,?3,?4,NULL,1,?5,?6,?6)",
                    params![
                        new_artifact_id,
                        row.document_id,
                        row.kind,
                        format!("{}(副本)", row.title),
                        key,
                        now
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
                let new_row = load_artifact_tx(conn, &new_artifact_id)?
                    .ok_or_else(|| err(CoreErrorCode::Internal))?;
                let version = insert_version_tx(
                    conn,
                    &new_version_id,
                    &new_row,
                    Some(&version_id),
                    ArtifactVersionOrigin::Copy,
                    &source.schema_version,
                    &source.content_json,
                    source.snapshot_id.as_deref(),
                    None,
                    &source.coverage_json,
                    now,
                )?;
                conn.execute(
                    "UPDATE artifact SET current_version_id=?1, updated_at=?2 WHERE id=?3",
                    params![version.id, now, new_artifact_id],
                )
                .map_err(|e| db_app_error(&e))?;
                Ok(new_artifact_id)
            } else {
                // 复制为同产物新版本(copy 版本,设为当前)。
                let version = insert_version_tx(
                    conn,
                    &new_version_id,
                    &row,
                    Some(&version_id),
                    ArtifactVersionOrigin::Copy,
                    &source.schema_version,
                    &source.content_json,
                    source.snapshot_id.as_deref(),
                    None,
                    &source.coverage_json,
                    now,
                )?;
                conn.execute(
                    "UPDATE artifact SET current_version_id=?1, updated_at=?2 WHERE id=?3",
                    params![version.id, now, artifact_id],
                )
                .map_err(|e| db_app_error(&e))?;
                Ok(artifact_id)
            }
        })?;
        let view = self.get_artifact(&result_artifact_id, None)?;
        self.emit_artifact(
            &view.id,
            view.current_version_id.as_deref(),
            "copied",
            view.revision,
            &view.document_id,
        );
        Ok(view)
    }

    // —— 删除(《M10》§9)——

    /// 删除预览:引用、版本、导出记录与待澄清关联;不执行删除。
    pub fn delete_preview(&self, artifact_id: &str) -> Result<ArtifactDeletePreview, AppError> {
        let artifact_id = artifact_id.to_string();
        self.db.read(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let version_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM artifact_version WHERE artifact_id=?1",
                    [&artifact_id],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            let citation_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM citation WHERE owner_type='artifact_version' AND \
                     owner_id IN (SELECT id FROM artifact_version WHERE artifact_id=?1)",
                    [&artifact_id],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            let export_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM export_record WHERE artifact_id=?1",
                    [&artifact_id],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            let clarification_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM clarification_item WHERE \
                     json_extract(resolution_ref_json, '$.type') = 'artifact_version' \
                     AND json_extract(resolution_ref_json, '$.id') IN \
                     (SELECT id FROM artifact_version WHERE artifact_id=?1)",
                    [&artifact_id],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            Ok(ArtifactDeletePreview {
                artifact_id: row.id,
                version_count,
                citation_count,
                export_record_count: export_count,
                clarification_ref_count: clarification_count,
            })
        })
    }

    /// `artifact_delete`:只删除本产物及其版本/引用/导出记录;
    /// 不删除原文、笔记或其他产物。expectedRevision 为外壳 revision。
    pub fn delete_artifact(
        &self,
        artifact_id: &str,
        expected_revision: i64,
    ) -> Result<DeleteResult, AppError> {
        let artifact_id = artifact_id.to_string();
        let artifact_id_for_emit = artifact_id.clone();
        let now = self.clock.now_ms();
        let (document_id, revision) = self.db.write(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.revision != expected_revision {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch)
                    .with_detail("expected", expected_revision.into())
                    .with_detail("actual", row.revision.into()));
            }
            let version_ids: Vec<String> = {
                let mut stmt = conn
                    .prepare("SELECT id FROM artifact_version WHERE artifact_id=?1")
                    .map_err(|e| db_app_error(&e))?;
                let rows = stmt
                    .query_map([&artifact_id], |r| r.get(0))
                    .map_err(|e| db_app_error(&e))?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|e| db_app_error(&e))?
            };
            // 引用按属主级联清理(M05 §9)。
            for vid in &version_ids {
                conn.execute(
                    "DELETE FROM citation WHERE owner_type='artifact_version' AND owner_id=?1",
                    [vid],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            conn.execute("DELETE FROM artifact WHERE id=?1", [&artifact_id])
                .map_err(|e| db_app_error(&e))?;
            let _ = now;
            Ok((row.document_id, row.revision))
        })?;
        self.emit_artifact(
            &artifact_id_for_emit,
            None,
            "deleted",
            revision,
            &document_id,
        );
        Ok(DeleteResult {
            deleted: true,
            id: artifact_id_for_emit.clone(),
        })
    }

    // —— 导出(《M10》§8)——

    /// 导出记录写入(成功/失败都记录,只存状态与格式)。
    #[allow(clippy::too_many_arguments)]
    pub fn record_export_tx(
        conn: &Connection,
        id: &str,
        document_id: &str,
        artifact_id: &str,
        artifact_version_id: &str,
        format: &str,
        destination_kind: &str,
        status: &str,
        error_code: Option<&str>,
        now: i64,
    ) -> Result<(), AppError> {
        conn.execute(
            "INSERT INTO export_record(id, document_id, artifact_id, artifact_version_id, \
             format, destination_kind, status, error_code, created_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                id,
                document_id,
                artifact_id,
                artifact_version_id,
                format,
                destination_kind,
                status,
                error_code,
                now
            ],
        )
        .map_err(|e| db_app_error(&e))?;
        Ok(())
    }

    /// 读取导出所需的版本与文档信息(导出器入参投影)。
    pub fn export_projection(
        &self,
        artifact_id: &str,
        version_id: &str,
    ) -> Result<ExportProjection, AppError> {
        let artifact_id = artifact_id.to_string();
        let version_id = version_id.to_string();
        self.db.read(move |conn| {
            let row = load_artifact_tx(conn, &artifact_id)?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let version =
                load_version_tx(conn, &version_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if version.artifact_id != row.id {
                return Err(err(CoreErrorCode::NotFound));
            }
            let doc_title: Option<String> = conn
                .query_row(
                    "SELECT display_name FROM document WHERE id=?1",
                    [&row.document_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            Ok(ExportProjection {
                artifact_id: row.id,
                document_id: row.document_id,
                document_title: doc_title.unwrap_or_default(),
                artifact_title: row.title,
                kind: row.kind,
                schema_version: version.schema_version,
                content_json: version.content_json.clone(),
                coverage_json: version.coverage_json.clone(),
                version_no: version.version_no,
                created_at: version.created_at,
            })
        })
    }

    /// 导出/来源面板共享的来源投影:按原文引用与用户笔记分开(《M10》§7/§8)。
    pub fn markdown_sources(
        &self,
        version_id: &str,
    ) -> Result<crate::exporters::markdown::MarkdownSourceProjection, AppError> {
        let version_id = version_id.to_string();
        self.db.read(move |conn| {
            let mut citations = Vec::new();
            {
                let mut stmt = conn
                    .prepare(
                        "SELECT c.id, c.claim_key, c.quoted_text, c.content_block_id \
                         FROM citation c WHERE c.owner_type='artifact_version' AND c.owner_id=?1 \
                         ORDER BY c.created_at, c.id",
                    )
                    .map_err(|e| db_app_error(&e))?;
                let rows: Vec<(String, String, String, Option<String>)> = stmt
                    .query_map([&version_id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })
                    .map_err(|e| db_app_error(&e))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| db_app_error(&e))?;
                for (id, claim_key, quoted, block_id) in rows {
                    let (page_number, heading_path) = block_id
                        .as_deref()
                        .and_then(|bid| {
                            conn.query_row(
                                "SELECT locator_json, heading_path_json FROM content_block \
                                 WHERE id=?1",
                                [bid],
                                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                            )
                            .optional()
                            .ok()
                            .flatten()
                        })
                        .map(|(locator_json, heading_json)| {
                            let page = serde_json::from_str::<SourceLocator>(&locator_json)
                                .ok()
                                .and_then(|l| l.pdf_page());
                            let heading = serde_json::from_str::<Vec<String>>(&heading_json)
                                .unwrap_or_default();
                            (page, heading)
                        })
                        .unwrap_or((None, vec![]));
                    citations.push(crate::exporters::markdown::MarkdownCitation {
                        citation_id: id,
                        claim_key,
                        page_number,
                        heading_path,
                        quoted_text: quoted,
                    });
                }
            }
            // 用户笔记来源:该文档的 note(body 非空的 user 笔记摘要)。
            let mut notes = Vec::new();
            {
                let version_doc: Option<String> = conn
                    .query_row(
                        "SELECT a.document_id FROM artifact a \
                         JOIN artifact_version v ON v.artifact_id = a.id WHERE v.id=?1",
                        [&version_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| db_app_error(&e))?;
                if let Some(doc_id) = version_doc {
                    let mut stmt = conn
                        .prepare(
                            "SELECT id, body FROM note WHERE document_id=?1 AND \
                             origin IN ('user','user_edited_ai') AND body != '' \
                             ORDER BY updated_at LIMIT 50",
                        )
                        .map_err(|e| db_app_error(&e))?;
                    let rows: Vec<(String, String)> = stmt
                        .query_map([&doc_id], |r| Ok((r.get(0)?, r.get(1)?)))
                        .map_err(|e| db_app_error(&e))?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| db_app_error(&e))?;
                    for (id, body) in rows {
                        let text: String = body.chars().take(200).collect();
                        notes.push(crate::exporters::markdown::MarkdownNoteSource {
                            note_id: id,
                            text,
                        });
                    }
                }
            }
            Ok(crate::exporters::markdown::MarkdownSourceProjection { citations, notes })
        })
    }

    // —— Citation 属主校验器(M05 注册;artifact_version)——

    /// M05 Citation 属主 = artifact_version 时的存在性校验(事务内)。
    pub fn artifact_version_owner_exists_tx(
        conn: &Connection,
        version_id: &str,
        document_id: &str,
    ) -> Result<bool, AppError> {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM artifact_version av JOIN artifact a ON a.id = av.artifact_id \
                 WHERE av.id=?1 AND a.document_id=?2",
                params![version_id, document_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| db_app_error(&e))?;
        Ok(exists.is_some())
    }
}

/// 按版本 ID 反查的只读投影(端口适配用)。
#[derive(Debug, Clone)]
pub struct VersionBrief {
    pub version_id: String,
    pub artifact_id: String,
    pub document_id: String,
    pub kind: String,
    pub origin: ArtifactVersionOrigin,
    pub schema_version: String,
    pub content_json: String,
}

/// 向已有 artifact 追加 AI 草稿的输入(重新生成)。
/// kind/title/document_id 一律取 artifact 自身,调用方不能改写外壳。
#[derive(Debug, Clone)]
pub struct RegeneratedDraftInput {
    pub content: serde_json::Value,
    pub coverage: CoverageReport,
    pub snapshot_id: Option<String>,
    pub task_id: Option<String>,
}

/// 追加结果:artifact 视图(当前版本可能仍是用户编辑版)+ 新版本 ID。
#[derive(Debug, Clone)]
pub struct RegeneratedDraft {
    pub artifact: ArtifactView,
    pub version_id: String,
}

/// AI 草稿落库输入(M09 编排输出 → M10 保存;测试直接驱动)。#[derive(Debug, Clone)]
pub struct AiDraftCommitInput {
    pub document_id: String,
    pub kind: String,
    pub title: String,
    pub content: serde_json::Value,
    pub coverage: CoverageReport,
    pub snapshot_id: Option<String>,
    pub task_id: Option<String>,
    /// 额外生成的 Citation(可选;M09 校验后传入,M05 写入)。
    pub citations: Vec<CitationDraftForCommit>,
}

/// 保存 AI 草稿时的引用草稿(claimKey = segment id;owner=artifact_version)。
#[derive(Debug, Clone)]
pub struct CitationDraftForCommit {
    pub claim_key: String,
    pub claim_kind: ClaimKind,
    pub content_block_id: String,
    pub quoted_text: String,
}

/// 导出器入参投影(《M10》§8:ArtifactVersion + 来源投影)。
#[derive(Debug, Clone)]
pub struct ExportProjection {
    pub artifact_id: String,
    pub document_id: String,
    pub document_title: String,
    pub artifact_title: String,
    pub kind: String,
    pub schema_version: String,
    pub content_json: String,
    pub coverage_json: String,
    pub version_no: i64,
    pub created_at: i64,
}

/// 从版本 JSON 提取 schemaVersion(INSERT 前用)。
fn codec_schema_version(kind: &str, content_json: &str) -> String {
    let _ = kind;
    serde_json::from_str::<serde_json::Value>(content_json)
        .ok()
        .and_then(|v| {
            v.get("schemaVersion")
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".into())
}
