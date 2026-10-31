//! M04 语义解析与索引服务(《M04》§2–§10)。
//!
//! 关键不变量:
//! - M04 独占 ContentBlock 与索引写入;其他模块经本服务的公开接口消费。
//! - 解析块 ID = `UUIDv5(NS, generationId + ":" + blockKey)`:同代次同 key
//!   稳定 ⇒ Worker 崩溃重启后重提批次天然去重(《M04》§2、A1)。
//! - Worker 不受信:批次边界、定位格式、文本规范化与哈希全部由服务端裁决。
//! - 批次、完成、失败都在同一事务内校验任务/Attempt/取消状态,取消后零写入。
//! - 事件(`parse.progress` / `parser.request`)仅在 COMMIT 成功后发出。
//! - FTS 行是可重建缓存;完成时做完整性检查并修复(A9)。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};

use crate::document::models::DocumentFormat;
use crate::persistence::task::{CreateTask, TaskRow, TaskService};
use crate::persistence::{db_app_error, Db};
use crate::shared::dto::{TaskOutcome, TaskPhase, TaskStatus, TaskType};
use crate::shared::locator::{PageRotation, SourceLocator};
use crate::shared::textnorm::{normalize_v1, sha256_str};
use crate::shared::tokens::{TokenError, TokenRegistry};
use crate::shared::{
    AppError, Clock, CoreErrorCode, DomainEvent, DomainEventType, EventSink, IdGenerator,
};

use super::fts;
use super::models::*;

/// 批次与读取的硬上限(《M04》§3、《02》§5)。
pub const MAX_BLOCKS_PER_BATCH: usize = 32;
pub const MAX_BATCH_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_READ_CHUNK_BYTES: i64 = 1024 * 1024;
pub const MAX_EMBEDDING_BATCH: usize = 64;
pub const NORMALIZATION_VERSION: &str = "normalize_v1";
// OCR 逐页执行可能超过 10 分钟;令牌仍只绑定当前 Attempt,达上限即废弃。
const JOB_TOKEN_TTL_MS: i64 = 60 * 60 * 1000;
const FTS_SNIPPET_TOKENS: i64 = 16;

/// ParserConfigHash 输入(《M04》§2):任何一项变化都产生新代次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParserVersions {
    pub parser_version: String,
    pub adapter_version: String,
    pub normalization_version: String,
    pub segmentation_version: String,
    pub sanitizer_version: String,
}

impl Default for ParserVersions {
    fn default() -> Self {
        Self {
            parser_version: "knowwise-parser/2+ocr-6.0.1".into(),
            adapter_version: "format-adapter/1".into(),
            normalization_version: NORMALIZATION_VERSION.into(),
            segmentation_version: "segmentation/1".into(),
            sanitizer_version: "sanitizer/1".into(),
        }
    }
}

/// 配置哈希是纯函数:解析器/适配器/规范化/分段/清洗版本 + 格式。
pub fn parser_config_hash_of(v: &ParserVersions, format: DocumentFormat) -> String {
    let canonical = format!(
        "adapter={}\nnormalization={}\nparser={}\nsegmentation={}\nsanitizer={}\nformat={}",
        v.adapter_version,
        v.normalization_version,
        v.parser_version,
        v.segmentation_version,
        v.sanitizer_version,
        format.as_str(),
    );
    sha256_str(&canonical)
}

/// 解析桥令牌绑定:只存在于服务端内存,不落库、不进日志。
#[derive(Debug, Clone)]
pub struct ParseJobBinding {
    pub task_id: String,
    pub attempt_id: String,
    pub document_id: String,
    pub version_id: String,
    pub generation_id: String,
    pub format: DocumentFormat,
    pub path: PathBuf,
    pub size: i64,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct BatchAck {
    #[ts(type = "number")]
    pub accepted_blocks: i64,
    #[ts(type = "number")]
    pub total_blocks: i64,
    #[ts(type = "number")]
    pub last_batch_seq: i64,
}

#[derive(Debug, Clone)]
pub struct EmbeddingInput {
    pub block_id: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct VectorHit {
    pub block_id: String,
    pub distance: f64,
}

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

/// 定位联合必须与文档格式匹配(封闭联合不允许多态穿透)。
pub fn locator_matches_format(locator: &SourceLocator, format: DocumentFormat) -> bool {
    match locator {
        SourceLocator::Pdf { .. } => format == DocumentFormat::Pdf,
        SourceLocator::Markdown { .. } => format == DocumentFormat::Markdown,
        SourceLocator::Docx { .. } => format == DocumentFormat::Docx,
        SourceLocator::Composite { parts, .. } => {
            parts.iter().all(|p| locator_matches_format(p, format))
        }
    }
}

fn clear_ocr_pause_tx(conn: &Connection, version_id: &str, now: i64) -> Result<(), AppError> {
    conn.execute(
        "UPDATE page_ocr_control SET paused=0, paused_at=NULL, updated_at=?2 \
         WHERE document_version_id=?1",
        params![version_id, now],
    )
    .map_err(|e| db_app_error(&e))?;
    Ok(())
}

fn load_generation(conn: &Connection, id: &str) -> Result<Option<GenerationRow>, AppError> {
    conn.query_row(
        &format!("SELECT {GENERATION_COLS} FROM parse_generation WHERE id=?1"),
        [id],
        |r| GenerationRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_generation_by_task(
    conn: &Connection,
    task_id: &str,
) -> Result<Option<GenerationRow>, AppError> {
    conn.query_row(
        &format!(
            "SELECT {GENERATION_COLS} FROM parse_generation WHERE task_id=?1 \
             ORDER BY started_at DESC LIMIT 1"
        ),
        [task_id],
        |r| GenerationRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_current_generation(
    conn: &Connection,
    version_id: &str,
) -> Result<Option<GenerationRow>, AppError> {
    conn.query_row(
        &format!(
            "SELECT {GENERATION_COLS} FROM parse_generation \
             WHERE document_version_id=?1 AND is_current=1 LIMIT 1"
        ),
        [version_id],
        |r| GenerationRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_block(conn: &Connection, id: &str) -> Result<Option<BlockRow>, AppError> {
    conn.query_row(
        &format!("SELECT {BLOCK_COLS} FROM content_block WHERE id=?1"),
        [id],
        |r| BlockRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_block_by_key(
    conn: &Connection,
    generation_id: &str,
    block_key: &str,
) -> Result<Option<BlockRow>, AppError> {
    conn.query_row(
        &format!(
            "SELECT {BLOCK_COLS} FROM content_block \
             WHERE parse_generation_id=?1 AND block_key=?2 LIMIT 1"
        ),
        params![generation_id, block_key],
        |r| BlockRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

/// 主位置路径 + 大小(解析 Worker 的受控输入来源)。
fn load_input_source(conn: &Connection, document_id: &str) -> Result<(PathBuf, i64), AppError> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT path, COALESCE(last_seen_size,0) FROM file_location \
             WHERE document_id=?1 ORDER BY is_primary DESC, last_verified_at DESC LIMIT 1",
            [document_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    row.map(|(p, s)| (PathBuf::from(p), s))
        .ok_or_else(|| err(CoreErrorCode::DocFileMissing))
}

/// FTS 完整性修复:为缺失 FTS 行的可索引块补插(可重建缓存,A9)。
fn repair_fts_tx(conn: &Connection, generation_id: &str) -> Result<i64, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT c.id, c.normalized_text FROM content_block c \
             WHERE c.parse_generation_id=?1 AND c.indexable=1 \
               AND NOT EXISTS (SELECT 1 FROM content_block_fts f WHERE f.block_id = c.id)",
        )
        .map_err(|e| db_app_error(&e))?;
    let missing: Vec<(String, String)> = stmt
        .query_map([generation_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_app_error(&e))?;
    drop(stmt);
    for (block_id, text) in &missing {
        conn.execute(
            "INSERT INTO content_block_fts(text, block_id, parse_generation_id) VALUES (?1,?2,?3)",
            params![text, block_id, generation_id],
        )
        .map_err(|e| db_app_error(&e))?;
    }
    Ok(missing.len() as i64)
}

/// BlockSetHash:对代次内全部解析块的 (ordinal, blockKey, textHash) 排序哈希。
/// 与块 ID 无关 ⇒ 同一内容在不同代次重解析得到相同哈希(A1)。
fn compute_block_set_hash(conn: &Connection, generation_id: &str) -> Result<String, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT ordinal, block_key, text_hash FROM content_block \
             WHERE parse_generation_id=?1 AND origin IN ('parser','ocr') ORDER BY ordinal",
        )
        .map_err(|e| db_app_error(&e))?;
    let rows: Vec<String> = stmt
        .query_map([generation_id], |r| {
            Ok(format!(
                "{}|{}|{}\n",
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
            ))
        })
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_app_error(&e))?;
    let mut buffer = String::new();
    for line in rows {
        buffer.push_str(&line);
    }
    Ok(sha256_str(&buffer))
}

fn block_count(conn: &Connection, generation_id: &str) -> Result<i64, AppError> {
    conn.query_row(
        "SELECT COUNT(*) FROM content_block WHERE parse_generation_id=?1 AND origin IN ('parser','ocr')",
        [generation_id],
        |r| r.get(0),
    )
    .map_err(|e| db_app_error(&e))
}

fn fts_row_count(conn: &Connection, generation_id: &str) -> Result<i64, AppError> {
    conn.query_row(
        "SELECT COUNT(*) FROM content_block_fts WHERE parse_generation_id=?1",
        [generation_id],
        |r| r.get(0),
    )
    .map_err(|e| db_app_error(&e))
}

/// 区域统计:(complete, partial, failed, no_text_layer, total)。
fn region_stats(
    conn: &Connection,
    generation_id: &str,
) -> Result<(i64, i64, i64, i64, i64), AppError> {
    conn.query_row(
        "SELECT \
           SUM(CASE WHEN status='complete' THEN 1 ELSE 0 END), \
           SUM(CASE WHEN status='partial' THEN 1 ELSE 0 END), \
           SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END), \
           SUM(CASE WHEN status='no_text_layer' THEN 1 ELSE 0 END), \
           COUNT(*) \
         FROM parse_region WHERE generation_id=?1",
        [generation_id],
        |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                r.get::<_, i64>(4)?,
            ))
        },
    )
    .map_err(|e| db_app_error(&e))
}

pub struct ParseIndexService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    events: Arc<dyn EventSink>,
    tasks: Arc<TaskService>,
    versions: ParserVersions,
    job_tokens: TokenRegistry<ParseJobBinding>,
}

impl ParseIndexService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        events: Arc<dyn EventSink>,
        tasks: Arc<TaskService>,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            events,
            tasks,
            versions: ParserVersions::default(),
            job_tokens: TokenRegistry::default(),
        }
    }

    /// 测试注入:配置版本变化 → 新代次。
    pub fn with_versions(mut self, versions: ParserVersions) -> Self {
        self.versions = versions;
        self
    }

    pub fn parser_config_hash(&self, format: DocumentFormat) -> String {
        parser_config_hash_of(&self.versions, format)
    }

    fn emit_progress(
        &self,
        binding: &ParseJobBinding,
        state: &str,
        blocks: i64,
        regions_complete: i64,
        regions_total: i64,
    ) {
        let event = DomainEvent::new(
            DomainEventType::ParseProgress,
            binding.generation_id.clone(),
            1,
            self.clock.now_ms(),
            serde_json::json!({
                "generationId": binding.generation_id,
                "versionId": binding.version_id,
                "state": state,
                "blockCount": blocks,
                "regionsComplete": regions_complete,
                "regionsTotal": regions_total,
            }),
        )
        .with_document(
            Some(binding.document_id.clone()),
            Some(binding.version_id.clone()),
        )
        .with_task(Some(binding.task_id.clone()));
        self.events.emit(event);
    }

    fn emit_parser_request(&self, binding: &ParseJobBinding, token: &str, expires_at: i64) {
        let ticket = ParseJobTicket {
            token: token.to_string(),
            expires_at,
            task_id: binding.task_id.clone(),
            attempt_id: binding.attempt_id.clone(),
            document_id: binding.document_id.clone(),
            version_id: binding.version_id.clone(),
            generation_id: binding.generation_id.clone(),
            format: binding.format,
            input_size: binding.size,
            chunk_size: MAX_READ_CHUNK_BYTES,
            read_mode: ParseReadMode::Chunked,
        };
        let event = DomainEvent::new(
            DomainEventType::ParserRequest,
            binding.task_id.clone(),
            1,
            self.clock.now_ms(),
            serde_json::to_value(&ticket).unwrap_or(serde_json::Value::Null),
        )
        .with_document(
            Some(binding.document_id.clone()),
            Some(binding.version_id.clone()),
        )
        .with_task(Some(binding.task_id.clone()));
        self.events.emit(event);
    }

    fn issue_job_token(&self, binding: &ParseJobBinding) -> (String, i64) {
        let token = self.ids.new_token();
        let now = self.clock.now_ms();
        self.job_tokens
            .issue(&token, binding.clone(), now, JOB_TOKEN_TTL_MS);
        (token, now + JOB_TOKEN_TTL_MS)
    }

    fn validate_job_token(&self, token: &str) -> Result<ParseJobBinding, AppError> {
        self.job_tokens
            .validate(token, self.clock.now_ms())
            .map_err(|e| match e {
                TokenError::UnknownOrExpired => {
                    err(CoreErrorCode::FileAccessExpired).with_detail("scope", "parse_job".into())
                }
            })
    }

    /// 校验文档与版本:存在、归属正确、指纹 ready(《01》§4.1:
    /// 语义解析必须等待 ready)。
    fn validate_version(
        &self,
        document_id: &str,
        version_id: &str,
    ) -> Result<DocumentFormat, AppError> {
        let did = document_id.to_string();
        let vid = version_id.to_string();
        self.db.read(move |conn| {
            let format: Option<String> = conn
                .query_row("SELECT format FROM document WHERE id=?1", [&did], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let format =
                DocumentFormat::parse_db(&format.ok_or_else(|| err(CoreErrorCode::DocNotFound))?)
                    .ok_or_else(|| err(CoreErrorCode::Internal))?;
            let ver: Option<(String, String)> = conn
                .query_row(
                    "SELECT document_id, fingerprint_status FROM document_version WHERE id=?1",
                    [&vid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let (owner, fp) = ver.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if owner != did {
                return Err(err(CoreErrorCode::NotFound));
            }
            if fp != "ready" {
                return Err(err(CoreErrorCode::IndexGenerationUnavailable)
                    .with_detail("reason", "fingerprint_not_ready".into()));
            }
            Ok(format)
        })
    }

    /// `parse_start`:开始解析与索引;重复请求返回已有活动任务(《02》§3.1)。
    pub fn start_parse(
        &self,
        document_id: &str,
        version_id: &str,
        _reason: &str,
        idempotency_key: &str,
    ) -> Result<TaskRow, AppError> {
        self.start_parse_inner(
            document_id,
            version_id,
            _reason,
            idempotency_key,
            false,
            false,
        )
    }

    /// 2D “重新识别全文”:复用当前代次重跑 OCR，保留旧页状态与块 ID。
    /// 只有 parser 配置变化才创建新代次；前端逐页增量识别时不会走这里。
    pub fn reprocess_ocr(
        &self,
        document_id: &str,
        version_id: &str,
        idempotency_key: &str,
    ) -> Result<TaskRow, AppError> {
        self.start_parse_inner(
            document_id,
            version_id,
            "reprocess_ocr",
            idempotency_key,
            false,
            true,
        )
    }

    fn start_parse_inner(
        &self,
        document_id: &str,
        version_id: &str,
        _reason: &str,
        idempotency_key: &str,
        force_new_generation: bool,
        allow_complete_reuse: bool,
    ) -> Result<TaskRow, AppError> {
        let format = self.validate_version(document_id, version_id)?;
        let task = self.tasks.create_task(CreateTask {
            document_id: Some(document_id.to_string()),
            task_type: TaskType::ParseDocument,
            idempotency_key: idempotency_key.to_string(),
            phase: TaskPhase::Parsing,
        })?;
        match task.status {
            TaskStatus::Running => {
                // 活动任务:重发 parser.request(新令牌;批次幂等,事件丢失可恢复)。
                self.reissue_ticket(&task)?;
                Ok(task)
            }
            TaskStatus::Succeeded => {
                let current = self.db.read({
                    let vid = version_id.to_string();
                    move |conn| load_current_generation(conn, &vid)
                })?;
                let same_complete = current.is_some_and(|g| {
                    g.parser_config_hash == self.parser_config_hash(format)
                        && g.status == GenerationStatus::Complete
                });
                if same_complete {
                    Ok(task)
                } else {
                    let retried = self.tasks.retry_task(&task.id)?;
                    self.launch_attempt(
                        &retried,
                        document_id,
                        version_id,
                        format,
                        force_new_generation,
                        allow_complete_reuse,
                    )
                }
            }
            TaskStatus::Queued => self.launch_attempt(
                &task,
                document_id,
                version_id,
                format,
                force_new_generation,
                allow_complete_reuse,
            ),
            _ => {
                let retried = self.tasks.retry_task(&task.id)?;
                self.launch_attempt(
                    &retried,
                    document_id,
                    version_id,
                    format,
                    force_new_generation,
                    allow_complete_reuse,
                )
            }
        }
    }

    /// `parse_retry`:同一 Task 新 Attempt;同代次重试依赖确定性块 ID 去重。
    pub fn retry_parse(&self, task_id: &str) -> Result<TaskRow, AppError> {
        let tid = task_id.to_string();
        let (document_id, version_id, format) = self.db.read(move |conn| {
            let task: Option<(Option<String>, String)> = conn
                .query_row(
                    "SELECT document_id, task_type FROM task WHERE id=?1",
                    [&tid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let doc_id = task
                .filter(|(_, t)| t.as_str() == "parse_document")
                .and_then(|(d, _)| d)
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let gen =
                load_generation_by_task(conn, &tid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let format: String = conn
                .query_row(
                    "SELECT d.format FROM document d JOIN document_version v \
                     ON v.document_id = d.id WHERE v.id=?1",
                    [&gen.document_version_id],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            Ok((
                doc_id,
                gen.document_version_id,
                DocumentFormat::parse_db(&format).ok_or_else(|| err(CoreErrorCode::Internal))?,
            ))
        })?;
        let retried = self.tasks.retry_task(task_id)?;
        self.launch_attempt(&retried, &document_id, &version_id, format, false, false)
    }

    fn reissue_ticket(&self, task: &TaskRow) -> Result<(), AppError> {
        let tid = task.id.clone();
        let attempt_id = task
            .current_attempt_id
            .clone()
            .ok_or_else(|| err(CoreErrorCode::TaskNotRunning))?;
        let binding = self.db.read(move |conn| {
            let gen = load_generation_by_task(conn, &tid)?
                .filter(|g| {
                    matches!(
                        g.status,
                        GenerationStatus::Parsing | GenerationStatus::Indexing
                    ) && g.current_attempt_id.as_deref() == Some(attempt_id.as_str())
                })
                .ok_or_else(|| err(CoreErrorCode::TaskNotRunning))?;
            let (document_id, format): (String, String) = conn
                .query_row(
                    "SELECT d.id, d.format FROM document d JOIN document_version v \
                     ON v.document_id = d.id WHERE v.id=?1",
                    [&gen.document_version_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|e| db_app_error(&e))?;
            let (path, size) = load_input_source(conn, &document_id)?;
            Ok(ParseJobBinding {
                task_id: tid,
                attempt_id,
                document_id,
                version_id: gen.document_version_id.clone(),
                generation_id: gen.id,
                format: DocumentFormat::parse_db(&format)
                    .ok_or_else(|| err(CoreErrorCode::Internal))?,
                path,
                size,
            })
        })?;
        let (token, expires_at) = self.issue_job_token(&binding);
        self.emit_parser_request(&binding, &token, expires_at);
        Ok(())
    }

    /// 启动新 Attempt:等价复用同配置已完成代次,或创建/复用代次并签发令牌。
    fn launch_attempt(
        &self,
        task: &TaskRow,
        document_id: &str,
        version_id: &str,
        format: DocumentFormat,
        force_new_generation: bool,
        allow_complete_reuse: bool,
    ) -> Result<TaskRow, AppError> {
        let config_hash = self.parser_config_hash(format);

        // 等价复用:当前代次同配置已完成(且不属于本任务)→ 快速提交,
        // 顺带执行 FTS 完整性修复(清索引后等价重建,A9)。
        let current = self.db.read({
            let vid = version_id.to_string();
            move |conn| load_current_generation(conn, &vid)
        })?;
        if let Some(gen) = current {
            if !force_new_generation
                && !allow_complete_reuse
                && gen.parser_config_hash == config_hash
                && gen.status == GenerationStatus::Complete
                && gen.task_id.as_deref() != Some(task.id.as_str())
            {
                let (t, attempt) = self.tasks.begin_attempt(&task.id, None, None)?;
                let blocks = self.db.read({
                    let gid = gen.id.clone();
                    move |conn| block_count(conn, &gid)
                })?;
                let set_hash = gen.block_set_hash.clone().unwrap_or_default();
                let gid = gen.id.clone();
                let outcome = self.tasks.commit_result(
                    &t.id,
                    &attempt.id,
                    serde_json::json!({
                        "generationId": gid,
                        "blockCount": blocks,
                        "blockSetHash": set_hash,
                        "reused": true,
                    }),
                    TaskOutcome::None,
                    None,
                    move |conn, _t| {
                        repair_fts_tx(conn, &gid)?;
                        Ok(())
                    },
                )?;
                return Ok(match outcome {
                    crate::persistence::task::CommitOutcome::Committed(t) => *t,
                    crate::persistence::task::CommitOutcome::AlreadyCommitted(t) => *t,
                });
            }
        }

        // 常规路径:新 Attempt + 代次复用/创建。复用同配置代次时保留
        // page_ocr 与内容块，逐页上报/提交会按 key upsert；只有 parser
        // 配置变化才新建代次（旧代次保留给历史引用）。
        let (task, _attempt) = self.tasks.begin_attempt(&task.id, None, None)?;
        let attempt_id = task
            .current_attempt_id
            .clone()
            .ok_or_else(|| err(CoreErrorCode::Internal))?;
        let now = self.clock.now_ms();
        let new_gen_id = self.ids.new_entity_id();
        let vid = version_id.to_string();
        let tid = task.id.clone();
        let cfg = config_hash.clone();
        let parser_version = self.versions.parser_version.clone();
        // 写闭包会移动 tid/attempt_id;为随后的令牌绑定保留副本。
        let tid_for_binding = tid.clone();
        let attempt_for_binding = attempt_id.clone();

        let generation_id = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            // 暂停信号属于旧 Attempt:新 Attempt 一律从未暂停开始。
            // page_ocr 不清空：已识别页、块与纠错必须跨 Attempt/代次保留。
            tx.execute(
                "DELETE FROM page_ocr_control WHERE document_version_id=?1",
                params![vid],
            )
            .map_err(|e| db_app_error(&e))?;
            let current = load_current_generation(&tx, &vid)?;
            let task_gen = load_generation_by_task(&tx, &tid)?;
            let mut candidates: Vec<GenerationRow> = Vec::new();
            if let Some(g) = current {
                candidates.push(g);
            }
            if let Some(g) = task_gen {
                if candidates.iter().all(|candidate| candidate.id != g.id) {
                    candidates.push(g);
                }
            }
            let candidate = candidates.into_iter().find(|g| {
                g.parser_config_hash == cfg
                    && (g.status != GenerationStatus::Complete || allow_complete_reuse)
            });
            let gen_id = match candidate {
                // 同配置:复用代次(重试/重新识别),保留 page_ocr 与已写入的块。
                Some(g) => {
                    tx.execute(
                        "UPDATE parse_generation SET is_current=0 WHERE document_version_id=?1 \
                         AND is_current=1 AND id != ?2",
                        params![vid, g.id],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    tx.execute(
                        "UPDATE parse_generation SET status='parsing', is_current=1, task_id=?2, \
                         current_attempt_id=?3, last_batch_seq=0, error_code=NULL, started_at=?4, \
                         completed_at=NULL WHERE id=?1",
                        params![g.id, tid, attempt_id, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    g.id
                }
                // 配置变化或首次解析:新建代次;旧代次降级为非当前。
                None => {
                    tx.execute(
                        "UPDATE parse_generation SET is_current=0 WHERE document_version_id=?1 \
                         AND is_current=1",
                        params![vid],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    tx.execute(
                        "INSERT INTO parse_generation(id, document_version_id, \
                         parser_config_hash, parser_version, status, is_current, task_id, \
                         current_attempt_id, last_batch_seq, started_at) \
                         VALUES (?1,?2,?3,?4,'parsing',1,?5,?6,0,?7)",
                        params![new_gen_id, vid, cfg, parser_version, tid, attempt_id, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    new_gen_id
                }
            };
            let gen = load_generation(&tx, &gen_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(gen.id)
        })?;

        let binding = self.db.read({
            let did = document_id.to_string();
            let gid = generation_id.clone();
            let tid2 = tid_for_binding;
            let attempt2 = attempt_for_binding;
            let vid2 = version_id.to_string();
            move |conn| {
                let (path, size) = load_input_source(conn, &did)?;
                Ok(ParseJobBinding {
                    task_id: tid2,
                    attempt_id: attempt2,
                    document_id: did,
                    version_id: vid2,
                    generation_id: gid,
                    format,
                    path,
                    size,
                })
            }
        })?;
        let (token, expires_at) = self.issue_job_token(&binding);
        self.emit_parser_request(&binding, &token, expires_at);
        self.emit_progress(&binding, "parsing", 0, 0, 0);
        self.tasks
            .get_task(&binding.task_id)?
            .ok_or_else(|| err(CoreErrorCode::Internal))
    }

    /// `parser_read_input`:令牌门控的受控分块读取(《02》§5)。
    pub fn read_input(
        &self,
        token: &str,
        offset: i64,
        length: i64,
    ) -> Result<crate::shared::dto::BinaryChunk, AppError> {
        let binding = self.validate_job_token(token)?;
        if offset < 0 {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        if offset >= binding.size {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "offset_beyond_eof".into()));
        }
        // 超过单次上限截断到上限;再与 EOF 取交集。
        let length = length
            .clamp(0, MAX_READ_CHUNK_BYTES)
            .min(binding.size - offset);
        let mut buf = vec![0u8; length as usize];
        if length > 0 {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(&binding.path)
                .map_err(|_| err(CoreErrorCode::DocFileMissing))?;
            f.seek(SeekFrom::Start(offset as u64))
                .and_then(|_| f.read_exact(&mut buf))
                .map_err(|_| err(CoreErrorCode::DocFileMissing))?;
        }
        Ok(crate::shared::dto::BinaryChunk {
            offset,
            length,
            data_base64: crate::shared::dto::base64_encode(&buf),
            eof: offset + length >= binding.size,
        })
    }

    /// `parser_submit_batch`:一个事务写入块、区域、FTS 并推进批次序号。
    /// 取消/旧 Attempt/越界批次一律零写入(M04-A8、API-05)。
    pub fn submit_batch(&self, token: &str, batch: ParserBatchInput) -> Result<BatchAck, AppError> {
        let binding = self.validate_job_token(token)?;
        if batch.blocks.len() > MAX_BLOCKS_PER_BATCH {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("maxBlocks", (MAX_BLOCKS_PER_BATCH as i64).into()));
        }
        let text_bytes: usize = batch.blocks.iter().map(|b| b.text.len()).sum();
        if text_bytes > MAX_BATCH_TEXT_BYTES {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("maxBytes", (MAX_BATCH_TEXT_BYTES as i64).into()));
        }
        if batch.batch_seq < 1 {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        // 纯输入校验先于事务:定位格式与格式必须匹配。
        for b in &batch.blocks {
            if !locator_matches_format(&b.locator, binding.format) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "locator_format_mismatch".into()));
            }
        }
        for u in &batch.unsupported_regions {
            if !locator_matches_format(&u.locator, binding.format) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "locator_format_mismatch".into()));
            }
        }

        let now = self.clock.now_ms();
        let ids = Arc::clone(&self.ids);
        let binding2 = binding.clone();
        let ack = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            // 任务状态守卫:running + 当前 Attempt + 未取消(零写入路径)。
            let task: Option<(Option<String>, i64)> = tx
                .query_row(
                    "SELECT current_attempt_id, cancel_requested FROM task WHERE id=?1",
                    [&binding2.task_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let (cur_attempt, cancel_requested) =
                task.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if cancel_requested != 0 {
                return Err(err(CoreErrorCode::TaskCanceled));
            }
            if cur_attempt.as_deref() != Some(binding2.attempt_id.as_str()) {
                return Err(err(CoreErrorCode::TaskNotRunning));
            }
            let gen = load_generation(&tx, &binding2.generation_id)?
                .ok_or_else(|| err(CoreErrorCode::IndexGenerationUnavailable))?;
            // 已完成代次不可追加(《01》§10)。
            if !matches!(
                gen.status,
                GenerationStatus::Parsing | GenerationStatus::Indexing
            ) {
                return Err(err(CoreErrorCode::TaskNotRunning));
            }
            if gen.current_attempt_id.as_deref() != Some(binding2.attempt_id.as_str()) {
                return Err(err(CoreErrorCode::TaskNotRunning));
            }
            if batch.batch_seq != gen.last_batch_seq + 1 {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("expectedSeq", (gen.last_batch_seq + 1).into())
                    .with_detail("gotSeq", batch.batch_seq.into()));
            }

            for b in &batch.blocks {
                if let Some(confidence) = b.ocr_confidence {
                    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                        return Err(err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "ocr_confidence_out_of_range".into()));
                    }
                }
                if b.origin == BlockOrigin::Ocr && binding2.format != DocumentFormat::Pdf {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "ocr_block_requires_pdf".into()));
                }
            }

            let mut accepted: i64 = 0;
            let mut resolved_keys: HashMap<String, String> = HashMap::new();
            for b in &batch.blocks {
                let block_id = ids.parser_block_id(&binding2.generation_id, &b.block_key);
                // 父块解析:本批更早块或既有块。
                let parent_id = match &b.parent_block_key {
                    Some(pk) => {
                        let pid = match resolved_keys.get(pk) {
                            Some(id) => Some(id.clone()),
                            None => load_block_by_key(&tx, &binding2.generation_id, pk)?
                                .map(|r| r.id),
                        };
                        Some(pid.ok_or_else(|| {
                            err(CoreErrorCode::InvalidRequest)
                                .with_detail("reason", "parent_block_not_found".into())
                        })?)
                    }
                    None => None,
                };
                let existing = load_block_by_key(&tx, &binding2.generation_id, &b.block_key)?;
                let normalized_text = normalize_v1(&b.text).normalized;
                let text_hash = sha256_str(&b.text);
                match existing {
                    Some(row) => {
                        // OCR 是派生文本:同一 block_key 的重跑允许结果变化，
                        // 保留块 ID（旧引用仍可解析）并原地更新索引。
                        // 解析器文本层块仍要求确定性一致，防止重试漂移。
                        if b.origin == BlockOrigin::Ocr && row.origin == BlockOrigin::Ocr {
                            let ordinal_taken: bool = tx
                                .query_row(
                                    "SELECT EXISTS(SELECT 1 FROM content_block \
                                     WHERE parse_generation_id=?1 AND ordinal=?2 AND id<>?3)",
                                    params![binding2.generation_id, b.ordinal, row.id],
                                    |r| r.get(0),
                                )
                                .map_err(|e| db_app_error(&e))?;
                            if !ordinal_taken {
                                tx.execute(
                                    "UPDATE content_block SET ordinal=?2 WHERE id=?1",
                                    params![row.id, b.ordinal],
                                )
                                .map_err(|e| db_app_error(&e))?;
                            }
                            tx.execute(
                                "UPDATE content_block SET text=?2, normalized_text=?3, \
                                 text_hash=?4, normalized_text_hash=?5, heading_path_json=?6, \
                                 locator_json=?7, kind=?8, ocr_confidence=?9, indexable=?10 \
                                 WHERE id=?1",
                                params![
                                    row.id,
                                    b.text,
                                    normalized_text,
                                    text_hash,
                                    sha256_str(&normalized_text),
                                    serde_json::to_string(&b.heading_path)
                                        .unwrap_or_else(|_| "[]".into()),
                                    serde_json::to_string(&b.locator).unwrap_or_default(),
                                    b.kind.as_str(),
                                    b.ocr_confidence,
                                    b.indexable as i64,
                                ],
                            )
                            .map_err(|e| db_app_error(&e))?;
                            tx.execute(
                                "DELETE FROM content_block_fts WHERE block_id=?1",
                                [&row.id],
                            )
                            .map_err(|e| db_app_error(&e))?;
                            if b.indexable {
                                tx.execute(
                                    "INSERT INTO content_block_fts(text, block_id, parse_generation_id) \
                                     VALUES (?1,?2,?3)",
                                    params![normalized_text, row.id, binding2.generation_id],
                                )
                                .map_err(|e| db_app_error(&e))?;
                            }
                            resolved_keys.insert(b.block_key.clone(), row.id);
                            accepted += 1;
                        } else {
                            // 同 key 重提(重试路径):内容必须一致,否则拒绝。
                            if row.ordinal != Some(b.ordinal) || row.text != b.text {
                                return Err(err(CoreErrorCode::InvalidRequest)
                                    .with_detail("reason", "block_key_drift".into()));
                            }
                            resolved_keys.insert(b.block_key.clone(), row.id);
                        }
                    }
                    None => {
                        // OCR 块数量可能随重跑变化;若目标 ordinal 已被占用,
                        // 放到高段位保留页内顺序，避免与后续 parser 批次的
                        // 连续 ordinal 冲突，而不是让整批解析失败。
                        let insert_ordinal = if b.origin == BlockOrigin::Ocr {
                            let taken: bool = tx
                                .query_row(
                                    "SELECT EXISTS(SELECT 1 FROM content_block \
                                     WHERE parse_generation_id=?1 AND ordinal=?2)",
                                    params![binding2.generation_id, b.ordinal],
                                    |r| r.get(0),
                                )
                                .map_err(|e| db_app_error(&e))?;
                            if taken {
                                1_000_000_000i64.saturating_add(b.ordinal)
                            } else {
                                b.ordinal
                            }
                        } else {
                            b.ordinal
                        };
                        tx.execute(
                            "INSERT INTO content_block(id, document_version_id, \
                             parse_generation_id, origin, block_key, ordinal, parent_block_id, \
                             kind, text, normalized_text, text_hash, normalized_text_hash, \
                             heading_path_json, locator_json, completeness, indexable, \
                             normalization_version, ocr_confidence, created_at) \
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                            params![
                                block_id,
                                binding2.version_id,
                                binding2.generation_id,
                                b.origin.as_str(),
                                b.block_key,
                                insert_ordinal,
                                parent_id,
                                b.kind.as_str(),
                                b.text,
                                normalized_text,
                                text_hash,
                                sha256_str(&normalized_text),
                                serde_json::to_string(&b.heading_path)
                                    .unwrap_or_else(|_| "[]".into()),
                                serde_json::to_string(&b.locator).unwrap_or_default(),
                                match b.completeness {
                                    BlockCompleteness::Complete => "complete",
                                    BlockCompleteness::Partial => "partial",
                                    BlockCompleteness::Restricted => "restricted",
                                },
                                b.indexable as i64,
                                NORMALIZATION_VERSION,
                                b.ocr_confidence,
                                now,
                            ],
                        )
                        .map_err(|e| db_app_error(&e))?;
                        if b.indexable {
                            tx.execute(
                                "INSERT INTO content_block_fts(text, block_id, parse_generation_id) \
                                 VALUES (?1,?2,?3)",
                                params![normalized_text, block_id, binding2.generation_id],
                            )
                            .map_err(|e| db_app_error(&e))?;
                        }
                        resolved_keys.insert(b.block_key.clone(), block_id);
                        accepted += 1;
                    }
                }
            }

            for r in &batch.regions {
                tx.execute(
                    "INSERT INTO parse_region(generation_id, region_key, region_kind, status, \
                     block_count, order_confidence, error_code, updated_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8) \
                     ON CONFLICT(generation_id, region_key) DO UPDATE SET \
                     status=excluded.status, block_count=excluded.block_count, \
                     order_confidence=excluded.order_confidence, error_code=excluded.error_code, \
                     updated_at=excluded.updated_at",
                    params![
                        binding2.generation_id,
                        r.region_key,
                        match r.region_kind {
                            RegionKind::Page => "page",
                            RegionKind::Body => "body",
                            RegionKind::Heading => "heading",
                            RegionKind::Object => "object",
                        },
                        match r.status {
                            RegionStatus::Pending => "pending",
                            RegionStatus::Partial => "partial",
                            RegionStatus::Complete => "complete",
                            RegionStatus::Failed => "failed",
                            RegionStatus::NoTextLayer => "no_text_layer",
                        },
                        r.block_count,
                        match r.order_confidence {
                            OrderConfidence::High => "high",
                            OrderConfidence::Low => "low",
                        },
                        r.error_code,
                        now,
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
            }

            for u in &batch.unsupported_regions {
                tx.execute(
                    "INSERT OR IGNORE INTO unsupported_region(generation_id, region_key, \
                     object_type, locator_json, reason_code, created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        binding2.generation_id,
                        u.region_key,
                        u.object_type,
                        serde_json::to_string(&u.locator).unwrap_or_default(),
                        u.reason_code,
                        now,
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
            }

            tx.execute(
                "UPDATE parse_generation SET last_batch_seq=?2 WHERE id=?1",
                params![binding2.generation_id, batch.batch_seq],
            )
            .map_err(|e| db_app_error(&e))?;
            let total = block_count(&tx, &binding2.generation_id)?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(BatchAck {
                accepted_blocks: accepted,
                total_blocks: total,
                last_batch_seq: batch.batch_seq,
            })
        })?;

        // 事件仅在 COMMIT 后发出(《01》§9)。
        let (done, total_regions) = self.region_done_total(&binding.generation_id)?;
        self.emit_progress(&binding, "parsing", ack.total_blocks, done, total_regions);
        Ok(ack)
    }

    fn region_done_total(&self, generation_id: &str) -> Result<(i64, i64), AppError> {
        let gid = generation_id.to_string();
        self.db.read(move |conn| {
            conn.query_row(
                "SELECT \
                   SUM(CASE WHEN status IN ('complete','no_text_layer') THEN 1 ELSE 0 END), \
                   COUNT(*) \
                 FROM parse_region WHERE generation_id=?1",
                [&gid],
                |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                        r.get::<_, i64>(1)?,
                    ))
                },
            )
            .map_err(|e| db_app_error(&e))
        })
    }

    /// `parser_complete`:覆盖审计 + BlockSetHash + FTS 完整性检查,
    /// 与任务结果提交同一事务(《M04》§3)。
    pub fn complete_parse(&self, token: &str) -> Result<TaskRow, AppError> {
        let binding = self.validate_job_token(token)?;
        let now = self.clock.now_ms();

        // 预读覆盖,决定任务 outcome;闭包在事务内复核并落代次终态。
        let (complete_c, partial_c, failed_c, _no_text_c, total) = self.db.read({
            let gid = binding.generation_id.clone();
            move |conn| region_stats(conn, &gid)
        })?;
        let outcome = if failed_c > 0 || partial_c > 0 {
            TaskOutcome::Partial
        } else {
            TaskOutcome::None
        };

        let binding2 = binding.clone();
        let gid_for_json = binding.generation_id.clone();
        let blocks_pre = self.db.read({
            let gid = binding.generation_id.clone();
            move |conn| block_count(conn, &gid)
        })?;
        let result = self.tasks.commit_result(
            &binding.task_id,
            &binding.attempt_id,
            serde_json::json!({
                "generationId": gid_for_json,
                "blockCount": blocks_pre,
                "regionsTotal": total,
                "regionsComplete": complete_c,
            }),
            outcome,
            None,
            move |conn, _t| {
                let gen = load_generation(conn, &binding2.generation_id)?
                    .ok_or_else(|| err(CoreErrorCode::IndexGenerationUnavailable))?;
                if !matches!(
                    gen.status,
                    GenerationStatus::Parsing | GenerationStatus::Indexing
                ) {
                    return Err(err(CoreErrorCode::TaskNotRunning));
                }
                // FTS 完整性检查:缺失即修复(可重建缓存,A9)。
                repair_fts_tx(conn, &binding2.generation_id)?;
                let set_hash = compute_block_set_hash(conn, &binding2.generation_id)?;
                // 覆盖审计(《M04》§8):全部区域失败 → failed;否则部分/完整。
                let (_c, p, f, _nt, t) = region_stats(conn, &binding2.generation_id)?;
                let final_status = if t > 0 && f == t {
                    GenerationStatus::Failed
                } else if f > 0 || p > 0 {
                    GenerationStatus::Partial
                } else {
                    GenerationStatus::Complete
                };
                conn.execute(
                    "UPDATE parse_generation SET status=?2, block_set_hash=?3, completed_at=?4, \
                     error_code=?5 WHERE id=?1",
                    params![
                        binding2.generation_id,
                        final_status.as_str(),
                        set_hash,
                        now,
                        if final_status == GenerationStatus::Failed {
                            Some("PARSE_PARTIAL".to_string())
                        } else {
                            None
                        },
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
                clear_ocr_pause_tx(conn, &binding2.version_id, now)?;
                Ok(())
            },
        )?;

        // 事件在 COMMIT 后。
        let blocks = self.db.read({
            let gid = binding.generation_id.clone();
            move |conn| block_count(conn, &gid)
        })?;
        let (done, total_regions) = self.region_done_total(&binding.generation_id)?;
        self.emit_progress(&binding, "complete", blocks, done, total_regions);
        self.job_tokens.revoke(token);
        Ok(match result {
            crate::persistence::task::CommitOutcome::Committed(t) => *t,
            crate::persistence::task::CommitOutcome::AlreadyCommitted(t) => *t,
        })
    }

    /// `parse_cancel`:请求取消并确认。批次驱动的执行器在写线程串行化下
    /// 可以安全立即确认:取消标记落库后的任何批次提交都会被守卫拒绝。
    pub fn cancel_parse(
        &self,
        task_id: &str,
        expected_state_version: i64,
    ) -> Result<TaskRow, AppError> {
        let task = self.tasks.request_cancel(task_id, expected_state_version)?;
        let task = if task.status == TaskStatus::Running {
            match task.current_attempt_id.clone() {
                Some(attempt) => self.tasks.cancel_ack(task_id, &attempt)?,
                None => task,
            }
        } else {
            task
        };
        // 令牌保留到自然过期:Worker 随后的批次提交会得到精确的
        // TASK_CANCELED(而非令牌失效),便于其正确终止协议。
        let now = self.clock.now_ms();
        let tid2 = task_id.to_string();
        let _ = self.db.write(move |conn| {
            conn.execute(
                "UPDATE parse_generation SET status='canceled', completed_at=?2 \
                 WHERE task_id=?1 AND status IN ('parsing','indexing')",
                params![tid2, now],
            )
            .map_err(|e| db_app_error(&e))?;
            conn.execute(
                "UPDATE page_ocr_control SET paused=0, paused_at=NULL, updated_at=?2 \
                 WHERE document_version_id IN \
                   (SELECT document_version_id FROM parse_generation WHERE task_id=?1)",
                params![tid2, now],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        });
        Ok(task)
    }

    /// `parser_fail`:Worker 报告不可恢复失败;代次标记 failed(不覆盖终态)。
    pub fn fail_parse(&self, token: &str, error_code: &str) -> Result<TaskRow, AppError> {
        let binding = self.validate_job_token(token)?;
        // 先经任务状态机失败,再回落代次状态(崩溃间隙由恢复例程兜底)。
        let task = self
            .tasks
            .fail_task(&binding.task_id, &binding.attempt_id, error_code)?;
        let gid = binding.generation_id.clone();
        let vid = binding.version_id.clone();
        let code = error_code.to_string();
        let now = self.clock.now_ms();
        let _ = self.db.write(move |conn| {
            conn.execute(
                "UPDATE parse_generation SET status='failed', error_code=?2, completed_at=?3 \
                 WHERE id=?1 AND status IN ('parsing','indexing')",
                params![gid, code, now],
            )
            .map_err(|e| db_app_error(&e))?;
            clear_ocr_pause_tx(conn, &vid, now)?;
            Ok(())
        });
        self.emit_progress(&binding, "failed", 0, 0, 0);
        self.job_tokens.revoke(token);
        Ok(task)
    }

    /// 按需选区块(《M04》§7):同选区重复请求返回相同块;不进 FTS/向量。
    pub fn ensure_selection_blocks(
        &self,
        req: &SelectionBlockRequest,
    ) -> Result<ContentBlockView, AppError> {
        let format = self.validate_version(&req.document_id, &req.version_id)?;
        if !locator_matches_format(&req.locator, format) {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "locator_format_mismatch".into()));
        }
        if req.text.trim().is_empty() {
            return Err(
                err(CoreErrorCode::InvalidRequest).with_detail("reason", "empty_selection".into())
            );
        }
        let normalized = normalize_v1(&req.text).normalized;
        let norm_hash = sha256_str(&normalized);
        let canonical = req.locator.canonical_string();
        let block_id = self
            .ids
            .on_demand_block_id(&req.version_id, &norm_hash, &canonical);
        let now = self.clock.now_ms();
        let row = {
            let block_id = block_id.clone();
            let req = req.clone();
            let norm_hash2 = norm_hash.clone();
            let normalized2 = normalized.clone();
            self.db.write(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO content_block(id, document_version_id, \
                     parse_generation_id, origin, block_key, ordinal, parent_block_id, kind, \
                     text, normalized_text, text_hash, normalized_text_hash, heading_path_json, \
                     locator_json, completeness, indexable, normalization_version, created_at) \
                     VALUES (?1,?2,NULL,'on_demand',?3,NULL,NULL,'selection',?4,?5,?6,?7,?8,?9,\
                     'complete',0,?10,?11)",
                    params![
                        block_id,
                        req.version_id,
                        format!("on_demand:{}", &norm_hash2[..16.min(norm_hash2.len())]),
                        req.text,
                        normalized2,
                        sha256_str(&req.text),
                        norm_hash2,
                        serde_json::to_string(&req.heading_path).unwrap_or_else(|_| "[]".into()),
                        serde_json::to_string(&req.locator).unwrap_or_default(),
                        NORMALIZATION_VERSION,
                        now,
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
                load_block(conn, &block_id)?.ok_or_else(|| err(CoreErrorCode::Internal))
            })?
        };
        row.to_view().ok_or_else(|| err(CoreErrorCode::Internal))
    }

    /// `parse_get_status`:覆盖与能力视图(《M04》§8)。
    pub fn get_status(&self, document_id: &str) -> Result<ParseStatusView, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            let format: Option<String> = conn
                .query_row("SELECT format FROM document WHERE id=?1", [&did], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let format =
                DocumentFormat::parse_db(&format.ok_or_else(|| err(CoreErrorCode::DocNotFound))?)
                    .ok_or_else(|| err(CoreErrorCode::Internal))?;
            let version_id: Option<String> = conn
                .query_row(
                    "SELECT current_version_id FROM document WHERE id=?1",
                    [&did],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            let gen = match &version_id {
                Some(v) => load_current_generation(conn, v)?,
                None => None,
            };

            let mut completed = Vec::new();
            let mut partial = Vec::new();
            let mut failed: Vec<RegionStatusView> = Vec::new();
            let mut order_warnings = Vec::new();
            let mut no_text_all = false;
            let mut has_blocks = false;
            if let Some(g) = &gen {
                let mut stmt = conn
                    .prepare(
                        "SELECT region_key, region_kind, status, block_count, order_confidence, \
                         error_code FROM parse_region WHERE generation_id=?1 ORDER BY region_key",
                    )
                    .map_err(|e| db_app_error(&e))?;
                let regions: Vec<(String, String, String, i64, String, Option<String>)> = stmt
                    .query_map([&g.id], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    })
                    .map_err(|e| db_app_error(&e))?
                    .collect::<Result<_, _>>()
                    .map_err(|e| db_app_error(&e))?;
                drop(stmt);
                let mut no_text_count = 0;
                for (key, kind, status, count, confidence, code) in &regions {
                    match status.as_str() {
                        "complete" => completed.push(key.clone()),
                        "partial" => partial.push(key.clone()),
                        "no_text_layer" => {
                            no_text_count += 1;
                            failed.push(RegionStatusView {
                                region_key: key.clone(),
                                region_kind: parse_region_kind(kind),
                                status: RegionStatus::NoTextLayer,
                                block_count: *count,
                                order_confidence: parse_confidence(confidence),
                                error_code: Some("PARSE_NO_TEXT_LAYER".to_string()),
                            });
                        }
                        "failed" => failed.push(RegionStatusView {
                            region_key: key.clone(),
                            region_kind: parse_region_kind(kind),
                            status: RegionStatus::Failed,
                            block_count: *count,
                            order_confidence: parse_confidence(confidence),
                            error_code: code.clone(),
                        }),
                        _ => {}
                    }
                    if confidence == "low" {
                        order_warnings.push(key.clone());
                    }
                    if *count > 0 {
                        has_blocks = true;
                    }
                }
                no_text_all = !regions.is_empty() && no_text_count == regions.len() as i64;
                if !has_blocks {
                    has_blocks = block_count(conn, &g.id)? > 0;
                }
            }

            let unsupported = match &gen {
                Some(g) => list_unsupported_for_generation(conn, &g.id)?,
                None => Vec::new(),
            };

            let has_text_layer = match (&gen, format) {
                (Some(_), DocumentFormat::Pdf) => Some(!no_text_all && has_blocks),
                _ => None,
            };

            let (parse_state, fts_state) = match &gen {
                None => (ParseState::NotStarted, FtsState::NotBuilt),
                Some(g) => {
                    let parse = match g.status {
                        GenerationStatus::Queued => ParseState::NotStarted,
                        GenerationStatus::Parsing | GenerationStatus::Indexing => {
                            ParseState::InProgress
                        }
                        GenerationStatus::Complete => ParseState::Complete,
                        GenerationStatus::Partial => ParseState::Partial,
                        GenerationStatus::Failed | GenerationStatus::Canceled => ParseState::Failed,
                    };
                    let fts = match g.status {
                        GenerationStatus::Parsing | GenerationStatus::Indexing => {
                            FtsState::Building
                        }
                        GenerationStatus::Complete | GenerationStatus::Partial => {
                            let blocks: i64 = conn
                                .query_row(
                                    "SELECT COUNT(*) FROM content_block \
                                     WHERE parse_generation_id=?1 AND indexable=1",
                                    [&g.id],
                                    |r| r.get(0),
                                )
                                .map_err(|e| db_app_error(&e))?;
                            let rows = fts_row_count(conn, &g.id)?;
                            if blocks == 0 {
                                FtsState::NotBuilt
                            } else if rows >= blocks {
                                FtsState::Complete
                            } else {
                                FtsState::Partial
                            }
                        }
                        _ => FtsState::NotBuilt,
                    };
                    (parse, fts)
                }
            };

            // 向量代次:当前解析代次的最新嵌入代次;仅旧代次有向量 → Stale。
            let vector_state = match &gen {
                None => VectorState::NotConfigured,
                Some(g) => {
                    let latest: Option<String> = conn
                        .query_row(
                            "SELECT status FROM embedding_generation \
                             WHERE parse_generation_id=?1 ORDER BY created_at DESC LIMIT 1",
                            [&g.id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(|e| db_app_error(&e))?;
                    match latest {
                        None => {
                            let older: i64 = conn
                                .query_row(
                                    "SELECT COUNT(*) FROM embedding_generation eg \
                                     JOIN parse_generation pg ON pg.id = eg.parse_generation_id \
                                     WHERE pg.document_version_id=?1 AND pg.id != ?2",
                                    params![g.document_version_id, g.id],
                                    |r| r.get(0),
                                )
                                .map_err(|e| db_app_error(&e))?;
                            if older > 0 {
                                VectorState::Stale
                            } else {
                                VectorState::NotConfigured
                            }
                        }
                        Some(status) => match status.as_str() {
                            "building" => VectorState::Building,
                            "complete" => VectorState::Complete,
                            "partial" => VectorState::Partial,
                            "failed" => VectorState::Failed,
                            _ => VectorState::Stale,
                        },
                    }
                }
            };

            let capability = match (&gen, parse_state) {
                (None, _) => Capability::ViewOnly,
                (Some(_), ParseState::Failed) => Capability::Failed,
                (Some(_), ParseState::InProgress) => {
                    if has_blocks {
                        Capability::PartialQa
                    } else {
                        Capability::ViewOnly
                    }
                }
                (Some(_), ParseState::NotStarted) => Capability::ViewOnly,
                (Some(_), ParseState::Complete | ParseState::Partial) => {
                    if has_text_layer == Some(false) {
                        // 扫描件:不产生伪文本,仅选区按需解析问答(A5)。
                        Capability::SelectionQa
                    } else if parse_state == ParseState::Partial {
                        Capability::PartialQa
                    } else if vector_state == VectorState::Complete {
                        Capability::HybridFull
                    } else {
                        Capability::FtsOnly
                    }
                }
            };

            Ok(ParseStatusView {
                document_id: did,
                version_id: version_id.unwrap_or_default(),
                generation_id: gen.as_ref().map(|g| g.id.clone()),
                parse_state,
                coverage: CoverageView {
                    completed_regions: completed,
                    partial_regions: partial,
                    failed_regions: failed,
                    unsupported_regions: unsupported,
                    has_text_layer,
                    order_warnings,
                },
                fts_state,
                vector_state,
                capability,
                active_task_id: gen
                    .as_ref()
                    .filter(|g| {
                        matches!(
                            g.status,
                            GenerationStatus::Queued
                                | GenerationStatus::Parsing
                                | GenerationStatus::Indexing
                        )
                    })
                    .and_then(|g| g.task_id.clone()),
            })
        })
    }

    // —— 2D 页级 OCR 状态 ——

    /// Worker 页状态上报。只接受当前解析任务/Attempt 的 PDF 令牌;
    /// 状态与解析块同属一个代次,事件仅在 COMMIT 后发出。
    pub fn report_page_ocr(
        &self,
        token: &str,
        entries: &[PageOcrReportEntry],
    ) -> Result<(), AppError> {
        if entries.is_empty() || entries.len() > MAX_PAGE_OCR_REPORTS {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "page_ocr_report_size".into()));
        }
        let binding = self.validate_job_token(token)?;
        if binding.format != DocumentFormat::Pdf {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "page_ocr_requires_pdf".into()));
        }
        for entry in entries {
            if entry.page_number < 1 || entry.page_number > 100_000 {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "page_number_out_of_range".into()));
            }
            if entry.block_count < 0 || entry.block_count > 100_000 {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "block_count_out_of_range".into()));
            }
            if let Some(confidence) = entry.confidence {
                if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "ocr_confidence_out_of_range".into()));
                }
            }
            if entry.reasons.len() > 16
                || entry.reasons.iter().any(|r| r.chars().count() > 80)
                || entry.render_hash.as_ref().is_some_and(|h| h.len() > 128)
                || entry.engine_version.as_ref().is_some_and(|v| v.len() > 64)
                || entry.error_code.as_ref().is_some_and(|c| c.len() > 64)
            {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "page_ocr_report_field_too_long".into()));
            }
            if let Some(quality) = &entry.quality {
                let size = serde_json::to_string(quality).map(|s| s.len()).unwrap_or(0);
                if size > 4096 {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "page_ocr_quality_too_large".into()));
                }
            }
        }

        let now = self.clock.now_ms();
        let binding2 = binding.clone();
        let entries2 = entries.to_vec();
        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let task: Option<(Option<String>, i64)> = tx
                .query_row(
                    "SELECT current_attempt_id, cancel_requested FROM task WHERE id=?1",
                    [&binding2.task_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let (cur_attempt, cancel_requested) =
                task.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if cancel_requested != 0 {
                return Err(err(CoreErrorCode::TaskCanceled));
            }
            if cur_attempt.as_deref() != Some(binding2.attempt_id.as_str()) {
                return Err(err(CoreErrorCode::TaskNotRunning));
            }
            let gen = load_generation(&tx, &binding2.generation_id)?
                .ok_or_else(|| err(CoreErrorCode::IndexGenerationUnavailable))?;
            if !matches!(
                gen.status,
                GenerationStatus::Parsing | GenerationStatus::Indexing
            ) || gen.current_attempt_id.as_deref() != Some(binding2.attempt_id.as_str())
            {
                return Err(err(CoreErrorCode::TaskNotRunning));
            }

            for entry in &entries2 {
                // 已完成的页不能被“本次排队/文本层跳过”降级为 pending/skipped:
                // 范围识别会先上报所有目标页,取消或失败时旧结果必须仍然可见。
                let existing: Option<(String, i64, Option<String>)> = tx
                    .query_row(
                        "SELECT status, block_count, parse_generation_id FROM page_ocr \
                         WHERE document_version_id=?1 AND page_number=?2",
                        params![binding2.version_id, entry.page_number],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()
                    .map_err(|e| db_app_error(&e))?;
                if matches!(
                    entry.status,
                    PageOcrStatus::Pending | PageOcrStatus::Skipped
                ) && existing.as_ref().is_some_and(|(status, blocks, generation)| {
                    matches!(status.as_str(), "completed" | "low_confidence")
                        && *blocks > 0
                        && generation.as_deref() == Some(binding2.generation_id.as_str())
                }) {
                    continue;
                }
                let quality_json = entry
                    .quality
                    .as_ref()
                    .map(|q| serde_json::to_string(q).unwrap_or_else(|_| "null".into()));
                let reasons_json = serde_json::to_string(&entry.reasons)
                    .unwrap_or_else(|_| "[]".into());
                let completed_at = if entry.status.is_terminal() {
                    Some(now)
                } else {
                    None
                };
                tx.execute(
                    "INSERT INTO page_ocr(document_version_id, page_number, parse_generation_id, \
                     status, quality_json, reasons_json, render_hash, engine_version, confidence, \
                     block_count, error_code, attempt, updated_at, completed_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11, \
                       CASE WHEN ?4='ocr_running' THEN 1 ELSE 0 END,?12,?13) \
                     ON CONFLICT(document_version_id, page_number) DO UPDATE SET \
                       parse_generation_id=excluded.parse_generation_id, \
                       status=excluded.status, quality_json=excluded.quality_json, \
                       reasons_json=excluded.reasons_json, \
                       render_hash=COALESCE(excluded.render_hash, page_ocr.render_hash), \
                       engine_version=COALESCE(excluded.engine_version, page_ocr.engine_version), \
                       confidence=excluded.confidence, block_count=excluded.block_count, \
                       error_code=excluded.error_code, \
                       attempt=page_ocr.attempt + CASE WHEN excluded.status='ocr_running' THEN 1 ELSE 0 END, \
                       updated_at=excluded.updated_at, \
                       completed_at=CASE WHEN excluded.status IN \
                         ('completed','low_confidence','failed','skipped') \
                         THEN excluded.completed_at ELSE page_ocr.completed_at END",
                    params![
                        binding2.version_id,
                        entry.page_number,
                        binding2.generation_id,
                        entry.status.as_str(),
                        quality_json,
                        reasons_json,
                        entry.render_hash,
                        entry.engine_version,
                        entry.confidence,
                        entry.block_count,
                        entry.error_code,
                        now,
                        completed_at,
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(())
        })?;

        // 事件只在 COMMIT 成功后发出;只含状态,不含页图/正文。
        // aggregate 按页;版本用单调时间 + 批内序号,避免同毫秒被判 stale。
        for (index, entry) in entries.iter().enumerate() {
            let event = DomainEvent::new(
                DomainEventType::OcrProgress,
                format!("page_ocr:{}:{}", binding.version_id, entry.page_number),
                now + index as i64,
                now,
                serde_json::json!({
                    "generationId": binding.generation_id,
                    "versionId": binding.version_id,
                    "pageNumber": entry.page_number,
                    "status": entry.status.as_str(),
                    "confidence": entry.confidence,
                    "blockCount": entry.block_count,
                    "errorCode": entry.error_code,
                    "reasons": entry.reasons,
                }),
            )
            .with_document(
                Some(binding.document_id.clone()),
                Some(binding.version_id.clone()),
            )
            .with_task(Some(binding.task_id.clone()));
            self.events.emit(event);
        }
        Ok(())
    }

    /// 读取某版本的页级 OCR 状态(UI 页角徽标/覆盖率)。
    pub fn get_page_ocr_status(
        &self,
        document_id: &str,
        version_id: &str,
    ) -> Result<Vec<PageOcrStatusView>, AppError> {
        let did = document_id.to_string();
        let vid = version_id.to_string();
        self.db.read(move |conn| {
            let owner: Option<String> = conn
                .query_row(
                    "SELECT document_id FROM document_version WHERE id=?1",
                    [&vid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if owner.as_deref() != Some(did.as_str()) {
                return Err(err(CoreErrorCode::NotFound));
            }
            let mut stmt = conn
                .prepare(
                    "SELECT page_number, status, confidence, block_count, error_code, \
                     reasons_json, engine_version, parse_generation_id, updated_at, completed_at \
                     FROM page_ocr WHERE document_version_id=?1 ORDER BY page_number",
                )
                .map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map([&vid], |r| {
                    let status: String = r.get(1)?;
                    let reasons_json: String = r.get(5)?;
                    Ok(PageOcrStatusView {
                        page_number: r.get(0)?,
                        status: PageOcrStatus::parse_db(&status).unwrap_or(PageOcrStatus::Failed),
                        confidence: r.get(2)?,
                        block_count: r.get(3)?,
                        error_code: r.get(4)?,
                        reasons: serde_json::from_str(&reasons_json).unwrap_or_default(),
                        engine_version: r.get(6)?,
                        generation_id: r.get(7)?,
                        updated_at: r.get(8)?,
                        completed_at: r.get(9)?,
                    })
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(rows)
        })
    }

    /// 读取页级 OCR 暂停信号(无记录 = 未暂停)。
    pub fn get_page_ocr_control(
        &self,
        document_id: &str,
        version_id: &str,
    ) -> Result<PageOcrControlView, AppError> {
        let did = document_id.to_string();
        let vid = version_id.to_string();
        self.db.read(move |conn| {
            let owner: Option<String> = conn
                .query_row(
                    "SELECT document_id FROM document_version WHERE id=?1",
                    [&vid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if owner.as_deref() != Some(did.as_str()) {
                return Err(err(CoreErrorCode::NotFound));
            }
            let row = conn
                .query_row(
                    "SELECT paused, paused_at, updated_at FROM page_ocr_control \
                     WHERE document_version_id=?1",
                    [&vid],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)? != 0,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let (paused, paused_at, updated_at) = row.unwrap_or((false, None, 0));
            Ok(PageOcrControlView {
                document_id: did,
                version_id: vid,
                paused,
                paused_at,
                updated_at,
            })
        })
    }

    /// 暂停/继续页级 OCR:只写控制信号,不取消任务、不删除已识别页。
    /// 运行中的 OCR 循环在「当前页完成后」读取此信号;继续后从下一张 pending 页开始。
    pub fn set_page_ocr_paused(
        &self,
        document_id: &str,
        version_id: &str,
        paused: bool,
    ) -> Result<PageOcrControlView, AppError> {
        let now = self.clock.now_ms();
        let did = document_id.to_string();
        let vid = version_id.to_string();
        self.db.write(move |conn| {
            let owner: Option<String> = conn
                .query_row(
                    "SELECT document_id FROM document_version WHERE id=?1",
                    [&vid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if owner.as_deref() != Some(did.as_str()) {
                return Err(err(CoreErrorCode::NotFound));
            }
            conn.execute(
                "INSERT INTO page_ocr_control(document_version_id, paused, paused_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(document_version_id) DO UPDATE SET paused=excluded.paused, \
                 paused_at=excluded.paused_at, updated_at=excluded.updated_at",
                params![
                    vid,
                    if paused { 1 } else { 0 },
                    if paused { Some(now) } else { None },
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })?;
        self.get_page_ocr_control(document_id, version_id)
    }

    /// 读取某页 OCR 原文块(供 UI 纠错;只含 origin='ocr' 的块)。
    pub fn get_page_ocr_text(
        &self,
        document_id: &str,
        version_id: &str,
        page_number: i64,
    ) -> Result<PageOcrText, AppError> {
        if !(1..=100_000).contains(&page_number) {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "page_number_out_of_range".into()));
        }
        let did = document_id.to_string();
        let vid = version_id.to_string();
        self.db.read(move |conn| {
            let owner: Option<String> = conn
                .query_row(
                    "SELECT document_id FROM document_version WHERE id=?1",
                    [&vid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if owner.as_deref() != Some(did.as_str()) {
                return Err(err(CoreErrorCode::NotFound));
            }
            let gen = load_current_generation(conn, &vid)?
                .ok_or_else(|| err(CoreErrorCode::IndexGenerationUnavailable))?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, COALESCE(ordinal, 0), kind, text FROM content_block \
                     WHERE parse_generation_id=?1 AND origin='ocr' AND indexable=1 \
                       AND CAST(json_extract(locator_json, '$.pageNumber') AS INTEGER)=?2 \
                     ORDER BY COALESCE(ordinal, 0), block_key",
                )
                .map_err(|e| db_app_error(&e))?;
            let blocks = stmt
                .query_map(params![gen.id, page_number], |r| {
                    Ok(PageOcrBlockText {
                        block_id: r.get(0)?,
                        ordinal: r.get(1)?,
                        kind: r.get(2)?,
                        text: r.get(3)?,
                    })
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(PageOcrText {
                page_number,
                blocks,
            })
        })
    }

    /// 保存人工纠错并重建该页 FTS/覆盖:不新增迁移、不伪造模型文字。
    ///
    /// - 已有 OCR 块按 block_id 就地更新(引用/锚点不失效);
    /// - 扫描页原本没有块时,允许追加一段(origin='ocr', indexable);
    /// - 代次从 failed/canceled 升为 partial(如实:只有人工补录的文本可检索);
    /// - 完成后发出 ocr.progress,前端只按真实状态刷新。
    pub fn correct_page_ocr(
        &self,
        document_id: &str,
        version_id: &str,
        page_number: i64,
        corrections: &[PageOcrBlockCorrection],
        remove_block_ids: &[String],
        idempotency_key: &str,
    ) -> Result<Vec<PageOcrStatusView>, AppError> {
        if !(1..=100_000).contains(&page_number) {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "page_number_out_of_range".into()));
        }
        if idempotency_key.trim().is_empty()
            || corrections.len() > 200
            || remove_block_ids.len() > 200
        {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "invalid_ocr_correction".into()));
        }
        let mut total_chars = 0usize;
        for correction in corrections {
            let length = correction.text.chars().count();
            if length > 200_000 {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "ocr_correction_too_large".into()));
            }
            total_chars += length;
        }
        if total_chars > 500_000 {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "ocr_correction_too_large".into()));
        }
        if !corrections
            .iter()
            .any(|entry| !entry.text.trim().is_empty())
        {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "ocr_correction_empty".into()));
        }

        let did = document_id.to_string();
        let vid = version_id.to_string();
        let now = self.clock.now_ms();
        let new_ids: Vec<String> = corrections
            .iter()
            .filter(|entry| entry.block_id.is_none())
            .map(|_| self.ids.new_entity_id())
            .collect();
        let corrections = corrections.to_vec();
        let remove_block_ids = remove_block_ids.to_vec();
        let generation_id = self.db.write({
            let did = did.clone();
            let vid = vid.clone();
            let corrections = corrections.clone();
            let remove_block_ids = remove_block_ids.clone();
            let new_ids = new_ids.clone();
            move |conn| {
                let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
                let owner: Option<String> = tx
                    .query_row(
                        "SELECT document_id FROM document_version WHERE id=?1",
                        [&vid],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| db_app_error(&e))?;
                if owner.as_deref() != Some(did.as_str()) {
                    return Err(err(CoreErrorCode::NotFound));
                }
                let gen = load_current_generation(&tx, &vid)?
                    .ok_or_else(|| err(CoreErrorCode::IndexGenerationUnavailable))?;
                if matches!(
                    gen.status,
                    GenerationStatus::Queued
                        | GenerationStatus::Parsing
                        | GenerationStatus::Indexing
                ) {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "parse_in_progress".into()));
                }
                let mut stmt = tx
                    .prepare(
                        "SELECT id, block_key, COALESCE(ordinal, 0) FROM content_block \
                         WHERE parse_generation_id=?1 AND origin='ocr' \
                           AND CAST(json_extract(locator_json, '$.pageNumber') AS INTEGER)=?2",
                    )
                    .map_err(|e| db_app_error(&e))?;
                let existing = stmt
                    .query_map(params![gen.id, page_number], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    })
                    .map_err(|e| db_app_error(&e))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| db_app_error(&e))?;
                drop(stmt);
                let existing_ids: std::collections::HashSet<String> =
                    existing.iter().map(|(id, _, _)| id.clone()).collect();
                // 编辑器中明确删除的块:仅从索引与编辑视图移除(indexable=0),
                // 保留块记录供既有引用/锚点解析,不做物理删除。
                for block_id in &remove_block_ids {
                    if !existing_ids.contains(block_id) {
                        return Err(err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "ocr_block_not_on_page".into()));
                    }
                    tx.execute(
                        "DELETE FROM content_block_fts WHERE block_id=?1",
                        [block_id],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    tx.execute(
                        "UPDATE content_block SET indexable=0 WHERE id=?1 AND origin='ocr'",
                        [block_id],
                    )
                    .map_err(|e| db_app_error(&e))?;
                }
                let mut next_ordinal = tx
                    .query_row(
                        "SELECT COALESCE(MAX(ordinal), 0) FROM content_block \
                         WHERE parse_generation_id=?1",
                        [&gen.id],
                        |r| r.get::<_, i64>(0),
                    )
                    .map_err(|e| db_app_error(&e))?
                    + 1;
                let locator = serde_json::to_string(&SourceLocator::pdf(
                    page_number,
                    Vec::new(),
                    PageRotation::R0,
                ))
                .unwrap_or_default();
                let mut new_index = 0usize;
                for correction in &corrections {
                    let text = correction.text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let normalized = normalize_v1(text).normalized;
                    let text_hash = sha256_str(text);
                    match &correction.block_id {
                        Some(block_id) => {
                            if !existing_ids.contains(block_id) {
                                return Err(err(CoreErrorCode::InvalidRequest)
                                    .with_detail("reason", "ocr_block_not_on_page".into()));
                            }
                            tx.execute(
                                "UPDATE content_block SET text=?2, normalized_text=?3, \
                                 text_hash=?4, normalized_text_hash=?5 WHERE id=?1",
                                params![
                                    block_id,
                                    text,
                                    normalized,
                                    text_hash,
                                    sha256_str(&normalized)
                                ],
                            )
                            .map_err(|e| db_app_error(&e))?;
                            tx.execute(
                                "DELETE FROM content_block_fts WHERE block_id=?1",
                                [block_id],
                            )
                            .map_err(|e| db_app_error(&e))?;
                            tx.execute(
                                "INSERT INTO content_block_fts(text, block_id, parse_generation_id) \
                                 VALUES (?1,?2,?3)",
                                params![normalized, block_id, gen.id],
                            )
                            .map_err(|e| db_app_error(&e))?;
                        }
                        None => {
                            let block_id = new_ids
                                .get(new_index)
                                .cloned()
                                .ok_or_else(|| AppError::internal("ocr_correction_id"))?;
                            new_index += 1;
                            let block_key = format!("ocr:corrected:p{page_number}:{block_id}");
                            tx.execute(
                                "INSERT INTO content_block(id, document_version_id, \
                                 parse_generation_id, origin, block_key, ordinal, parent_block_id, \
                                 kind, text, normalized_text, text_hash, normalized_text_hash, \
                                 heading_path_json, locator_json, completeness, indexable, \
                                 normalization_version, ocr_confidence, created_at) \
                                 VALUES (?1,?2,?3,'ocr',?4,?5,NULL,'paragraph',?6,?7,?8,?9, \
                                 '[]',?10,'complete',1,?11,NULL,?12)",
                                params![
                                    block_id,
                                    vid,
                                    gen.id,
                                    block_key,
                                    next_ordinal,
                                    text,
                                    normalized,
                                    text_hash,
                                    sha256_str(&normalized),
                                    locator,
                                    NORMALIZATION_VERSION,
                                    now,
                                ],
                            )
                            .map_err(|e| db_app_error(&e))?;
                            tx.execute(
                                "INSERT INTO content_block_fts(text, block_id, parse_generation_id) \
                                 VALUES (?1,?2,?3)",
                                params![normalized, block_id, gen.id],
                            )
                            .map_err(|e| db_app_error(&e))?;
                            next_ordinal += 1;
                        }
                    }
                }
                let block_count: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM content_block \
                         WHERE parse_generation_id=?1 AND origin='ocr' AND indexable=1 \
                           AND CAST(json_extract(locator_json, '$.pageNumber') AS INTEGER)=?2",
                        params![gen.id, page_number],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let mut reasons: Vec<String> = tx
                    .query_row(
                        "SELECT reasons_json FROM page_ocr \
                         WHERE document_version_id=?1 AND page_number=?2",
                        params![vid, page_number],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|e| db_app_error(&e))?
                    .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
                    .unwrap_or_default();
                if !reasons.iter().any(|reason| reason == "user_corrected") {
                    reasons.push("user_corrected".into());
                }
                let reasons_json =
                    serde_json::to_string(&reasons).unwrap_or_else(|_| "[]".into());
                tx.execute(
                    "INSERT INTO page_ocr(document_version_id, page_number, parse_generation_id, \
                     status, quality_json, reasons_json, block_count, error_code, updated_at, \
                     completed_at) \
                     VALUES (?1,?2,?3,'completed',NULL,?4,?5,NULL,?6,?6) \
                     ON CONFLICT(document_version_id, page_number) DO UPDATE SET \
                     parse_generation_id=excluded.parse_generation_id, status='completed', \
                     reasons_json=excluded.reasons_json, block_count=excluded.block_count, \
                     error_code=NULL, updated_at=excluded.updated_at, \
                     completed_at=excluded.completed_at",
                    params![vid, page_number, gen.id, reasons_json, block_count, now],
                )
                .map_err(|e| db_app_error(&e))?;
                tx.execute(
                    "INSERT INTO parse_region(generation_id, region_key, region_kind, status, \
                     block_count, order_confidence, error_code, updated_at) \
                     VALUES (?1,?2,'page','complete',?3,'high',NULL,?4) \
                     ON CONFLICT(generation_id, region_key) DO UPDATE SET \
                     status='complete', block_count=excluded.block_count, error_code=NULL, \
                     updated_at=excluded.updated_at",
                    params![gen.id, format!("page:{page_number}"), block_count, now],
                )
                .map_err(|e| db_app_error(&e))?;
                let set_hash = compute_block_set_hash(&tx, &gen.id)?;
                tx.execute(
                    "UPDATE parse_generation SET status='partial', block_set_hash=?2, \
                     completed_at=?3, error_code=NULL WHERE id=?1",
                    params![gen.id, set_hash, now],
                )
                .map_err(|e| db_app_error(&e))?;
                tx.commit().map_err(|e| db_app_error(&e))?;
                Ok(gen.id)
            }
        })?;

        let event = DomainEvent::new(
            DomainEventType::OcrProgress,
            format!("page_ocr:{vid}:{page_number}:corrected"),
            1,
            now,
            serde_json::json!({
                "generationId": generation_id,
                "versionId": vid,
                "pageNumber": page_number,
                "status": "completed",
                "blockCount": corrections.len(),
                "reasons": ["user_corrected"],
            }),
        )
        .with_document(Some(did.clone()), Some(vid.clone()));
        self.events.emit(event);
        self.get_page_ocr_status(&did, &vid)
    }

    /// 指定页 OCR:前端 tesseract 识别结果落库(不经过解析桥)。
    ///
    /// - 只写 origin='ocr' 块;按确定性 block_key 更新,保留块 ID(既有引用不失效);
    /// - 选中页里消失的块只从索引与编辑视图移除(indexable=0),保留记录供旧引用解析;
    /// - 仅允许在当前代次已终态(complete/partial/failed/canceled)时写入;
    /// - 未识别过的页保持 pending,失败页如实标 failed;不把无文本页标成可问答。
    pub fn save_page_ocr_recognition(
        &self,
        document_id: &str,
        version_id: &str,
        pages: &[PageOcrRecognitionPage],
        idempotency_key: &str,
    ) -> Result<Vec<PageOcrStatusView>, AppError> {
        if idempotency_key.trim().is_empty() || pages.is_empty() || pages.len() > 50 {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "invalid_ocr_recognition".into()));
        }
        let mut seen_pages = std::collections::HashSet::new();
        let mut total_chars = 0usize;
        for page in pages {
            if page.page_number < 1 || page.page_number > 100_000 {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "page_number_out_of_range".into()));
            }
            if !seen_pages.insert(page.page_number) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "duplicate_page".into()));
            }
            if !matches!(
                page.status.as_str(),
                "completed" | "low_confidence" | "failed" | "skipped"
            ) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "invalid_ocr_status".into()));
            }
            if page.status == "skipped" && !page.blocks.is_empty() {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "skipped_with_blocks".into()));
            }
            if page.blocks.len() > 500 {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "ocr_blocks_too_many".into()));
            }
            if let Some(confidence) = page.confidence {
                if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "ocr_confidence_out_of_range".into()));
                }
            }
            for block in &page.blocks {
                if let Some(confidence) = block.confidence {
                    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                        return Err(err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "ocr_confidence_out_of_range".into()));
                    }
                }
                let length = block.text.chars().count();
                if length > 200_000 {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "ocr_recognition_too_large".into()));
                }
                total_chars += length;
                if total_chars > 500_000 {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "ocr_recognition_too_large".into()));
                }
                match &block.locator {
                    SourceLocator::Pdf { page_number, .. } if *page_number == page.page_number => {}
                    _ => {
                        return Err(err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "ocr_locator_page_mismatch".into()));
                    }
                }
            }
        }

        let did = document_id.to_string();
        let vid = version_id.to_string();
        let now = self.clock.now_ms();
        let pages = pages.to_vec();
        let block_ids: Vec<String> = pages
            .iter()
            .flat_map(|page| {
                page.blocks
                    .iter()
                    .filter(|block| !block.text.trim().is_empty())
                    .map(|_| self.ids.new_entity_id())
            })
            .collect();
        let generation_id = self.db.write({
            let did = did.clone();
            let vid = vid.clone();
            let pages = pages.clone();
            let block_ids = block_ids.clone();
            move |conn| {
                let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
                let owner: Option<String> = tx
                    .query_row(
                        "SELECT document_id FROM document_version WHERE id=?1",
                        [&vid],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| db_app_error(&e))?;
                if owner.as_deref() != Some(did.as_str()) {
                    return Err(err(CoreErrorCode::NotFound));
                }
                let gen = load_current_generation(&tx, &vid)?
                    .ok_or_else(|| err(CoreErrorCode::IndexGenerationUnavailable))?;
                if matches!(
                    gen.status,
                    GenerationStatus::Queued
                        | GenerationStatus::Parsing
                        | GenerationStatus::Indexing
                ) {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "parse_in_progress".into()));
                }
                let was_complete = gen.status == GenerationStatus::Complete;
                // 中断的全文识别留下的瞬态页：有可索引 OCR 块就恢复为
                // completed/low_confidence，没有块才回落 pending。不能把
                // 所有瞬态页一律清成 pending，否则重跑失败会丢掉旧识别结果。
                tx.execute(
                    "UPDATE page_ocr SET \
                       status = CASE WHEN (SELECT COUNT(*) FROM content_block cb \
                           WHERE cb.parse_generation_id=page_ocr.parse_generation_id \
                             AND cb.origin='ocr' AND cb.indexable=1 \
                             AND CAST(json_extract(cb.locator_json, '$.pageNumber') AS INTEGER)=page_ocr.page_number) > 0 \
                         THEN CASE WHEN page_ocr.confidence IS NOT NULL \
                                    AND page_ocr.confidence < 0.6 \
                              THEN 'low_confidence' ELSE 'completed' END \
                         ELSE 'pending' END, \
                       block_count = (SELECT COUNT(*) FROM content_block cb \
                           WHERE cb.parse_generation_id=page_ocr.parse_generation_id \
                             AND cb.origin='ocr' AND cb.indexable=1 \
                             AND CAST(json_extract(cb.locator_json, '$.pageNumber') AS INTEGER)=page_ocr.page_number), \
                       completed_at = CASE WHEN (SELECT COUNT(*) FROM content_block cb \
                           WHERE cb.parse_generation_id=page_ocr.parse_generation_id \
                             AND cb.origin='ocr' AND cb.indexable=1 \
                             AND CAST(json_extract(cb.locator_json, '$.pageNumber') AS INTEGER)=page_ocr.page_number) > 0 \
                         THEN COALESCE(page_ocr.completed_at, ?2) ELSE NULL END, \
                       error_code = NULL, updated_at=?2 \
                     WHERE document_version_id=?3 AND parse_generation_id=?1 \
                       AND status IN ('rendering','ocr_running')",
                    params![gen.id, now, vid],
                )
                .map_err(|e| db_app_error(&e))?;
                let mut next_ordinal: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MAX(ordinal), 0) FROM content_block \
                         WHERE parse_generation_id=?1",
                        [&gen.id],
                        |r| r.get::<_, i64>(0),
                    )
                    .map_err(|e| db_app_error(&e))?
                    + 1;
                let mut next_block_id = 0usize;
                for page in &pages {
                    let mut stmt = tx
                        .prepare(
                            "SELECT id, block_key FROM content_block \
                             WHERE parse_generation_id=?1 AND origin='ocr' \
                               AND CAST(json_extract(locator_json, '$.pageNumber') AS INTEGER)=?2",
                        )
                        .map_err(|e| db_app_error(&e))?;
                    let existing: Vec<(String, String)> = stmt
                        .query_map(params![gen.id, page.page_number], |r| {
                            Ok((r.get(0)?, r.get(1)?))
                        })
                        .map_err(|e| db_app_error(&e))?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| db_app_error(&e))?;
                    drop(stmt);
                    let existing_by_key: std::collections::HashMap<String, String> = existing
                        .iter()
                        .map(|(id, key)| (key.clone(), id.clone()))
                        .collect();
                    let mut kept_keys: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    let mut page_block_count = 0i64;
                    for (index, block) in page.blocks.iter().enumerate() {
                        let text = block.text.trim();
                        if text.is_empty() {
                            continue;
                        }
                        let block_key = format!("ocr:p{}:l{}", page.page_number, index);
                        kept_keys.insert(block_key.clone());
                        let normalized = normalize_v1(text).normalized;
                        let text_hash = sha256_str(text);
                        let locator_json =
                            serde_json::to_string(&block.locator).unwrap_or_default();
                        let kind = BlockKind::parse_db(block.kind.as_deref().unwrap_or("paragraph"))
                            .unwrap_or(BlockKind::Paragraph)
                            .as_str()
                            .to_string();
                        match existing_by_key.get(&block_key) {
                            Some(block_id) => {
                                tx.execute(
                                    "UPDATE content_block SET text=?2, normalized_text=?3, \
                                     text_hash=?4, normalized_text_hash=?5, locator_json=?6, \
                                     kind=?7, ocr_confidence=?8, indexable=1 WHERE id=?1",
                                    params![
                                        block_id,
                                        text,
                                        normalized,
                                        text_hash,
                                        sha256_str(&normalized),
                                        locator_json,
                                        kind,
                                        block.confidence,
                                    ],
                                )
                                .map_err(|e| db_app_error(&e))?;
                                tx.execute(
                                    "DELETE FROM content_block_fts WHERE block_id=?1",
                                    [block_id],
                                )
                                .map_err(|e| db_app_error(&e))?;
                                tx.execute(
                                    "INSERT INTO content_block_fts(text, block_id, parse_generation_id) \
                                     VALUES (?1,?2,?3)",
                                    params![normalized, block_id, gen.id],
                                )
                                .map_err(|e| db_app_error(&e))?;
                            }
                            None => {
                                let block_id = block_ids
                                    .get(next_block_id)
                                    .cloned()
                                    .ok_or_else(|| AppError::internal("ocr_recognition_id"))?;
                                next_block_id += 1;
                                tx.execute(
                                    "INSERT INTO content_block(id, document_version_id, \
                                     parse_generation_id, origin, block_key, ordinal, parent_block_id, \
                                     kind, text, normalized_text, text_hash, normalized_text_hash, \
                                     heading_path_json, locator_json, completeness, indexable, \
                                     normalization_version, ocr_confidence, created_at) \
                                     VALUES (?1,?2,?3,'ocr',?4,?5,NULL,?6,?7,?8,?9,?10, \
                                     '[]',?11,'complete',1,?12,?13,?14)",
                                    params![
                                        block_id,
                                        vid,
                                        gen.id,
                                        block_key,
                                        next_ordinal,
                                        kind,
                                        text,
                                        normalized,
                                        text_hash,
                                        sha256_str(&normalized),
                                        locator_json,
                                        NORMALIZATION_VERSION,
                                        block.confidence,
                                        now,
                                    ],
                                )
                                .map_err(|e| db_app_error(&e))?;
                                tx.execute(
                                    "INSERT INTO content_block_fts(text, block_id, parse_generation_id) \
                                     VALUES (?1,?2,?3)",
                                    params![normalized, block_id, gen.id],
                                )
                                .map_err(|e| db_app_error(&e))?;
                                next_ordinal += 1;
                            }
                        }
                        page_block_count += 1;
                    }
                    // 选中页中本次识别不存在的旧块:从索引与编辑视图移除(软删除)。
                    for (id, key) in &existing {
                        if kept_keys.contains(key) {
                            continue;
                        }
                        tx.execute("DELETE FROM content_block_fts WHERE block_id=?1", [id])
                            .map_err(|e| db_app_error(&e))?;
                        tx.execute(
                            "UPDATE content_block SET indexable=0 WHERE id=?1 AND origin='ocr'",
                            [id],
                        )
                        .map_err(|e| db_app_error(&e))?;
                    }
                    let mut reasons: Vec<String> = tx
                        .query_row(
                            "SELECT reasons_json FROM page_ocr \
                             WHERE document_version_id=?1 AND page_number=?2",
                            params![vid, page.page_number],
                            |r| r.get::<_, String>(0),
                        )
                        .optional()
                        .map_err(|e| db_app_error(&e))?
                        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
                        .unwrap_or_default();
                    reasons.retain(|reason| reason == "no_text_layer");
                    if !reasons.iter().any(|reason| reason == "page_scoped_ocr") {
                        reasons.push("page_scoped_ocr".into());
                    }
                    if page.status == "low_confidence"
                        && !reasons.iter().any(|reason| reason == "low_confidence")
                    {
                        reasons.push("low_confidence".into());
                    }
                    if page.status == "skipped" {
                        if !reasons.iter().any(|reason| reason == "text_layer_ok") {
                            reasons.push("text_layer_ok".into());
                        }
                    } else if page_block_count == 0
                        && !reasons.iter().any(|reason| reason == "no_text_recognized")
                    {
                        reasons.push("no_text_recognized".into());
                    }
                    let reasons_json =
                        serde_json::to_string(&reasons).unwrap_or_else(|_| "[]".into());
                    tx.execute(
                        "INSERT INTO page_ocr(document_version_id, page_number, parse_generation_id, \
                         status, quality_json, reasons_json, render_hash, engine_version, confidence, \
                         block_count, error_code, attempt, updated_at, completed_at) \
                         VALUES (?1,?2,?3,?4,NULL,?5,NULL,?6,?7,?8,?9,0,?10,?10) \
                         ON CONFLICT(document_version_id, page_number) DO UPDATE SET \
                         parse_generation_id=excluded.parse_generation_id, status=excluded.status, \
                         reasons_json=excluded.reasons_json, engine_version=excluded.engine_version, \
                         confidence=excluded.confidence, block_count=excluded.block_count, \
                         error_code=excluded.error_code, updated_at=excluded.updated_at, \
                         completed_at=excluded.completed_at",
                        params![
                            vid,
                            page.page_number,
                            gen.id,
                            page.status,
                            reasons_json,
                            page.engine_version,
                            page.confidence,
                            page_block_count,
                            page.error_code,
                            now,
                        ],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    // skipped 表示文本层合格、本次未做 OCR：保留解析器已有的
                    // page region，不要把它的 block_count/状态改成 0。
                    if page.status != "skipped" {
                        tx.execute(
                            "INSERT INTO parse_region(generation_id, region_key, region_kind, status, \
                             block_count, order_confidence, error_code, updated_at) \
                             VALUES (?1,?2,'page',?3,?4,'high',?5,?6) \
                             ON CONFLICT(generation_id, region_key) DO UPDATE SET \
                             status=excluded.status, block_count=excluded.block_count, \
                             error_code=excluded.error_code, updated_at=excluded.updated_at",
                            params![
                                gen.id,
                                format!("page:{}", page.page_number),
                                if page.status == "failed" { "failed" } else { "complete" },
                                page_block_count,
                                page.error_code,
                                now,
                            ],
                        )
                        .map_err(|e| db_app_error(&e))?;
                    }
                }
                let _ = repair_fts_tx(&tx, &gen.id)?;
                let set_hash = compute_block_set_hash(&tx, &gen.id)?;
                let (_complete_c, partial_c, failed_c, _no_text_c, total) =
                    region_stats(&tx, &gen.id)?;
                let final_status = if total > 0 && failed_c == total {
                    GenerationStatus::Failed
                } else if was_complete && (total == 0 || (partial_c == 0 && failed_c == 0)) {
                    GenerationStatus::Complete
                } else {
                    GenerationStatus::Partial
                };
                tx.execute(
                    "UPDATE parse_generation SET status=?2, block_set_hash=?3, \
                     completed_at=?4, error_code=NULL WHERE id=?1",
                    params![gen.id, final_status.as_str(), set_hash, now],
                )
                .map_err(|e| db_app_error(&e))?;
                tx.commit().map_err(|e| db_app_error(&e))?;
                Ok(gen.id)
            }
        })?;

        for page in &pages {
            let event = DomainEvent::new(
                DomainEventType::OcrProgress,
                format!("page_ocr:{vid}:{}:recognized", page.page_number),
                1,
                now,
                serde_json::json!({
                    "generationId": generation_id,
                    "versionId": vid,
                    "pageNumber": page.page_number,
                    "status": page.status,
                    "blockCount": page.blocks.iter().filter(|b| !b.text.trim().is_empty()).count(),
                    "scoped": true,
                }),
            )
            .with_document(Some(did.clone()), Some(vid.clone()));
            self.events.emit(event);
        }
        self.get_page_ocr_status(&did, &vid)
    }

    /// 代次内关键词检索(M04-A6):FTS MATCH(字面量化)+ 短词 LIKE 降级。
    pub fn fts_search(
        &self,
        generation_id: &str,
        keywords: &[String],
        limit: i64,
    ) -> Result<Vec<FtsHit>, AppError> {
        let keywords: Vec<String> = keywords
            .iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        if keywords.is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let limit = limit.clamp(1, 200);
        let gid = generation_id.to_string();
        self.db.read(move |conn| {
            let gen = load_generation(conn, &gid)?
                .ok_or_else(|| err(CoreErrorCode::IndexGenerationUnavailable))?;
            if !matches!(
                gen.status,
                GenerationStatus::Complete
                    | GenerationStatus::Partial
                    | GenerationStatus::Parsing
                    | GenerationStatus::Indexing
            ) {
                return Err(err(CoreErrorCode::IndexGenerationUnavailable)
                    .with_detail("reason", "generation_not_searchable".into()));
            }
            let mut hits: Vec<FtsHit> = Vec::new();
            if fts::needs_like_fallback(&keywords) {
                // 短词降级:代次内 LIKE,不走全文 FTS(《01》§8.1)。
                // 先 AND(精确);零命中且多关键词时放宽 OR,避免长问题/短数字词
                // 导致“文档明明有证据却检索为空”。
                let run_like = |operator: &str| -> Result<Vec<(String, String)>, AppError> {
                    let n = keywords.len();
                    let patterns: Vec<rusqlite::types::Value> = keywords
                        .iter()
                        .map(|kw| rusqlite::types::Value::Text(fts::like_pattern(kw)))
                        .collect();
                    let mut sql = String::from("SELECT c.id, c.normalized_text, (");
                    // 命中词数作为排序分:OR 放宽后仍把最相关的块排在前面,
                    // 不能按扫描顺序把前几页塞满候选。
                    for index in 0..n {
                        if index > 0 {
                            sql.push_str(" + ");
                        }
                        sql.push_str(&format!(
                            "(c.normalized_text LIKE ?{} ESCAPE '\\')",
                            index + 2
                        ));
                    }
                    sql.push_str(
                        ") AS score FROM content_block c \
                         WHERE c.parse_generation_id=?1 AND c.indexable=1 AND (",
                    );
                    for index in 0..n {
                        if index > 0 {
                            sql.push_str(operator);
                        }
                        sql.push_str(&format!(
                            "c.normalized_text LIKE ?{} ESCAPE '\\'",
                            n + 2 + index
                        ));
                    }
                    sql.push_str(&format!(
                        ") ORDER BY score DESC, c.ordinal LIMIT ?{}",
                        2 * n + 2
                    ));
                    let mut binds: Vec<rusqlite::types::Value> =
                        vec![rusqlite::types::Value::Text(gid.clone())];
                    binds.extend(patterns.iter().cloned());
                    binds.extend(patterns.iter().cloned());
                    binds.push(rusqlite::types::Value::Integer(limit));
                    let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
                    let rows: Vec<(String, String)> = stmt
                        .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                            Ok((r.get(0)?, r.get(1)?))
                        })
                        .map_err(|e| db_app_error(&e))?
                        .collect::<Result<_, _>>()
                        .map_err(|e| db_app_error(&e))?;
                    drop(stmt);
                    Ok(rows)
                };
                let mut rows = run_like(" AND ")?;
                if rows.is_empty() && keywords.len() > 1 {
                    rows = run_like(" OR ")?;
                }
                for (block_id, text) in rows {
                    let first_kw = keywords
                        .iter()
                        .find(|k| text.contains(k.trim()))
                        .cloned()
                        .unwrap_or_default();
                    hits.push(FtsHit {
                        block_id,
                        snippet: snippet_around(&text, &first_kw),
                        rank: 0.0,
                    });
                }
            } else {
                let expr = fts::build_match_expression(&keywords)
                    .ok_or_else(|| err(CoreErrorCode::InvalidRequest))?;
                let query_with =
                    |expression: &str| -> Result<Vec<(String, String, f64)>, AppError> {
                        let mut stmt = conn
                        .prepare(
                            // 不使用别名:FROM 中别名与表名 MATCH 混用会被解析为
                            // 同表两次引用(自连接),产生重复行。
                            "SELECT block_id, snippet(content_block_fts, 0, '', '', '…', ?3), rank \
                             FROM content_block_fts \
                             WHERE parse_generation_id=?1 AND content_block_fts MATCH ?2 \
                             ORDER BY rank LIMIT ?4",
                        )
                        .map_err(|e| db_app_error(&e))?;
                        let rows = stmt
                            .query_map(params![gid, expression, FTS_SNIPPET_TOKENS, limit], |r| {
                                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                            })
                            .map_err(|e| db_app_error(&e))?
                            .collect::<Result<_, _>>()
                            .map_err(|e| db_app_error(&e))?;
                        drop(stmt);
                        Ok(rows)
                    };
                let mut rows = query_with(&expr)?;
                // 长问题 AND 零命中时放宽为 OR:避免整篇文档明明有证据却返回“无依据”。
                if rows.is_empty() {
                    if let Some(relaxed) = fts::build_match_expression_relaxed(&keywords) {
                        if relaxed != expr {
                            rows = query_with(&relaxed)?;
                        }
                    }
                }
                for (block_id, snippet, rank) in rows {
                    hits.push(FtsHit {
                        block_id,
                        snippet,
                        rank,
                    });
                }
            }
            hits.truncate(limit as usize);
            Ok(hits)
        })
    }

    /// 创建向量代次(《M04》§9):唯一键 (pg, provider, model, dims, config)。
    pub fn embedding_start(
        &self,
        parse_generation_id: &str,
        provider_id: &str,
        model_id: &str,
        dimensions: i64,
        config_hash: &str,
    ) -> Result<String, AppError> {
        if dimensions <= 0 || dimensions > 8192 {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "invalid_dimensions".into()));
        }
        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let pg = parse_generation_id.to_string();
        let pid = provider_id.to_string();
        let mid = model_id.to_string();
        let cfg = config_hash.to_string();
        self.db.write(move |conn| {
            let gen = load_generation(conn, &pg)?
                .ok_or_else(|| err(CoreErrorCode::IndexGenerationUnavailable))?;
            if !matches!(
                gen.status,
                GenerationStatus::Complete | GenerationStatus::Partial
            ) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "parse_not_complete".into()));
            }
            conn.execute(
                "INSERT OR IGNORE INTO embedding_generation(id, parse_generation_id, provider_id, \
                 model_id, dimensions, distance_function, config_hash, status, created_at) \
                 VALUES (?1,?2,?3,?4,?5,'cosine',?6,'building',?7)",
                params![id, pg, pid, mid, dimensions, cfg, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let existing: String = conn
                .query_row(
                    "SELECT id FROM embedding_generation WHERE parse_generation_id=?1 \
                     AND provider_id=?2 AND model_id=?3 AND dimensions=?4 AND config_hash=?5",
                    params![pg, pid, mid, dimensions, cfg],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            // 重建:非 building 状态复位。
            conn.execute(
                "UPDATE embedding_generation SET status='building', error_code=NULL, \
                 completed_at=NULL WHERE id=?1 AND status != 'building'",
                params![existing],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(existing)
        })
    }

    /// 向量批次:维度不一致整批失败零写入(M04-A7);向量与元数据同事务。
    pub fn submit_embedding_batch(
        &self,
        embedding_generation_id: &str,
        items: &[EmbeddingInput],
    ) -> Result<(), AppError> {
        if items.is_empty() || items.len() > MAX_EMBEDDING_BATCH {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("maxItems", (MAX_EMBEDDING_BATCH as i64).into()));
        }
        let now = self.clock.now_ms();
        let egid = embedding_generation_id.to_string();
        let items: Vec<(String, Vec<u8>)> = items
            .iter()
            .map(|i| {
                let mut blob = Vec::with_capacity(i.vector.len() * 4);
                for v in &i.vector {
                    blob.extend_from_slice(&v.to_le_bytes());
                }
                (i.block_id.clone(), blob)
            })
            .collect();
        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let eg: Option<(String, i64)> = tx
                .query_row(
                    "SELECT parse_generation_id, dimensions FROM embedding_generation WHERE id=?1",
                    [&egid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let (pg_id, dims) = eg.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            // 维度校验先于任何写入:任何一项不匹配,整批拒绝(A7)。
            if items.iter().any(|(_, b)| b.len() as i64 != dims * 4) {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "embedding_dimension_mismatch".into())
                    .with_detail("expected", dims.into()));
            }
            for (block_id, blob) in &items {
                let owned: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM content_block \
                         WHERE id=?1 AND parse_generation_id=?2 AND indexable=1",
                        params![block_id, pg_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                if owned == 0 {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "block_not_in_generation".into()));
                }
                tx.execute(
                    "INSERT OR REPLACE INTO vector_embedding(generation_id, block_id, dimensions, \
                     vector_blob, created_at) VALUES (?1,?2,?3,?4,?5)",
                    params![egid, block_id, dims, blob, now],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }

    /// 嵌入完成:覆盖比较决定 complete/partial。
    pub fn embedding_complete(
        &self,
        embedding_generation_id: &str,
    ) -> Result<VectorState, AppError> {
        let now = self.clock.now_ms();
        let egid = embedding_generation_id.to_string();
        self.db.write(move |conn| {
            let pg_id: String = conn
                .query_row(
                    "SELECT parse_generation_id FROM embedding_generation WHERE id=?1",
                    [&egid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let vectors: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM vector_embedding WHERE generation_id=?1",
                    [&egid],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            let blocks: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM content_block \
                     WHERE parse_generation_id=?1 AND indexable=1",
                    [&pg_id],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            let status = if blocks > 0 && vectors >= blocks {
                "complete"
            } else {
                "partial"
            };
            conn.execute(
                "UPDATE embedding_generation SET status=?2, completed_at=?3 WHERE id=?1",
                params![egid, status, now],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(if status == "complete" {
                VectorState::Complete
            } else {
                VectorState::Partial
            })
        })
    }

    /// 全文代次 KNN:MVP 在进程内对向量行做精确余弦距离;sqlite-vec 虚表
    /// 由 M08 接入后替换执行层,语义与代次隔离不变(《01》§8.2)。
    pub fn vector_search_full(
        &self,
        parse_generation_id: &str,
        query: &[f32],
        limit: i64,
    ) -> Result<Vec<VectorHit>, AppError> {
        let limit = limit.clamp(1, 100);
        let pg = parse_generation_id.to_string();
        let query = query.to_vec();
        self.db.read(move |conn| {
            let egid: Option<String> = conn
                .query_row(
                    "SELECT id FROM embedding_generation WHERE parse_generation_id=?1 \
                     AND status IN ('complete','partial','building') \
                     ORDER BY created_at DESC LIMIT 1",
                    [&pg],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let egid = egid.ok_or_else(|| {
                err(CoreErrorCode::IndexFtsOnly)
                    .with_detail("reason", "vector_not_available".into())
            })?;
            let dims: i64 = conn
                .query_row(
                    "SELECT dimensions FROM embedding_generation WHERE id=?1",
                    [&egid],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            if query.len() as i64 != dims {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "query_dimension_mismatch".into())
                    .with_detail("expected", dims.into()));
            }
            let mut stmt = conn
                .prepare(
                    "SELECT block_id, vector_blob FROM vector_embedding WHERE generation_id=?1",
                )
                .map_err(|e| db_app_error(&e))?;
            let rows: Vec<(String, Vec<u8>)> = stmt
                .query_map([&egid], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            let mut hits: Vec<VectorHit> = rows
                .into_iter()
                .filter_map(|(id, blob)| {
                    cosine_distance(&query, &blob).map(|d| VectorHit {
                        block_id: id,
                        distance: d,
                    })
                })
                .collect();
            hits.sort_by(|a, b| {
                a.distance
                    .partial_cmp(&b.distance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(limit as usize);
            Ok(hits)
        })
    }

    /// 范围内精排:显式块集合上计算距离,范围过滤先于排名(《01》§8.2)。
    pub fn vector_search_within(
        &self,
        block_ids: &[String],
        query: &[f32],
        limit: i64,
    ) -> Result<Vec<VectorHit>, AppError> {
        if block_ids.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 100);
        let ids = block_ids.to_vec();
        let query = query.to_vec();
        self.db.read(move |conn| {
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT block_id, vector_blob FROM vector_embedding \
                 WHERE dimensions=?1 AND block_id IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let mut binds: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::Integer(query.len() as i64)];
            binds.extend(ids.iter().cloned().map(rusqlite::types::Value::Text));
            let rows: Vec<(String, Vec<u8>)> = stmt
                .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            let mut hits: Vec<VectorHit> = rows
                .into_iter()
                .filter_map(|(id, blob)| {
                    cosine_distance(&query, &blob).map(|d| VectorHit {
                        block_id: id,
                        distance: d,
                    })
                })
                .collect();
            hits.sort_by(|a, b| {
                a.distance
                    .partial_cmp(&b.distance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(limit as usize);
            Ok(hits)
        })
    }

    pub fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<ContentBlockView>, AppError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = ids.to_vec();
        self.db.read(move |conn| {
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql =
                format!("SELECT {BLOCK_COLS} FROM content_block WHERE id IN ({placeholders})");
            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let rows: Vec<BlockRow> = stmt
                .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                    BlockRow::try_from(r)
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(rows.iter().filter_map(|r| r.to_view()).collect())
        })
    }

    /// 固定代次全文块(快照固定集合的底层查询;M07 在此之上冻结)。
    pub fn list_generation_blocks(
        &self,
        generation_id: &str,
    ) -> Result<Vec<ContentBlockView>, AppError> {
        let gid = generation_id.to_string();
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {BLOCK_COLS} FROM content_block \
                     WHERE parse_generation_id=?1 AND origin IN ('parser','ocr') ORDER BY ordinal"
                ))
                .map_err(|e| db_app_error(&e))?;
            let rows: Vec<BlockRow> = stmt
                .query_map([&gid], |r| BlockRow::try_from(r))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(rows.iter().filter_map(|r| r.to_view()).collect())
        })
    }

    pub fn list_unsupported_regions(
        &self,
        version_id: &str,
    ) -> Result<Vec<UnsupportedRegionView>, AppError> {
        let vid = version_id.to_string();
        self.db.read(move |conn| {
            let gen = load_current_generation(conn, &vid)?;
            match gen {
                Some(g) => list_unsupported_for_generation(conn, &g.id),
                None => Ok(Vec::new()),
            }
        })
    }

    /// 启动恢复:遗留 parsing/indexing 代次标记 failed(TASK_INTERRUPTED)。
    /// 对应任务已由 TaskService 恢复为 interrupted;重试复用同代次。
    pub fn recover_generations(&self) -> Result<Vec<String>, AppError> {
        let now = self.clock.now_ms();
        self.db.write(move |conn| {
            let mut stmt = conn
                .prepare("SELECT id FROM parse_generation WHERE status IN ('parsing','indexing')")
                .map_err(|e| db_app_error(&e))?;
            let ids: Vec<String> = stmt
                .query_map([], |r| r.get(0))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            for id in &ids {
                let (c, p, f, _nt, total) = region_stats(conn, id)?;
                if total > 0 && c == total && f == 0 && p == 0 {
                    repair_fts_tx(conn, id)?;
                    let set_hash = compute_block_set_hash(conn, id)?;
                    conn.execute(
                        "UPDATE parse_generation SET status='complete', block_set_hash=?2, completed_at=?3, error_code=NULL WHERE id=?1",
                        params![id, set_hash, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                } else {
                    conn.execute(
                        "UPDATE parse_generation SET status='failed', error_code='TASK_INTERRUPTED', \
                         completed_at=?2 WHERE id=?1",
                        params![id, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                }
            }
            Ok(ids)
        })
    }

    /// 显式 FTS 修复入口(A9 验证与维护路径)。
    pub fn repair_fts(&self, generation_id: &str) -> Result<i64, AppError> {
        let gid = generation_id.to_string();
        self.db.write(move |conn| repair_fts_tx(conn, &gid))
    }

    /// 删除代次 FTS 行(等价重建验证用)。
    #[cfg(test)]
    pub fn drop_fts_rows(&self, generation_id: &str) -> Result<i64, AppError> {
        let gid = generation_id.to_string();
        self.db.write(move |conn| {
            let n = conn
                .execute(
                    "DELETE FROM content_block_fts WHERE parse_generation_id=?1",
                    [&gid],
                )
                .map_err(|e| db_app_error(&e))?;
            Ok(n as i64)
        })
    }

    /// 读取代次(测试与上层组装用)。
    pub fn get_generation(&self, generation_id: &str) -> Result<Option<GenerationRow>, AppError> {
        let gid = generation_id.to_string();
        self.db.read(move |conn| load_generation(conn, &gid))
    }
}

fn list_unsupported_for_generation(
    conn: &Connection,
    generation_id: &str,
) -> Result<Vec<UnsupportedRegionView>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT region_key, object_type, reason_code, locator_json \
             FROM unsupported_region WHERE generation_id=?1 ORDER BY region_key",
        )
        .map_err(|e| db_app_error(&e))?;
    let rows: Vec<(String, String, String, String)> = stmt
        .query_map([generation_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<_, _>>()
        .map_err(|e| db_app_error(&e))?;
    Ok(rows
        .into_iter()
        .filter_map(|(k, t, c, l)| {
            Some(UnsupportedRegionView {
                region_key: k,
                object_type: t,
                reason_code: c,
                locator: serde_json::from_str(&l).ok()?,
            })
        })
        .collect())
}

fn parse_region_kind(s: &str) -> RegionKind {
    match s {
        "page" => RegionKind::Page,
        "heading" => RegionKind::Heading,
        "object" => RegionKind::Object,
        _ => RegionKind::Body,
    }
}

fn parse_confidence(s: &str) -> OrderConfidence {
    if s == "low" {
        OrderConfidence::Low
    } else {
        OrderConfidence::High
    }
}

/// LIKE 路径的简易摘要:关键词前后各约 24 个字符。
fn snippet_around(text: &str, keyword: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let kw: Vec<char> = keyword.chars().collect();
    if kw.is_empty() || kw.len() > chars.len() {
        return chars.iter().take(48).collect();
    }
    let pos = (0..=chars.len() - kw.len()).find(|&i| chars[i..i + kw.len()] == kw[..]);
    match pos {
        Some(i) => {
            let start = i.saturating_sub(24);
            let end = (i + kw.len() + 24).min(chars.len());
            let prefix = if start > 0 { "…" } else { "" };
            let suffix = if end < chars.len() { "…" } else { "" };
            format!(
                "{prefix}{}{suffix}",
                chars[start..end].iter().collect::<String>()
            )
        }
        None => chars.iter().take(48).collect(),
    }
}

/// f32 LE blob 的余弦距离;维度不符或零向量返回 None。
fn cosine_distance(query: &[f32], blob: &[u8]) -> Option<f64> {
    let n = blob.len() / 4;
    if query.len() != n || n == 0 {
        return None;
    }
    let mut dot = 0f64;
    let mut qn = 0f64;
    let mut bn = 0f64;
    for (i, q) in query.iter().take(n).enumerate() {
        let bytes: [u8; 4] = blob[i * 4..i * 4 + 4].try_into().ok()?;
        let b = f32::from_le_bytes(bytes) as f64;
        dot += (*q as f64) * b;
        qn += (*q as f64) * (*q as f64);
        bn += b * b;
    }
    if qn == 0.0 || bn == 0.0 {
        return None;
    }
    Some(1.0 - dot / (qn.sqrt() * bn.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::locator::PageRotation;

    #[test]
    fn locator_format_matrix() {
        let pdf = SourceLocator::pdf(1, vec![], PageRotation::R0);
        let md = SourceLocator::markdown(0, 1, "paragraph", vec![]);
        let dx = SourceLocator::docx(0, 1, vec![], vec![]);
        assert!(locator_matches_format(&pdf, DocumentFormat::Pdf));
        assert!(!locator_matches_format(&pdf, DocumentFormat::Markdown));
        assert!(locator_matches_format(&md, DocumentFormat::Markdown));
        assert!(locator_matches_format(&dx, DocumentFormat::Docx));
        let comp = SourceLocator::Composite {
            parts: vec![pdf.clone()],
            locator_version: 1,
        };
        assert!(locator_matches_format(&comp, DocumentFormat::Pdf));
        assert!(!locator_matches_format(&comp, DocumentFormat::Docx));
    }

    #[test]
    fn config_hash_separates_versions_and_formats() {
        let v1 = ParserVersions::default();
        let mut v2 = ParserVersions::default();
        assert_eq!(
            parser_config_hash_of(&v1, DocumentFormat::Pdf),
            parser_config_hash_of(&v1, DocumentFormat::Pdf)
        );
        v2.segmentation_version = "segmentation/2".into();
        assert_ne!(
            parser_config_hash_of(&v1, DocumentFormat::Pdf),
            parser_config_hash_of(&v2, DocumentFormat::Pdf)
        );
        assert_ne!(
            parser_config_hash_of(&v1, DocumentFormat::Pdf),
            parser_config_hash_of(&v1, DocumentFormat::Markdown)
        );
    }

    #[test]
    fn cosine_distance_basics() {
        let blob = |vals: &[f32]| {
            let mut b = Vec::new();
            for v in vals {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b
        };
        let d = cosine_distance(&[1.0, 0.0], &blob(&[1.0, 0.0])).unwrap();
        assert!(d.abs() < 1e-9, "identical vectors distance {d}");
        let d2 = cosine_distance(&[1.0, 0.0], &blob(&[2.0, 0.0])).unwrap();
        assert!(d2.abs() < 1e-9, "same direction distance {d2}");
        let d3 = cosine_distance(&[1.0, 0.0], &blob(&[0.0, 1.0])).unwrap();
        assert!((d3 - 1.0).abs() < 1e-9, "orthogonal distance {d3}");
        assert!(cosine_distance(&[1.0, 2.0, 3.0], &blob(&[1.0, 0.0])).is_none());
        assert!(cosine_distance(&[0.0, 0.0], &blob(&[0.0, 0.0])).is_none());
    }

    #[test]
    fn base64_helper_roundtrip_shape() {
        let encoded = crate::shared::dto::base64_encode(&[0u8, 1, 2, 250, 251]);
        assert!(!encoded.is_empty() && !encoded.contains(' '));
    }

    #[test]
    fn active_task_id_only_exposed_while_parse_is_active() {
        // 解析完成后 task_id 仍留在代次行上;视图必须只在进行中状态下暴露它,
        // 否则前端“解析完成后自动补识别”会被永久挡住。
        let path = std::env::temp_dir().join(format!(
            "kw-parse-status-{}.sqlite3",
            crate::shared::SystemIdGenerator.new_entity_id()
        ));
        let db = Arc::new(Db::open(&path, 1).unwrap());
        db.write(|conn| {
            let migrations = crate::persistence::registration::migrations()
                .into_iter()
                .chain(crate::document::registration::migrations())
                .chain(crate::parse_index::registration::migrations());
            for migration in migrations {
                conn.execute_batch(migration.sql)
                    .map_err(|e| db_app_error(&e))?;
            }
            conn.execute_batch(
                "INSERT INTO document(id,display_name,format,current_version_id,created_at,updated_at)
                   VALUES ('d1','scan','pdf','v1',0,0);
                 INSERT INTO document_version(id,document_id,fingerprint_status,created_at)
                   VALUES ('v1','d1','ready',0);
                 INSERT INTO parse_generation(id,document_version_id,parser_config_hash,parser_version,
                   status,is_current,task_id,started_at)
                   VALUES ('g1','v1','hash','parser/1','parsing',1,'t1',0);",
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
        .unwrap();
        let clock = Arc::new(crate::shared::SystemClock);
        let ids = Arc::new(crate::shared::SystemIdGenerator);
        let events = Arc::new(crate::shared::NoopEventSink);
        let tasks = Arc::new(crate::persistence::task::TaskService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let service = ParseIndexService::new(db.clone(), clock, ids, events, tasks);

        for (status, expected) in [
            ("queued", true),
            ("parsing", true),
            ("indexing", true),
            ("complete", false),
            ("partial", false),
            ("failed", false),
            ("canceled", false),
        ] {
            db.write(move |conn| {
                conn.execute(
                    "UPDATE parse_generation SET status=?1 WHERE id='g1'",
                    rusqlite::params![status],
                )
                .map_err(|e| db_app_error(&e))?;
                Ok(())
            })
            .unwrap();
            let view = service.get_status("d1").unwrap();
            assert_eq!(
                view.active_task_id.is_some(),
                expected,
                "status={status} 时 active_task_id 暴露状态不正确"
            );
        }
        drop(service);
        drop(db);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
    }

    #[test]
    fn page_ocr_control_persists_pause_without_touching_pages() {
        let path = std::env::temp_dir().join(format!(
            "kw-ocr-control-{}.sqlite3",
            crate::shared::SystemIdGenerator.new_entity_id()
        ));
        let db = Arc::new(Db::open(&path, 1).unwrap());
        db.write(|conn| {
            let migrations = crate::persistence::registration::migrations()
                .into_iter()
                .chain(crate::document::registration::migrations())
                .chain(crate::parse_index::registration::migrations());
            for migration in migrations {
                conn.execute_batch(migration.sql)
                    .map_err(|e| db_app_error(&e))?;
            }
            conn.execute_batch(
                "INSERT INTO document(id,display_name,format,current_version_id,created_at,updated_at)
                   VALUES ('d1','scan','pdf','v1',0,0);
                 INSERT INTO document_version(id,document_id,fingerprint_status,created_at)
                   VALUES ('v1','d1','ready',0);
                 INSERT INTO parse_generation(id,document_version_id,parser_config_hash,parser_version,
                   status,is_current,started_at)
                   VALUES ('g1','v1','hash','parser/1','parsing',1,0);
                 INSERT INTO page_ocr(document_version_id,page_number,parse_generation_id,status,
                   reasons_json,block_count,attempt,updated_at)
                   VALUES ('v1',1,'g1','completed','[]',2,0,0),
                          ('v1',2,'g1','pending','[]',0,0,0);",
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
        .unwrap();
        let clock = Arc::new(crate::shared::SystemClock);
        let ids = Arc::new(crate::shared::SystemIdGenerator);
        let events = Arc::new(crate::shared::NoopEventSink);
        let tasks = Arc::new(crate::persistence::task::TaskService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let service = ParseIndexService::new(db.clone(), clock, ids, events, tasks);
        let initial = service.get_page_ocr_control("d1", "v1").unwrap();
        assert!(!initial.paused);
        let paused = service.set_page_ocr_paused("d1", "v1", true).unwrap();
        assert!(paused.paused);
        assert!(paused.paused_at.is_some());
        let again = service.get_page_ocr_control("d1", "v1").unwrap();
        assert!(again.paused, "暂停信号必须持久化");
        // 暂停只写控制信号:页状态与块数不变。
        let status = service.get_page_ocr_status("d1", "v1").unwrap();
        assert_eq!(status.len(), 2);
        assert_eq!(status[0].block_count, 2);
        let resumed = service.set_page_ocr_paused("d1", "v1", false).unwrap();
        assert!(!resumed.paused);
        assert!(resumed.paused_at.is_none());
        let wrong_doc = service.set_page_ocr_paused("d2", "v1", true);
        assert_eq!(wrong_doc.unwrap_err().code, "NOT_FOUND");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
    }

    #[test]
    fn page_scoped_ocr_saves_blocks_preserves_other_pages_and_ids() {
        use crate::parse_index::models::{
            PageOcrRecognitionBlock, PageOcrRecognitionPage, PageOcrStatus,
        };
        use crate::shared::locator::PdfRect;

        let path = std::env::temp_dir().join(format!(
            "kw-ocr-recognition-{}.sqlite3",
            crate::shared::SystemIdGenerator.new_entity_id()
        ));
        let db = Arc::new(Db::open(&path, 1).unwrap());
        db.write(|conn| {
            let migrations = crate::persistence::registration::migrations()
                .into_iter()
                .chain(crate::document::registration::migrations())
                .chain(crate::parse_index::registration::migrations());
            for migration in migrations {
                conn.execute_batch(migration.sql)
                    .map_err(|e| db_app_error(&e))?;
            }
            conn.execute_batch(
                "INSERT INTO document(id,display_name,format,current_version_id,created_at,updated_at)
                   VALUES ('d1','scan','pdf','v1',0,0);
                 INSERT INTO document_version(id,document_id,fingerprint_status,created_at)
                   VALUES ('v1','d1','ready',0);
                 INSERT INTO parse_generation(id,document_version_id,parser_config_hash,parser_version,
                   status,is_current,started_at)
                   VALUES ('g1','v1','hash','parser/1','canceled',1,0);
                 INSERT INTO page_ocr(document_version_id,page_number,parse_generation_id,status,
                   reasons_json,block_count,attempt,updated_at)
                   VALUES ('v1',1,'g1','completed','[\"no_text_layer\"]',3,0,0),
                          ('v1',2,'g1','completed','[\"no_text_layer\"]',4,0,0);",
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
        .unwrap();
        let clock = Arc::new(crate::shared::SystemClock);
        let ids = Arc::new(crate::shared::SystemIdGenerator);
        let events = Arc::new(crate::shared::NoopEventSink);
        let tasks = Arc::new(crate::persistence::task::TaskService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let service = ParseIndexService::new(db.clone(), clock, ids, events, tasks);
        let locator = SourceLocator::pdf(
            2,
            vec![PdfRect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 12.0,
            }],
            PageRotation::R0,
        );
        let pages = vec![PageOcrRecognitionPage {
            page_number: 2,
            status: "completed".into(),
            confidence: Some(0.9),
            engine_version: Some("test-engine".into()),
            error_code: None,
            blocks: vec![
                PageOcrRecognitionBlock {
                    text: "第一段识别文本".into(),
                    confidence: Some(0.9),
                    kind: Some("paragraph".into()),
                    locator: locator.clone(),
                },
                PageOcrRecognitionBlock {
                    text: "第二段识别文本".into(),
                    confidence: Some(0.8),
                    kind: Some("paragraph".into()),
                    locator: locator.clone(),
                },
            ],
        }];
        let statuses = service
            .save_page_ocr_recognition("d1", "v1", &pages, "k1")
            .unwrap();
        let page2 = statuses.iter().find(|row| row.page_number == 2).unwrap();
        assert_eq!(page2.status, PageOcrStatus::Completed);
        assert_eq!(page2.block_count, 2);
        // 只更新本次提交的页:未选中的 completed 页必须原样保留。
        let (blocks, fts, page2_status, page1_state, generation) = db
            .read(|conn| {
                let blocks: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM content_block WHERE parse_generation_id='g1' \
                         AND origin='ocr' AND indexable=1",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let fts: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM content_block_fts WHERE parse_generation_id='g1'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let page2_status: String = conn
                    .query_row(
                        "SELECT status FROM page_ocr WHERE document_version_id='v1' AND page_number=2",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let page1_state: String = conn
                    .query_row(
                        "SELECT status FROM page_ocr WHERE document_version_id='v1' AND page_number=1",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let generation: String = conn
                    .query_row("SELECT status FROM parse_generation WHERE id='g1'", [], |r| {
                        r.get(0)
                    })
                    .map_err(|e| db_app_error(&e))?;
                Ok((blocks, fts, page2_status, page1_state, generation))
            })
            .unwrap();
        assert_eq!(blocks, 2);
        assert_eq!(fts, 2);
        assert_eq!(page2_status, "completed");
        assert_eq!(page1_state, "completed");
        assert_eq!(generation, "partial");
        // 再识别同一页:按 block_key 更新,保留块 ID;少了的段软删除。
        let first_id: String = db
            .read(|conn| {
                conn.query_row(
                    "SELECT id FROM content_block WHERE parse_generation_id='g1' \
                     AND block_key='ocr:p2:l0'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))
            })
            .unwrap();
        let revised = vec![PageOcrRecognitionPage {
            page_number: 2,
            status: "low_confidence".into(),
            confidence: Some(0.4),
            engine_version: Some("test-engine".into()),
            error_code: None,
            blocks: vec![PageOcrRecognitionBlock {
                text: "第一段识别文本(修订)".into(),
                confidence: Some(0.4),
                kind: Some("paragraph".into()),
                locator: locator.clone(),
            }],
        }];
        let revised_statuses = service
            .save_page_ocr_recognition("d1", "v1", &revised, "k2")
            .unwrap();
        assert_eq!(
            revised_statuses
                .iter()
                .find(|row| row.page_number == 2)
                .unwrap()
                .status,
            PageOcrStatus::LowConfidence
        );
        let (same_id, text, indexable_deleted, fts_after) = db
            .read(|conn| {
                let same_id: String = conn
                    .query_row(
                        "SELECT id FROM content_block WHERE parse_generation_id='g1' \
                         AND block_key='ocr:p2:l0'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let text: String = conn
                    .query_row(
                        "SELECT text FROM content_block WHERE parse_generation_id='g1' \
                         AND block_key='ocr:p2:l0'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let indexable_deleted: i64 = conn
                    .query_row(
                        "SELECT indexable FROM content_block WHERE parse_generation_id='g1' \
                         AND block_key='ocr:p2:l1'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let fts_after: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM content_block_fts WHERE parse_generation_id='g1'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                Ok((same_id, text, indexable_deleted, fts_after))
            })
            .unwrap();
        assert_eq!(same_id, first_id, "同一 block_key 必须保留块 ID");
        assert_eq!(text, "第一段识别文本(修订)");
        assert_eq!(indexable_deleted, 0, "消失的块只软删除");
        assert_eq!(fts_after, 1);
        // 全文识别进行中禁止写入:返回稳定 reason,不覆盖代次。
        db.write(|conn| {
            conn.execute(
                "UPDATE parse_generation SET status='parsing' WHERE id='g1'",
                [],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
        .unwrap();
        let error = service
            .save_page_ocr_recognition("d1", "v1", &revised, "k3")
            .unwrap_err();
        assert!(matches!(
            error
                .details
                .as_ref()
                .and_then(|details| details.get("reason")),
            Some(crate::shared::error::DetailValue::Str(value)) if value == "parse_in_progress"
        ));
        drop(service);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn page_scoped_ocr_incremental_range_preserves_existing_and_is_idempotent() {
        use crate::parse_index::models::{
            PageOcrRecognitionBlock, PageOcrRecognitionPage, PageOcrStatus,
        };
        use crate::shared::locator::PdfRect;

        let path = std::env::temp_dir().join(format!(
            "kw-ocr-range-{}.sqlite3",
            crate::shared::SystemIdGenerator.new_entity_id()
        ));
        let db = Arc::new(Db::open(&path, 1).unwrap());
        db.write(|conn| {
            let migrations = crate::persistence::registration::migrations()
                .into_iter()
                .chain(crate::document::registration::migrations())
                .chain(crate::parse_index::registration::migrations());
            for migration in migrations {
                conn.execute_batch(migration.sql)
                    .map_err(|e| db_app_error(&e))?;
            }
            conn.execute_batch(
                "INSERT INTO document(id,display_name,format,current_version_id,created_at,updated_at)
                   VALUES ('d1','scan','pdf','v1',0,0);
                 INSERT INTO document_version(id,document_id,fingerprint_status,created_at)
                   VALUES ('v1','d1','ready',0);
                 INSERT INTO parse_generation(id,document_version_id,parser_config_hash,parser_version,
                   status,is_current,started_at)
                   VALUES ('g1','v1','hash','parser/1','canceled',1,0);",
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
        .unwrap();
        let clock = Arc::new(crate::shared::SystemClock);
        let ids = Arc::new(crate::shared::SystemIdGenerator);
        let events = Arc::new(crate::shared::NoopEventSink);
        let tasks = Arc::new(crate::persistence::task::TaskService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let service = ParseIndexService::new(db.clone(), clock, ids, events, tasks);

        let make_pages = |start: i64, end: i64| -> Vec<PageOcrRecognitionPage> {
            (start..=end)
                .map(|page_number| {
                    let locator = SourceLocator::pdf(
                        page_number,
                        vec![PdfRect {
                            x: 10.0,
                            y: 20.0,
                            width: 100.0,
                            height: 12.0,
                        }],
                        PageRotation::R0,
                    );
                    PageOcrRecognitionPage {
                        page_number,
                        status: "completed".into(),
                        confidence: Some(0.9),
                        engine_version: Some("test-engine".into()),
                        error_code: None,
                        blocks: vec![
                            PageOcrRecognitionBlock {
                                text: format!("page-{page_number}-a"),
                                confidence: Some(0.9),
                                kind: Some("paragraph".into()),
                                locator: locator.clone(),
                            },
                            PageOcrRecognitionBlock {
                                text: format!("page-{page_number}-b"),
                                confidence: Some(0.8),
                                kind: Some("paragraph".into()),
                                locator,
                            },
                        ],
                    }
                })
                .collect()
        };
        service
            .save_page_ocr_recognition("d1", "v1", &make_pages(1, 3), "k1")
            .unwrap();
        let ids_before: Vec<String> = db
            .read(|conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT id FROM content_block WHERE parse_generation_id='g1' \
                         AND origin='ocr' ORDER BY block_key",
                    )
                    .map_err(|e| db_app_error(&e))?;
                let rows = stmt
                    .query_map([], |r| r.get(0))
                    .map_err(|e| db_app_error(&e))?
                    .collect::<Result<Vec<String>, _>>()
                    .map_err(|e| db_app_error(&e))?;
                Ok(rows)
            })
            .unwrap();
        assert_eq!(ids_before.len(), 6);

        let statuses = service
            .save_page_ocr_recognition("d1", "v1", &make_pages(4, 10), "k2")
            .unwrap();
        assert_eq!(statuses.len(), 10);
        for page_number in 1..=3 {
            let row = statuses
                .iter()
                .find(|row| row.page_number == page_number)
                .unwrap();
            assert_eq!(row.status, PageOcrStatus::Completed);
            assert_eq!(row.block_count, 2);
        }
        for page_number in 4..=10 {
            let row = statuses
                .iter()
                .find(|row| row.page_number == page_number)
                .unwrap();
            assert_eq!(row.status, PageOcrStatus::Completed);
            assert_eq!(row.block_count, 2);
        }
        let ids_after: Vec<String> = db
            .read(|conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT id FROM content_block WHERE parse_generation_id='g1' \
                         AND origin='ocr' AND CAST(json_extract(locator_json, '$.pageNumber') \
                         AS INTEGER) BETWEEN 1 AND 3 ORDER BY block_key",
                    )
                    .map_err(|e| db_app_error(&e))?;
                let rows = stmt
                    .query_map([], |r| r.get(0))
                    .map_err(|e| db_app_error(&e))?
                    .collect::<Result<Vec<String>, _>>()
                    .map_err(|e| db_app_error(&e))?;
                Ok(rows)
            })
            .unwrap();
        assert_eq!(ids_before, ids_after, "补识别 4-10 不能改动 1-3 的块 ID");
        let (blocks, fts) = db
            .read(|conn| {
                let blocks: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM content_block WHERE parse_generation_id='g1' \
                         AND origin='ocr' AND indexable=1",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let fts: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM content_block_fts WHERE parse_generation_id='g1'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                Ok((blocks, fts))
            })
            .unwrap();
        assert_eq!(blocks, 20);
        assert_eq!(fts, 20);

        // 再次请求 1-3:幂等,不产生重复块,块 ID 不变。
        service
            .save_page_ocr_recognition("d1", "v1", &make_pages(1, 3), "k3")
            .unwrap();
        let (blocks2, fts2, ids_again) = db
            .read(|conn| {
                let blocks: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM content_block WHERE parse_generation_id='g1' \
                         AND origin='ocr' AND indexable=1",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let fts: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM content_block_fts WHERE parse_generation_id='g1'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let mut stmt = conn
                    .prepare(
                        "SELECT id FROM content_block WHERE parse_generation_id='g1' \
                         AND origin='ocr' AND CAST(json_extract(locator_json, '$.pageNumber') \
                         AS INTEGER) BETWEEN 1 AND 3 ORDER BY block_key",
                    )
                    .map_err(|e| db_app_error(&e))?;
                let ids = stmt
                    .query_map([], |r| r.get(0))
                    .map_err(|e| db_app_error(&e))?
                    .collect::<Result<Vec<String>, _>>()
                    .map_err(|e| db_app_error(&e))?;
                Ok((blocks, fts, ids))
            })
            .unwrap();
        assert_eq!(blocks2, 20);
        assert_eq!(fts2, 20);
        assert_eq!(ids_again, ids_before);

        // 新增第 11 页:只追加,不重建前 10 页。
        let statuses = service
            .save_page_ocr_recognition("d1", "v1", &make_pages(11, 11), "k4")
            .unwrap();
        assert_eq!(statuses.len(), 11);
        assert_eq!(
            statuses
                .iter()
                .find(|row| row.page_number == 11)
                .unwrap()
                .block_count,
            2
        );
        let (blocks3, fts3) = db
            .read(|conn| {
                let blocks: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM content_block WHERE parse_generation_id='g1' \
                         AND origin='ocr' AND indexable=1",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let fts: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM content_block_fts WHERE parse_generation_id='g1'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                Ok((blocks, fts))
            })
            .unwrap();
        assert_eq!(blocks3, 22);
        assert_eq!(fts3, 22);
        drop(service);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn page_scoped_ocr_skipped_persists_text_layer_status() {
        use crate::parse_index::models::{PageOcrRecognitionPage, PageOcrStatus};

        let path = std::env::temp_dir().join(format!(
            "kw-ocr-skipped-{}.sqlite3",
            crate::shared::SystemIdGenerator.new_entity_id()
        ));
        let db = Arc::new(Db::open(&path, 1).unwrap());
        db.write(|conn| {
            let migrations = crate::persistence::registration::migrations()
                .into_iter()
                .chain(crate::document::registration::migrations())
                .chain(crate::parse_index::registration::migrations());
            for migration in migrations {
                conn.execute_batch(migration.sql)
                    .map_err(|e| db_app_error(&e))?;
            }
            conn.execute_batch(
                "INSERT INTO document(id,display_name,format,current_version_id,created_at,updated_at)
                   VALUES ('d1','scan','pdf','v1',0,0);
                 INSERT INTO document_version(id,document_id,fingerprint_status,created_at)
                   VALUES ('v1','d1','ready',0);
                 INSERT INTO parse_generation(id,document_version_id,parser_config_hash,parser_version,
                   status,is_current,started_at)
                   VALUES ('g1','v1','hash','parser/1','canceled',1,0);",
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
        .unwrap();
        let clock = Arc::new(crate::shared::SystemClock);
        let ids = Arc::new(crate::shared::SystemIdGenerator);
        let events = Arc::new(crate::shared::NoopEventSink);
        let tasks = Arc::new(crate::persistence::task::TaskService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let service = ParseIndexService::new(db.clone(), clock, ids, events, tasks);
        service
            .save_page_ocr_recognition(
                "d1",
                "v1",
                &[PageOcrRecognitionPage {
                    page_number: 1,
                    status: "skipped".into(),
                    confidence: None,
                    engine_version: None,
                    error_code: None,
                    blocks: vec![],
                }],
                "k1",
            )
            .unwrap();
        let status = service.get_page_ocr_status("d1", "v1").unwrap();
        let row = status.iter().find(|row| row.page_number == 1).unwrap();
        assert_eq!(row.status, PageOcrStatus::Skipped);
        assert_eq!(row.block_count, 0);
        assert_eq!(row.generation_id.as_deref(), Some("g1"));
        assert!(row.reasons.iter().any(|reason| reason == "text_layer_ok"));
        let blocks: i64 = db
            .read(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM content_block WHERE parse_generation_id='g1' \
                     AND origin='ocr'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))
            })
            .unwrap();
        assert_eq!(blocks, 0);
        drop(service);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn page_ocr_report_pending_does_not_downgrade_completed_and_retry_keeps_pages() {
        use crate::parse_index::models::{PageOcrReportEntry, PageOcrStatus};
        use crate::shared::events::CollectingEventSink;

        let id = crate::shared::SystemIdGenerator.new_entity_id();
        let db_path = std::env::temp_dir().join(format!("kw-ocr-report-{id}.sqlite3"));
        let input_path = std::env::temp_dir().join(format!("kw-ocr-report-{id}.pdf"));
        std::fs::write(&input_path, b"%PDF-1.4\n").unwrap();
        let input_size = std::fs::metadata(&input_path).unwrap().len() as i64;
        let input_path_sql = input_path.to_string_lossy().to_string();
        let db = Arc::new(Db::open(&db_path, 1).unwrap());
        db.write(move |conn| {
            let migrations = crate::persistence::registration::migrations()
                .into_iter()
                .chain(crate::document::registration::migrations())
                .chain(crate::parse_index::registration::migrations());
            for migration in migrations {
                conn.execute_batch(migration.sql)
                    .map_err(|e| db_app_error(&e))?;
            }
            conn.execute_batch(
                "INSERT INTO document(id,display_name,format,current_version_id,created_at,updated_at)
                   VALUES ('d1','scan','pdf','v1',0,0);
                 INSERT INTO document_version(id,document_id,fingerprint_status,created_at)
                   VALUES ('v1','d1','ready',0);",
            )
            .map_err(|e| db_app_error(&e))?;
            conn.execute(
                "INSERT INTO file_location(id,document_id,observed_version_id,path,is_primary, \
                 last_seen_size,last_verified_at) VALUES ('l1','d1','v1',?1,1,?2,0)",
                params![input_path_sql, input_size],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
        .unwrap();
        let clock = Arc::new(crate::shared::SystemClock);
        let ids = Arc::new(crate::shared::SystemIdGenerator);
        let events = Arc::new(CollectingEventSink::default());
        let tasks = Arc::new(crate::persistence::task::TaskService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let service = ParseIndexService::new(db.clone(), clock, ids, events.clone(), tasks);
        let task = service
            .start_parse("d1", "v1", "test", "report-guard")
            .unwrap();
        let (token, generation_id) = {
            let captured = events.events.lock().unwrap();
            let request = captured
                .iter()
                .find(|event| event.event_type == DomainEventType::ParserRequest)
                .expect("parser.request emitted");
            (
                request.payload["token"].as_str().unwrap().to_string(),
                request.payload["generationId"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        };
        let generation_id_sql = generation_id.clone();
        db.write(move |conn| {
            conn.execute(
                "INSERT INTO page_ocr(document_version_id,page_number,parse_generation_id,status, \
                 reasons_json,block_count,attempt,updated_at) \
                 VALUES ('v1',1,?1,'completed','[]',1,0,0)",
                [&generation_id_sql],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
        .unwrap();
        let entry = |status: PageOcrStatus| PageOcrReportEntry {
            page_number: 1,
            status,
            quality: None,
            reasons: vec![],
            render_hash: None,
            engine_version: None,
            confidence: None,
            block_count: 0,
            error_code: None,
        };
        service
            .report_page_ocr(&token, &[entry(PageOcrStatus::Pending)])
            .unwrap();
        let row = service
            .get_page_ocr_status("d1", "v1")
            .unwrap()
            .into_iter()
            .find(|row| row.page_number == 1)
            .unwrap();
        assert_eq!(row.status, PageOcrStatus::Completed);
        assert_eq!(row.block_count, 1);
        assert_eq!(row.generation_id.as_deref(), Some(generation_id.as_str()));

        service
            .report_page_ocr(&token, &[entry(PageOcrStatus::OcrRunning)])
            .unwrap();
        let row = service
            .get_page_ocr_status("d1", "v1")
            .unwrap()
            .into_iter()
            .find(|row| row.page_number == 1)
            .unwrap();
        assert_eq!(row.status, PageOcrStatus::OcrRunning);

        service
            .report_page_ocr(
                &token,
                &[PageOcrReportEntry {
                    page_number: 1,
                    status: PageOcrStatus::Completed,
                    quality: None,
                    reasons: vec![],
                    render_hash: None,
                    engine_version: Some("test-engine".into()),
                    confidence: Some(0.9),
                    block_count: 1,
                    error_code: None,
                }],
            )
            .unwrap();
        let row = service
            .get_page_ocr_status("d1", "v1")
            .unwrap()
            .into_iter()
            .find(|row| row.page_number == 1)
            .unwrap();
        assert_eq!(row.status, PageOcrStatus::Completed);
        assert_eq!(row.block_count, 1);
        assert_eq!(row.generation_id.as_deref(), Some(generation_id.as_str()));

        // OCR 文本重跑允许变化：同 block_key 原地更新，不报 block_key_drift。
        let locator = SourceLocator::pdf(1, vec![], PageRotation::R0);
        let batch = |seq: i64, text: &str, ordinal: i64| ParserBatchInput {
            batch_seq: seq,
            blocks: vec![ParserBlockInput {
                block_key: "ocr:p1:l0".into(),
                ordinal,
                parent_block_key: None,
                kind: BlockKind::Paragraph,
                text: text.into(),
                heading_path: vec![],
                locator: locator.clone(),
                completeness: BlockCompleteness::Complete,
                indexable: true,
                origin: BlockOrigin::Ocr,
                ocr_confidence: Some(0.5),
            }],
            regions: vec![ParserRegionInput {
                region_key: "page:1".into(),
                region_kind: RegionKind::Page,
                status: RegionStatus::Complete,
                block_count: 1,
                order_confidence: OrderConfidence::High,
                error_code: None,
            }],
            unsupported_regions: vec![],
        };
        service
            .submit_batch(&token, batch(1, "old ocr text", 0))
            .unwrap();
        service
            .submit_batch(&token, batch(2, "new ocr text", 1))
            .unwrap();
        let generation_id_read = generation_id.clone();
        let (updated_text, updated_ordinal, updated_fts) = db
            .read(move |conn| {
                let text: String = conn
                    .query_row(
                        "SELECT text FROM content_block WHERE parse_generation_id=?1 \
                         AND block_key='ocr:p1:l0'",
                        [&generation_id_read],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let ordinal: i64 = conn
                    .query_row(
                        "SELECT ordinal FROM content_block WHERE parse_generation_id=?1 \
                         AND block_key='ocr:p1:l0'",
                        [&generation_id_read],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                let fts: String = conn
                    .query_row(
                        "SELECT text FROM content_block_fts WHERE block_id=(\
                           SELECT id FROM content_block WHERE parse_generation_id=?1 \
                           AND block_key='ocr:p1:l0')",
                        [&generation_id_read],
                        |r| r.get(0),
                    )
                    .map_err(|e| db_app_error(&e))?;
                Ok((text, ordinal, fts))
            })
            .unwrap();
        assert_eq!(updated_text, "new ocr text");
        assert_eq!(updated_ordinal, 1);
        assert_eq!(updated_fts, "new ocr text");

        // 取消后重试：同配置复用同一代次，page_ocr 不能被删除。
        let current = service.tasks.get_task(&task.id).unwrap().unwrap();
        service
            .cancel_parse(&current.id, current.state_version)
            .unwrap();
        service.retry_parse(&task.id).unwrap();
        let rows = service.get_page_ocr_status("d1", "v1").unwrap();
        let row = rows.iter().find(|row| row.page_number == 1).unwrap();
        assert_eq!(row.status, PageOcrStatus::Completed);
        assert_eq!(row.block_count, 1);
        assert_eq!(row.generation_id.as_deref(), Some(generation_id.as_str()));
        drop(service);
        drop(db);
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&db_path);
    }
}
