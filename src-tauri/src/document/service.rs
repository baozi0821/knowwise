//! M02 文档服务:导入、指纹、去重、文件检查、重连与版本确认。
//!
//! 关键不变量:
//! - 路径不是身份;Document ID 不因移动/重命名改变(M02-A3)。
//! - 内容变化只新建版本行,旧版本与其引用目标不变(DATA-01)。
//! - 同一内容重复导入不产生重复解析对象(M02-A5)。
//! - 事务内不做文件 I/O 与哈希计算(《01》§9);流式哈希在事务外执行。
//! - 访问令牌短期有效且绑定版本与大小;版本确认后重签(M02 §6)。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};

use crate::persistence::{db_app_error, Db};
use crate::shared::paging::{Page, PageRequest};
use crate::shared::tokens::{TokenError, TokenRegistry};
use crate::shared::{
    AppError, Clock, CoreErrorCode, DomainEvent, DomainEventType, EventSink, IdGenerator,
};

use super::detect::{detect_file, DetectedFile, SizeLimits};
use super::models::*;

const DOC_COLS: &str = "id, display_name, format, current_version_id, revision, created_at, updated_at, last_opened_at";
const VER_COLS: &str = "id, document_id, supersedes_version_id, content_hash, file_size, fingerprint_status, confirmed, created_at";
const LOC_COLS: &str = "id, document_id, observed_version_id, path, is_primary, last_seen_size, last_seen_mtime_ns, last_verified_at";

const TOKEN_TTL_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct TokenBinding {
    pub document_id: String,
    pub version_id: String,
    pub path: PathBuf,
    pub size: i64,
}

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

fn load_doc(conn: &Connection, id: &str) -> Result<Option<DocumentRow>, AppError> {
    let sql = format!("SELECT {DOC_COLS} FROM document WHERE id=?1");
    conn.query_row(&sql, [id], |r| DocumentRow::try_from(r))
        .optional()
        .map_err(|e| db_app_error(&e))
}

fn load_version(conn: &Connection, id: &str) -> Result<Option<VersionRow>, AppError> {
    let sql = format!("SELECT {VER_COLS} FROM document_version WHERE id=?1");
    conn.query_row(&sql, [id], |r| VersionRow::try_from(r))
        .optional()
        .map_err(|e| db_app_error(&e))
}

fn load_primary_location(
    conn: &Connection,
    document_id: &str,
) -> Result<Option<FileLocationRow>, AppError> {
    let sql = format!(
        "SELECT {LOC_COLS} FROM file_location WHERE document_id=?1 ORDER BY is_primary DESC, last_verified_at DESC LIMIT 1"
    );
    conn.query_row(&sql, [document_id], |r| FileLocationRow::try_from(r))
        .optional()
        .map_err(|e| db_app_error(&e))
}

fn load_location_by_path(
    conn: &Connection,
    path: &str,
) -> Result<Option<FileLocationRow>, AppError> {
    let sql = format!("SELECT {LOC_COLS} FROM file_location WHERE path=?1 LIMIT 1");
    conn.query_row(&sql, [path], |r| FileLocationRow::try_from(r))
        .optional()
        .map_err(|e| db_app_error(&e))
}

/// 流式 SHA-256(1 MiB 块)。在事务外执行。
fn stream_sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn fs_stat(path: &Path) -> std::io::Result<(u64, i64)> {
    let meta = std::fs::metadata(path)?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Ok((meta.len(), mtime_ns))
}

/// 规范化路径:尽量 canonicalize(符号链接折叠);失败退回绝对路径。
fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    })
}

pub struct DocumentService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    events: Arc<dyn EventSink>,
    tokens: TokenRegistry<TokenBinding>,
    limits: SizeLimits,
}

impl DocumentService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        events: Arc<dyn EventSink>,
        limits: SizeLimits,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            events,
            tokens: TokenRegistry::default(),
            limits,
        }
    }

    fn emit_document(&self, document: &DocumentRow, reason: &str) {
        self.emit_document_with(document, reason, serde_json::Value::Null);
    }

    fn emit_document_with(&self, document: &DocumentRow, reason: &str, extra: serde_json::Value) {
        let mut payload = serde_json::json!({
            "reason": reason,
            "documentId": document.id,
            "currentVersionId": document.current_version_id,
            "format": document.format.as_str(),
        });
        if let (Some(obj), serde_json::Value::Object(patch)) = (payload.as_object_mut(), extra) {
            for (k, v) in patch {
                obj.insert(k, v);
            }
        }
        let event = DomainEvent::new(
            DomainEventType::DocumentChanged,
            document.id.clone(),
            document.revision,
            self.clock.now_ms(),
            payload,
        )
        .with_document(
            Some(document.id.clone()),
            document.current_version_id.clone(),
        );
        self.events.emit(event);
    }

    /// 导入(同步形态,测试与批处理使用):建档 + 立即计算指纹。
    pub fn import_document(
        &self,
        path: &Path,
        idempotency_key: &str,
    ) -> Result<ImportResult, AppError> {
        let (mut result, doc_id, ver_id) = self.begin_import(path, idempotency_key)?;
        if result.fingerprint_status == FingerprintStatus::Computing {
            let outcome = self.finalize_import(&doc_id, &ver_id)?;
            result.version_id = outcome.final_version_id;
            result.import_kind = outcome.import_kind;
            result.duplicate_of_document_id = outcome.duplicate_of_document_id;
            result.fingerprint_status = outcome.fingerprint_status;
        }
        Ok(result)
    }

    /// 导入第一步:检测、建档(指纹 computing)、签发临时令牌后立即返回;
    /// 首屏不等待全量哈希(《M02》§3、A8)。返回 (ImportResult, 文档ID, 版本ID)。
    /// 已知路径的重复导入(重开/外部变化)在本地即完成,不走后台。
    pub fn begin_import(
        &self,
        path: &Path,
        _idempotency_key: &str,
    ) -> Result<(ImportResult, String, String), AppError> {
        let detected = detect_file(path, &self.limits)?;
        let norm = normalize_path(path);
        let norm_s = norm.to_string_lossy().to_string();
        let now = self.clock.now_ms();

        // 1) 已知路径:按《M02》§4 处理(重开或外部变化候选),不新建文档。
        let known = {
            let probe = norm_s.clone();
            self.db
                .read(move |conn| load_location_by_path(conn, &probe))?
        };
        if let Some(loc) = known {
            let result = self.import_known_path(loc, path, &detected, now)?;
            let doc = result.document_id.clone();
            let ver = result.version_id.clone();
            return Ok((result, doc, ver));
        }

        // 2) 新路径:建档(指纹 computing)。
        let document_id = self.ids.new_entity_id();
        let version_id = self.ids.new_entity_id();
        let location_id = self.ids.new_entity_id();
        let display_name = norm
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名文档".to_string());

        let doc = {
            let document_id = document_id.clone();
            let version_id = version_id.clone();
            let location_id = location_id.clone();
            let display_name = display_name.clone();
            let norm_s = norm_s.clone();
            self.db.write(move |conn| {
                let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
                tx.execute(
                    "INSERT INTO document(id, display_name, format, current_version_id, revision, \
                     created_at, updated_at, last_opened_at) VALUES (?1,?2,?3,NULL,1,?4,?4,?4)",
                    params![document_id, display_name, detected.format.as_str(), now],
                )
                .map_err(|e| db_app_error(&e))?;
                tx.execute(
                    "INSERT INTO document_version(id, document_id, supersedes_version_id, \
                     content_hash, file_size, fingerprint_status, confirmed, created_at) \
                     VALUES (?1,?2,NULL,NULL,?3,'computing',0,?4)",
                    params![version_id, document_id, detected.size as i64, now],
                )
                .map_err(|e| db_app_error(&e))?;
                tx.execute(
                    "INSERT INTO file_location(id, document_id, observed_version_id, path, \
                     is_primary, last_seen_size, last_seen_mtime_ns, last_verified_at) \
                     VALUES (?1,?2,NULL,?3,1,?4,?5,?6)",
                    params![
                        location_id,
                        document_id,
                        norm_s,
                        detected.size as i64,
                        detected.mtime_ns,
                        now
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
                // 循环外键:同事务补齐。
                tx.execute(
                    "UPDATE document SET current_version_id=?2 WHERE id=?1",
                    params![document_id, version_id],
                )
                .map_err(|e| db_app_error(&e))?;
                let doc = load_doc(&tx, &document_id)?.unwrap();
                tx.commit().map_err(|e| db_app_error(&e))?;
                Ok(doc)
            })?
        };
        self.emit_document(&doc, "imported");

        let token = self.issue_token(&TokenBinding {
            document_id: doc.id.clone(),
            version_id: version_id.clone(),
            path: norm,
            size: detected.size as i64,
        });

        Ok((
            ImportResult {
                document_id: doc.id.clone(),
                version_id: version_id.clone(),
                import_kind: ImportKind::NewDocument,
                duplicate_of_document_id: None,
                fingerprint_status: FingerprintStatus::Computing,
                access_token: token,
            },
            document_id,
            version_id,
        ))
    }

    /// 导入第二步:后台流式哈希 + 事务内定稿(去重裁决、置 ready)。
    /// 结果经 `document.changed` 事件携带 importKind/duplicateOfDocumentId。
    pub fn finalize_import(
        &self,
        document_id: &str,
        version_id: &str,
    ) -> Result<FingerprintOutcome, AppError> {
        let outcome = self.compute_fingerprint(document_id, version_id)?;
        if let Some(doc) = self.db.read({
            let did = document_id.to_string();
            move |conn| load_doc(conn, &did)
        })? {
            self.emit_document_with(
                &doc,
                "fingerprint_ready",
                serde_json::json!({
                    "versionId": outcome.final_version_id,
                    "importKind": serde_json::to_value(outcome.import_kind).unwrap_or_default(),
                    "duplicateOfDocumentId": outcome.duplicate_of_document_id,
                }),
            );
        }
        Ok(outcome)
    }

    /// 启动恢复:导入中途崩溃遗留的 computing 版本标记 unreadable,
    /// 文档与既有资产保留(《M02》§3)。返回受影响文档 ID。
    pub fn recover_imports(&self) -> Result<Vec<String>, AppError> {
        self.db.write(|conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let mut stmt = tx
                .prepare(
                    "SELECT DISTINCT document_id FROM document_version \
                     WHERE fingerprint_status='computing'",
                )
                .map_err(|e| db_app_error(&e))?;
            let ids: Vec<String> = stmt
                .query_map([], |r| r.get(0))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            if !ids.is_empty() {
                tx.execute(
                    "UPDATE document_version SET fingerprint_status='unreadable' \
                     WHERE fingerprint_status='computing'",
                    [],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(ids)
        })
    }

    /// 已知路径的导入:内容相同 → 重开;内容变化 → 在原文档上建候选版本。
    fn import_known_path(
        &self,
        loc: FileLocationRow,
        raw_path: &Path,
        detected: &DetectedFile,
        now: i64,
    ) -> Result<ImportResult, AppError> {
        let hash = stream_sha256(raw_path).map_err(|_| err(CoreErrorCode::DocFileMissing))?;
        let document_id = loc.document_id.clone();
        let path_s = loc.path.clone();
        let size = detected.size as i64;
        let mtime = detected.mtime_ns;
        let candidate_id = self.ids.new_entity_id();

        let result = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let doc =
                load_doc(&tx, &document_id)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
            // 该路径当前观察版本的哈希。
            let observed_hash: Option<String> = match &loc.observed_version_id {
                Some(vid) => load_version(&tx, vid)?.and_then(|v| v.content_hash),
                None => None,
            };
            if observed_hash.as_deref() == Some(hash.as_str()) {
                // 相同内容:重开,更新观察信息与时间。
                tx.execute(
                    "UPDATE document SET last_opened_at=?2, updated_at=?2 WHERE id=?1",
                    params![document_id, now],
                )
                .map_err(|e| db_app_error(&e))?;
                tx.execute(
                    "UPDATE file_location SET last_seen_size=?2, last_seen_mtime_ns=?3, \
                     last_verified_at=?4 WHERE id=?1",
                    params![loc.id, size, mtime, now],
                )
                .map_err(|e| db_app_error(&e))?;
                let version_id = doc
                    .current_version_id
                    .clone()
                    .or(loc.observed_version_id.clone());
                tx.commit().map_err(|e| db_app_error(&e))?;
                return Ok((
                    doc,
                    version_id,
                    ImportKind::ConnectedExisting,
                    None,
                    FingerprintStatus::Ready,
                ));
            }
            // 内容变化:创建候选版本(未确认),当前版本指针不动。
            tx.execute(
                "INSERT INTO document_version(id, document_id, supersedes_version_id, \
                 content_hash, file_size, fingerprint_status, confirmed, created_at) \
                 VALUES (?1,?2,?3,?4,?5,'ready',0,?6)",
                params![
                    candidate_id,
                    document_id,
                    doc.current_version_id,
                    hash,
                    size,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "UPDATE file_location SET observed_version_id=?2, last_seen_size=?3, \
                 last_seen_mtime_ns=?4, last_verified_at=?5 WHERE id=?1",
                params![loc.id, candidate_id, size, mtime, now],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "UPDATE document SET updated_at=?2, last_opened_at=?2 WHERE id=?1",
                params![document_id, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let doc = load_doc(&tx, &document_id)?.unwrap();
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok((
                doc,
                Some(candidate_id),
                ImportKind::ConnectedExisting,
                None,
                FingerprintStatus::Ready,
            ))
        })?;

        let (doc, version_id, kind, dup, fp) = result;
        let reason =
            if kind == ImportKind::ConnectedExisting && version_id != doc.current_version_id {
                "candidate_created"
            } else {
                "reopened"
            };
        self.emit_document(&doc, reason);
        let version_id = version_id.ok_or_else(|| err(CoreErrorCode::Internal))?;
        let token = self.issue_token(&TokenBinding {
            document_id: doc.id.clone(),
            version_id: version_id.clone(),
            path: PathBuf::from(path_s),
            size,
        });
        Ok(ImportResult {
            document_id: doc.id,
            version_id,
            import_kind: kind,
            duplicate_of_document_id: dup,
            fingerprint_status: fp,
            access_token: token,
        })
    }

    /// 指纹计算与定稿:computing → ready / unreadable;跨文档同内容由
    /// 上层报告为重复,不静默合并(《M02》§4)。
    pub fn compute_fingerprint(
        &self,
        document_id: &str,
        version_id: &str,
    ) -> Result<FingerprintOutcome, AppError> {
        let binding = {
            let did = document_id.to_string();
            let vid = version_id.to_string();
            self.db.read(move |conn| {
                let loc = load_primary_location(conn, &did)?
                    .ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
                let ver = load_version(conn, &vid)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
                Ok((loc, ver))
            })?
        };
        let (loc, _ver) = binding;
        let path = PathBuf::from(&loc.path);

        // 事务外流式哈希。
        let hash_result = stream_sha256(&path);
        let now = self.clock.now_ms();
        let document_id = document_id.to_string();
        let version_id = version_id.to_string();
        let doc_id_for_event = document_id.clone();

        let outcome = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let doc = load_doc(&tx, &document_id)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
            match hash_result {
                Err(_) => {
                    // 文件不可读:只改状态,不删除文档与资产(《M02》§5)。
                    tx.execute(
                        "UPDATE document_version SET fingerprint_status='unreadable' WHERE id=?1",
                        params![version_id],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    let ver = load_version(&tx, &version_id)?.unwrap();
                    tx.commit().map_err(|e| db_app_error(&e))?;
                    Ok(FingerprintOutcome {
                        final_version_id: version_id,
                        import_kind: ImportKind::NewDocument,
                        duplicate_of_document_id: None,
                        fingerprint_status: ver.fingerprint_status,
                    })
                }
                Ok(hash) => {
                    let (size, mtime) = fs_stat(&path).map(|(s, m)| (s as i64, m)).unwrap_or((0, 0));
                    // 同文档内已有相同内容版本:删除本次临时行,复用旧版本。
                    let dup_same_doc: Option<String> = tx
                        .query_row(
                            "SELECT id FROM document_version WHERE document_id=?1 \
                             AND content_hash=?2 AND id != ?3 LIMIT 1",
                            params![document_id, hash, version_id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(|e| db_app_error(&e))?;
                    if let Some(existing) = dup_same_doc {
                        // 未产生任何解析对象前清理临时版本;位置指向旧版本。
                        tx.execute(
                            "DELETE FROM page_ocr WHERE document_version_id=?1",
                            params![version_id],
                        )
                        .map_err(|e| db_app_error(&e))?;
                        tx.execute(
                            "DELETE FROM page_ocr_control WHERE document_version_id=?1",
                            params![version_id],
                        )
                        .map_err(|e| db_app_error(&e))?;
                        tx.execute("DELETE FROM document_version WHERE id=?1", params![version_id])
                            .map_err(|e| db_app_error(&e))?;
                        tx.execute(
                            "UPDATE file_location SET observed_version_id=?2, last_verified_at=?3 \
                             WHERE document_id=?1 AND observed_version_id IS NULL",
                            params![document_id, existing, now],
                        )
                        .map_err(|e| db_app_error(&e))?;
                        if doc.current_version_id.as_deref() == Some(version_id.as_str()) {
                            tx.execute(
                                "UPDATE document SET current_version_id=?2, updated_at=?3 WHERE id=?1",
                                params![document_id, existing, now],
                            )
                            .map_err(|e| db_app_error(&e))?;
                        }
                        tx.commit().map_err(|e| db_app_error(&e))?;
                        return Ok(FingerprintOutcome {
                            final_version_id: existing,
                            import_kind: ImportKind::ConnectedExisting,
                            duplicate_of_document_id: None,
                            fingerprint_status: FingerprintStatus::Ready,
                        });
                    }

                    // 跨文档重复:保留独立身份,仅报告(M02 §4)。
                    let dup_other_doc: Option<String> = tx
                        .query_row(
                            "SELECT document_id FROM document_version WHERE content_hash=?1 \
                             AND document_id != ?2 AND fingerprint_status='ready' LIMIT 1",
                            params![hash, document_id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(|e| db_app_error(&e))?;

                    tx.execute(
                        "UPDATE document_version SET content_hash=?2, file_size=?3, \
                         fingerprint_status='ready', confirmed=1 WHERE id=?1",
                        params![version_id, hash, size],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    tx.execute(
                        "UPDATE file_location SET observed_version_id=?1, last_seen_size=?2, \
                         last_seen_mtime_ns=?3, last_verified_at=?4 WHERE document_id=?5 \
                         AND observed_version_id IS NULL",
                        params![version_id, size, mtime, now, document_id],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    tx.execute(
                        "UPDATE document SET last_opened_at=?2, updated_at=?2 WHERE id=?1",
                        params![document_id, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    tx.commit().map_err(|e| db_app_error(&e))?;
                    Ok(FingerprintOutcome {
                        final_version_id: version_id,
                        import_kind: if dup_other_doc.is_some() {
                            ImportKind::DuplicateOfOtherDocument
                        } else {
                            ImportKind::NewDocument
                        },
                        duplicate_of_document_id: dup_other_doc,
                        fingerprint_status: FingerprintStatus::Ready,
                    })
                }
            }
        })?;
        // 事件由 finalize_import 统一发出(携带去重结论),此处保持纯数据操作。
        let _ = doc_id_for_event;
        Ok(outcome)
    }

    fn issue_token(
        &self,
        binding: &TokenBinding,
    ) -> Option<crate::shared::dto::AccessTokenDescriptor> {
        let token = self.ids.new_token();
        let now = self.clock.now_ms();
        self.tokens
            .issue(&token, binding.clone(), now, TOKEN_TTL_MS);
        Some(crate::shared::dto::AccessTokenDescriptor {
            token,
            expires_at: now + TOKEN_TTL_MS,
        })
    }

    /// 校验访问令牌:未知或过期 → FILE_ACCESS_EXPIRED。
    pub fn validate_token(&self, token: &str) -> Result<TokenBinding, AppError> {
        let now = self.clock.now_ms();
        self.tokens.validate(token, now).map_err(|e| match e {
            TokenError::UnknownOrExpired => err(CoreErrorCode::FileAccessExpired),
        })
    }

    /// 最近文档:按 last_opened_at(缺省 created_at)倒序,不返回正文。
    pub fn list_recent(&self, page: PageRequest) -> Result<Page<DocumentSummary>, AppError> {
        let page = page.normalized();
        let limit = page.limit;
        self.db.read(move |conn| {
            let mut sql = format!("SELECT {DOC_COLS} FROM document WHERE 1=1");
            let mut binds: Vec<rusqlite::types::Value> = Vec::new();
            if let Some(cur) = &page.cursor {
                let (ts, id): (i64, String) = crate::shared::paging::decode_cursor(cur)
                    .ok_or_else(|| err(CoreErrorCode::InvalidRequest))?;
                sql.push_str(&format!(
                    " AND (COALESCE(last_opened_at, created_at), id) < (?{}, ?{})",
                    binds.len() + 1,
                    binds.len() + 2
                ));
                binds.push(rusqlite::types::Value::Integer(ts));
                binds.push(rusqlite::types::Value::Text(id));
            }
            sql.push_str(&format!(
                " ORDER BY COALESCE(last_opened_at, created_at) DESC, id DESC LIMIT ?{}",
                binds.len() + 1
            ));
            binds.push(rusqlite::types::Value::Integer(limit + 1));

            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let rows: Vec<DocumentRow> = stmt
                .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                    DocumentRow::try_from(r)
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;

            let mut items = Vec::with_capacity(rows.len());
            for d in &rows {
                let state = derive_file_state(conn, d)?;
                items.push(summary_of(d, state, conn)?);
            }
            let out = Page::from_probe(items, limit, |s| {
                crate::shared::paging::encode_cursor(&(
                    s.last_opened_at.unwrap_or(s.created_at),
                    s.id.clone(),
                ))
            });
            Ok(Page {
                items: out.items,
                next_cursor: out.next_cursor,
                total: None,
            })
        })
    }

    /// 文档详情:元数据 + 版本列表(不含正文与完整路径)。
    pub fn get_document(&self, document_id: &str) -> Result<DocumentDetail, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            let doc = load_doc(conn, &did)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
            let state = derive_file_state(conn, &doc)?;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {VER_COLS} FROM document_version WHERE document_id=?1 ORDER BY created_at DESC, id DESC"
                ))
                .map_err(|e| db_app_error(&e))?;
            let versions: Vec<VersionRow> = stmt
                .query_map([&did], |r| VersionRow::try_from(r))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(DocumentDetail {
                summary: summary_of(&doc, state, conn)?,
                versions: versions.iter().map(|v| v.to_summary()).collect(),
            })
        })
    }

    /// 打开文档:校验文件可读,签发短期只读令牌(指纹未完成也可临时打开)。
    pub fn open_document(&self, document_id: &str) -> Result<DocumentOpenTokenDto, AppError> {
        let did = document_id.to_string();
        let now = self.clock.now_ms();
        let (doc, loc, version) = self.db.read(move |conn| {
            let doc = load_doc(conn, &did)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
            let loc = load_primary_location(conn, &did)?
                .ok_or_else(|| err(CoreErrorCode::DocFileMissing))?;
            let vid = doc
                .current_version_id
                .clone()
                .or(loc.observed_version_id.clone());
            let version = match vid {
                Some(v) => load_version(conn, &v)?,
                None => None,
            }
            .ok_or_else(|| err(CoreErrorCode::Internal))?;
            Ok((doc, loc, version))
        })?;

        let path = PathBuf::from(&loc.path);
        let (size, _) = fs_stat(&path).map_err(|_| err(CoreErrorCode::DocFileMissing))?;
        let version_id = version.id.clone();

        // 重开触摸时间。
        self.db.write({
            let did = doc.id.clone();
            move |conn| {
                conn.execute(
                    "UPDATE document SET last_opened_at=?2, updated_at=?2 WHERE id=?1",
                    params![did, now],
                )
                .map_err(|e| db_app_error(&e))?;
                Ok(())
            }
        })?;

        let descriptor = self
            .issue_token(&TokenBinding {
                document_id: doc.id.clone(),
                version_id: version_id.clone(),
                path,
                size: size as i64,
            })
            .ok_or_else(|| err(CoreErrorCode::Internal))?;

        Ok(DocumentOpenTokenDto {
            token: descriptor.token,
            expires_at: descriptor.expires_at,
            document_id: doc.id,
            version_id,
            file_size: size as i64,
            format: doc.format,
        })
    }

    /// 检查文件:缺失/未变/外部变化(候选版本,幂等)。
    pub fn check_file(&self, document_id: &str) -> Result<FileCheckResult, AppError> {
        let did = document_id.to_string();
        let now = self.clock.now_ms();
        let (doc, loc) = self.db.read({
            let did = did.clone();
            move |conn| {
                let doc = load_doc(conn, &did)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
                let loc = load_primary_location(conn, &did)?
                    .ok_or_else(|| err(CoreErrorCode::DocFileMissing))?;
                Ok((doc, loc))
            }
        })?;

        let path = PathBuf::from(&loc.path);
        let stat = match fs_stat(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(FileCheckResult {
                    document_id: doc.id,
                    status: FileCheckStatus::Missing,
                    current_version_id: doc.current_version_id,
                    candidate_version_id: None,
                });
            }
            Err(_) => {
                return Ok(FileCheckResult {
                    document_id: doc.id,
                    status: FileCheckStatus::Unreadable,
                    current_version_id: doc.current_version_id,
                    candidate_version_id: None,
                });
            }
        };

        // 大小与 mtime 未变:视为未变(不做全量哈希)。
        if loc.last_seen_size == Some(stat.0 as i64) && loc.last_seen_mtime_ns == Some(stat.1) {
            self.db.write({
                let lid = loc.id.clone();
                move |conn| {
                    conn.execute(
                        "UPDATE file_location SET last_verified_at=?2 WHERE id=?1",
                        params![lid, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    Ok(())
                }
            })?;
            return Ok(FileCheckResult {
                document_id: doc.id,
                status: FileCheckStatus::Unchanged,
                current_version_id: doc.current_version_id,
                candidate_version_id: None,
            });
        }

        // 有变化:重算哈希对比。
        self.ensure_candidate_for_change(&doc, &loc, &path, stat, now)
    }

    /// 内容变化 → 幂等创建候选版本;哈希与当前版本相同则只刷新观察信息。
    fn ensure_candidate_for_change(
        &self,
        doc: &DocumentRow,
        loc: &FileLocationRow,
        path: &Path,
        stat: (u64, i64),
        now: i64,
    ) -> Result<FileCheckResult, AppError> {
        let hash = stream_sha256(path).map_err(|_| err(CoreErrorCode::DocFileMissing))?;
        let doc_id = doc.id.clone();
        let loc_id = loc.id.clone();
        let current_version_id = doc.current_version_id.clone();
        let size = stat.0 as i64;
        let mtime = stat.1;
        let candidate_seed = self.ids.new_entity_id();

        let result = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let doc = load_doc(&tx, &doc_id)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
            let current_hash: Option<String> = match &current_version_id {
                Some(vid) => load_version(&tx, vid)?.and_then(|v| v.content_hash),
                None => None,
            };
            if current_hash.as_deref() == Some(hash.as_str()) {
                tx.execute(
                    "UPDATE file_location SET last_seen_size=?2, last_seen_mtime_ns=?3, \
                     last_verified_at=?4 WHERE id=?1",
                    params![loc_id, size, mtime, now],
                )
                .map_err(|e| db_app_error(&e))?;
                tx.commit().map_err(|e| db_app_error(&e))?;
                return Ok(FileCheckResult {
                    document_id: doc.id,
                    status: FileCheckStatus::Unchanged,
                    current_version_id: doc.current_version_id,
                    candidate_version_id: None,
                });
            }
            // 幂等:同哈希未确认候选已存在则复用(外部连续触发)。
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM document_version WHERE document_id=?1 AND content_hash=?2 \
                     AND confirmed=0 LIMIT 1",
                    params![doc_id, hash],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let candidate = match &existing {
                Some(e) => e.clone(),
                None => candidate_seed.clone(),
            };
            if existing.is_none() {
                tx.execute(
                    "INSERT INTO document_version(id, document_id, supersedes_version_id, \
                     content_hash, file_size, fingerprint_status, confirmed, created_at) \
                     VALUES (?1,?2,?3,?4,?5,'ready',0,?6)",
                    params![candidate, doc_id, doc.current_version_id, hash, size, now],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            tx.execute(
                "UPDATE file_location SET observed_version_id=?2, last_seen_size=?3, \
                 last_seen_mtime_ns=?4, last_verified_at=?5 WHERE id=?1",
                params![loc_id, candidate, size, mtime, now],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "UPDATE document SET updated_at=?2 WHERE id=?1",
                params![doc_id, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let doc = load_doc(&tx, &doc_id)?.unwrap();
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(FileCheckResult {
                document_id: doc.id.clone(),
                status: FileCheckStatus::ChangedPendingConfirm,
                current_version_id: doc.current_version_id,
                candidate_version_id: Some(candidate),
            })
        })?;
        if result.status == FileCheckStatus::ChangedPendingConfirm {
            if let Some(d) = self.db.read({
                let id = result.document_id.clone();
                move |conn| load_doc(conn, &id)
            })? {
                self.emit_document(&d, "candidate_created");
            }
        }
        Ok(result)
    }

    /// 重连:路径移动/重命名后指向新位置。内容相同保留原版本;
    /// 内容不同返回待确认候选,不静默切换(《M02》§5)。
    pub fn reconnect(
        &self,
        document_id: &str,
        new_path: &Path,
        _idempotency_key: &str,
    ) -> Result<FileCheckResult, AppError> {
        let did = document_id.to_string();
        let detected = detect_file(new_path, &self.limits)?;
        let doc_format = self.db.read({
            let did = did.clone();
            move |conn| {
                let doc = load_doc(conn, &did)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
                Ok(doc.format)
            }
        })?;
        if detected.format != doc_format {
            return Err(err(CoreErrorCode::DocUnsupportedFormat));
        }
        let norm = normalize_path(new_path);
        let now = self.clock.now_ms();
        let (doc, old_loc) = self.db.read({
            let did = did.clone();
            move |conn| {
                let doc = load_doc(conn, &did)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
                let loc = load_primary_location(conn, &did)?
                    .ok_or_else(|| err(CoreErrorCode::DocFileMissing))?;
                Ok((doc, loc))
            }
        })?;

        let stat = fs_stat(&norm).map_err(|_| err(CoreErrorCode::DocFileMissing))?;
        let hash = stream_sha256(&norm).map_err(|_| err(CoreErrorCode::DocFileMissing))?;

        let doc_id = doc.id.clone();
        let old_path = old_loc.path.clone();
        let old_loc_id = old_loc.id.clone();
        let new_path_s = norm.to_string_lossy().to_string();
        let new_loc_id = self.ids.new_entity_id();
        let current_version_id = doc.current_version_id.clone();
        let size = stat.0 as i64;
        let mtime = stat.1;
        let candidate_seed = self.ids.new_entity_id();

        let result =
            self.db.write(move |conn| {
                let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
                let doc = load_doc(&tx, &doc_id)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
                let current_hash: Option<String> = match &current_version_id {
                    Some(vid) => load_version(&tx, vid)?.and_then(|v| v.content_hash),
                    None => None,
                };

                // 旧位置降级(保留记录),新位置成为主位置。
                tx.execute(
                    "UPDATE file_location SET is_primary=0 WHERE id=?1",
                    params![old_loc_id],
                )
                .map_err(|e| db_app_error(&e))?;
                let replaced = tx
                .execute(
                    "INSERT OR REPLACE INTO file_location(id, document_id, observed_version_id, \
                     path, is_primary, last_seen_size, last_seen_mtime_ns, last_verified_at) \
                     VALUES (?1,?2,?3,?4,1,?5,?6,?7)",
                    params![new_loc_id, doc_id, current_version_id, new_path_s, size, mtime, now],
                )
                .map_err(|e| db_app_error(&e))?;
                let _ = replaced;

                if current_hash.as_deref() == Some(hash.as_str()) {
                    // 哈希相同:保留原版本,仅更新位置。
                    tx.execute(
                        "UPDATE document SET last_opened_at=?2, updated_at=?2 WHERE id=?1",
                        params![doc_id, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    let doc = load_doc(&tx, &doc_id)?.unwrap();
                    tx.commit().map_err(|e| db_app_error(&e))?;
                    Ok(FileCheckResult {
                        document_id: doc.id,
                        status: FileCheckStatus::Unchanged,
                        current_version_id: doc.current_version_id,
                        candidate_version_id: None,
                    })
                } else {
                    // 内容不同:待确认候选(复用同哈希候选,幂等)。
                    let existing: Option<String> = tx
                    .query_row(
                        "SELECT id FROM document_version WHERE document_id=?1 AND content_hash=?2 \
                         AND confirmed=0 LIMIT 1",
                        params![doc_id, hash],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| db_app_error(&e))?;
                    let candidate = match &existing {
                        Some(e) => e.clone(),
                        None => candidate_seed.clone(),
                    };
                    if existing.is_none() {
                        tx.execute(
                            "INSERT INTO document_version(id, document_id, supersedes_version_id, \
                         content_hash, file_size, fingerprint_status, confirmed, created_at) \
                         VALUES (?1,?2,?3,?4,?5,'ready',0,?6)",
                            params![candidate, doc_id, doc.current_version_id, hash, size, now],
                        )
                        .map_err(|e| db_app_error(&e))?;
                    }
                    tx.execute(
                        "UPDATE file_location SET observed_version_id=?2 WHERE id=?1",
                        params![new_loc_id, candidate],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    let doc = load_doc(&tx, &doc_id)?.unwrap();
                    tx.commit().map_err(|e| db_app_error(&e))?;
                    Ok(FileCheckResult {
                        document_id: doc.id,
                        status: FileCheckStatus::ChangedPendingConfirm,
                        current_version_id: doc.current_version_id,
                        candidate_version_id: Some(candidate),
                    })
                }
            })?;

        // 位置变化后旧令牌重签(《M02》§6)。
        self.tokens
            .revoke_where(|b| b.document_id == result.document_id);
        if let Some(d) = self.db.read({
            let id = result.document_id.clone();
            move |conn| load_doc(conn, &id)
        })? {
            let reason = if result.status == FileCheckStatus::Unchanged {
                "reconnected"
            } else {
                "candidate_created"
            };
            self.emit_document(&d, reason);
        }
        let _ = old_path;
        Ok(result)
    }

    /// 确认采用新版本:切换当前指针;旧版本与其引用不动(DATA-01)。
    /// expectedRevision 冲突 → CONFLICT_REVISION_MISMATCH,不强行覆盖。
    pub fn confirm_version(
        &self,
        document_id: &str,
        version_id: &str,
        expected_revision: i64,
    ) -> Result<DocumentDetail, AppError> {
        let did = document_id.to_string();
        let vid = version_id.to_string();
        let now = self.clock.now_ms();

        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let doc = load_doc(&tx, &did)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
            if doc.revision != expected_revision {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch));
            }
            let ver = load_version(&tx, &vid)?
                .filter(|v| v.document_id == did)
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if ver.fingerprint_status != FingerprintStatus::Ready || ver.content_hash.is_none() {
                return Err(err(CoreErrorCode::DocVersionUnconfirmed));
            }
            tx.execute(
                "UPDATE document_version SET confirmed=1 WHERE id=?1",
                params![vid],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "UPDATE document SET current_version_id=?2, revision=revision+1, \
                 updated_at=?3, last_opened_at=?3 WHERE id=?1",
                params![did, vid, now],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "UPDATE file_location SET observed_version_id=?2, last_verified_at=?3 \
                 WHERE document_id=?1 AND is_primary=1",
                params![did, vid, now],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(())
        })?;

        // 版本确认后令牌重签。
        self.tokens.revoke_where(|b| b.document_id == document_id);
        let doc = self
            .db
            .read({
                let did = document_id.to_string();
                move |conn| load_doc(conn, &did)
            })?
            .ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
        self.emit_document(&doc, "version_confirmed");
        self.get_document(document_id)
    }
}

#[derive(Debug, Clone)]
pub struct FingerprintOutcome {
    pub final_version_id: String,
    pub import_kind: ImportKind,
    pub duplicate_of_document_id: Option<String>,
    pub fingerprint_status: FingerprintStatus,
}

// —— 内部辅助 ——

fn derive_file_state(conn: &Connection, doc: &DocumentRow) -> Result<FileState, AppError> {
    let Some(loc) = load_primary_location(conn, &doc.id)? else {
        return Ok(FileState::Missing);
    };
    match fs_stat(Path::new(&loc.path)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileState::Missing),
        Err(_) => Ok(FileState::Unreadable),
        Ok((size, mtime)) => {
            if loc.last_seen_size == Some(size as i64) && loc.last_seen_mtime_ns == Some(mtime) {
                Ok(FileState::Present)
            } else {
                Ok(FileState::ChangedPendingConfirm)
            }
        }
    }
}

fn summary_of(
    doc: &DocumentRow,
    state: FileState,
    conn: &Connection,
) -> Result<DocumentSummary, AppError> {
    let fp = match &doc.current_version_id {
        Some(vid) => load_version(conn, vid)?.map(|v| v.fingerprint_status),
        None => None,
    };
    Ok(DocumentSummary {
        id: doc.id.clone(),
        display_name: doc.display_name.clone(),
        format: doc.format,
        current_version_id: doc.current_version_id.clone(),
        revision: doc.revision,
        file_state: state,
        fingerprint_status: fp,
        created_at: doc.created_at,
        updated_at: doc.updated_at,
        last_opened_at: doc.last_opened_at,
    })
}

impl DocumentService {
    /// 删除文档及其全部关联数据(阅读位置/笔记/标注/锚点/索引/任务/会话/
    /// 产物/授权/文件位置,单事务级联)。外键全开:严格按依赖序删除,
    /// 自引用列(current_version_id/supersedes_version_id/parent_block_id/
    /// current_attempt_id)先置空。
    pub fn delete_document(&self, document_id: &str) -> Result<(), AppError> {
        let id = document_id.to_string();
        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let step = |sql: &str| -> Result<(), crate::shared::AppError> {
                tx.execute(sql, rusqlite::params![id]).map_err(|e| db_app_error(&e))?;
                Ok(())
            };
            // 自引用/前置置空。
            step("UPDATE document SET current_version_id=NULL WHERE id=?1")?;
            step("UPDATE document_version SET supersedes_version_id=NULL WHERE document_id=?1")?;
            step("UPDATE content_block SET parent_block_id=NULL WHERE document_version_id IN                  (SELECT id FROM document_version WHERE document_id=?1)")?;
            // 检索链(经 task)。
            step("DELETE FROM retrieval_candidate WHERE run_id IN                  (SELECT id FROM retrieval_run WHERE task_id IN                  (SELECT id FROM task WHERE document_id=?1))")?;
            step("DELETE FROM retrieval_run WHERE task_id IN                  (SELECT id FROM task WHERE document_id=?1)")?;
            // 产物链(在快照/任务之前)。
            step("DELETE FROM quiz_question_feedback WHERE artifact_version_id IN                  (SELECT av.id FROM artifact_version av JOIN artifact a ON av.artifact_id=a.id                  WHERE a.document_id=?1)")?;
            step("DELETE FROM comprehension_record WHERE artifact_version_id IN                  (SELECT av.id FROM artifact_version av JOIN artifact a ON av.artifact_id=a.id                  WHERE a.document_id=?1)")?;
            step("DELETE FROM export_record WHERE document_id=?1")?;
            step("DELETE FROM artifact_version WHERE artifact_id IN                  (SELECT id FROM artifact WHERE document_id=?1)")?;
            step("DELETE FROM artifact WHERE document_id=?1")?;
            // 会话与消息(active_snapshot_id -> 快照;先删会话)。
            step("DELETE FROM message WHERE conversation_id IN                  (SELECT id FROM conversation WHERE document_id=?1)")?;
            step("DELETE FROM conversation WHERE document_id=?1")?;
            // 上下文快照链(parse_generation_id -> 解析代次)。
            step("DELETE FROM context_source WHERE snapshot_id IN                  (SELECT id FROM context_snapshot WHERE document_id=?1)")?;
            step("DELETE FROM snapshot_block WHERE snapshot_id IN                  (SELECT id FROM context_snapshot WHERE document_id=?1)")?;
            step("DELETE FROM context_snapshot WHERE document_id=?1")?;
            // 证据与标注链(evidence_feedback -> citation -> 标注)。
            step("DELETE FROM evidence_feedback WHERE citation_id IN                  (SELECT id FROM citation WHERE document_id=?1)")?;
            step("DELETE FROM citation WHERE document_id=?1")?;
            step("DELETE FROM annotation WHERE document_id=?1")?;
            // 笔记链(note_tag -> note; note 引用 anchor,故必须在 anchor 之前删除)。
            step("DELETE FROM note_tag WHERE note_id IN (SELECT id FROM note WHERE document_id=?1)")?;
            step("DELETE FROM note WHERE document_id=?1")?;
            step("DELETE FROM tag WHERE document_id=?1")?;
            step("DELETE FROM clarification_item WHERE document_id=?1")?;
            step("DELETE FROM bookmark WHERE document_id=?1")?;
            // 锚点链(在其所有引用方已清理后删除)。
            step("DELETE FROM anchor_block WHERE anchor_id IN                  (SELECT id FROM anchor WHERE document_id=?1)")?;
            step("DELETE FROM anchor_relocation WHERE anchor_id IN                  (SELECT id FROM anchor WHERE document_id=?1)")?;
            step("DELETE FROM anchor WHERE document_id=?1")?;
            // 本轮补全的派生数据级联:画像/卡片、翻译记忆、思考记录、
            // quiz.v2 作答、Agent 运行与步骤、运行指标、会话摘要、附件元数据。
            // 这些都不是用户资产(产物/笔记/标注单独处理),删文档必须清干净,
            // 否则会留下“已删文档仍能检索到”的残留。
            step("DELETE FROM insight_card WHERE document_id=?1")?;
            step("DELETE FROM document_profile WHERE document_id=?1")?;
            step("DELETE FROM agent_reasoning WHERE document_id=?1")?;
            step("DELETE FROM quiz_v2_attempt WHERE artifact_version_id IN                  (SELECT av.id FROM artifact_version av JOIN artifact a ON av.artifact_id=a.id                  WHERE a.document_id=?1)")?;
            step("DELETE FROM agent_step WHERE run_id IN                  (SELECT id FROM agent_run WHERE document_id=?1)")?;
            step("DELETE FROM agent_run WHERE document_id=?1")?;
            step("DELETE FROM run_metrics WHERE task_id IN                  (SELECT id FROM task WHERE document_id=?1)")?;
            step("DELETE FROM conversation_summary WHERE conversation_id IN                  (SELECT id FROM conversation WHERE document_id=?1)")?;
            // 附件:元数据按文档删除;字节文件由附件服务在事务外清理。
            step("DELETE FROM attachment WHERE document_id=?1")?;
            // 解析与索引链。
            let gens = "SELECT id FROM parse_generation WHERE document_version_id IN                  (SELECT id FROM document_version WHERE document_id=?1)";
            let vers = "SELECT id FROM document_version WHERE document_id=?1";
            step(&format!(
                "DELETE FROM page_ocr WHERE document_version_id IN ({vers})"
            ))?;
            step(&format!(
                "DELETE FROM page_ocr_control WHERE document_version_id IN ({vers})"
            ))?;
            step(&format!("DELETE FROM unsupported_region WHERE generation_id IN ({gens})"))?;
            step(&format!("DELETE FROM parse_region WHERE generation_id IN ({gens})"))?;
            step(&format!("DELETE FROM vector_embedding WHERE generation_id IN ({gens})"))?;
            step(&format!(
                "DELETE FROM embedding_generation WHERE parse_generation_id IN ({gens})"
            ))?;
            step(&format!(
                "DELETE FROM content_block_fts WHERE block_id IN                  (SELECT id FROM content_block WHERE document_version_id IN ({vers}))"
            ))?;
            step(&format!(
                "DELETE FROM content_block WHERE document_version_id IN ({vers})"
            ))?;
            step(&format!("DELETE FROM parse_generation WHERE document_version_id IN ({vers})"))?;
            // 任务链(其引用的 parse_generation 已清理)。
            step("UPDATE task SET current_attempt_id=NULL WHERE document_id=?1")?;
            step("DELETE FROM data_clear_audit WHERE document_id=?1")?;
            step("DELETE FROM task_result WHERE task_id IN                  (SELECT id FROM task WHERE document_id=?1)")?;
            step("DELETE FROM task_attempt WHERE task_id IN                  (SELECT id FROM task WHERE document_id=?1)")?;
            step("DELETE FROM task WHERE document_id=?1")?;
            // 文档本体链。
            step("DELETE FROM privacy_grant WHERE document_id=?1")?;
            step("DELETE FROM reading_position WHERE document_id=?1")?;
            step("DELETE FROM file_location WHERE document_id=?1")?;
            step("DELETE FROM document_version WHERE document_id=?1")?;
            let removed = tx
                .execute("DELETE FROM document WHERE id=?1", rusqlite::params![id])
                .map_err(|e| db_app_error(&e))?;
            if removed == 0 {
                return Err(crate::shared::AppError::new(
                    crate::shared::CoreErrorCode::NotFound,
                    crate::shared::new_trace_id(),
                ));
            }
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }
    pub fn get_suggested_prompts(
        &self,
        document_id: &str,
    ) -> Result<DocumentSuggestedPromptsResponse, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            let doc_name: String = conn
                .query_row(
                    "SELECT display_name FROM document WHERE id=?1",
                    [&did],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| "当前文档".to_string());

            let mut stmt_h = conn
                .prepare(
                    "SELECT b.text FROM content_block b                      JOIN parse_generation g ON b.parse_generation_id = g.id                      JOIN document_version v ON g.document_version_id = v.id                      WHERE v.document_id = ?1 AND b.origin IN ('parser','ocr') AND b.kind = 'heading'                      ORDER BY b.ordinal ASC LIMIT 10",
                )
                .map_err(|e| db_app_error(&e))?;

            let headings: Vec<String> = stmt_h
                .query_map([&did], |r| r.get::<_, String>(0))
                .map_err(|e| db_app_error(&e))?
                .filter_map(|r| r.ok())
                .filter(|h| !h.trim().is_empty())
                .collect();

            let mut stmt_p = conn
                .prepare(
                    "SELECT b.text FROM content_block b                      JOIN parse_generation g ON b.parse_generation_id = g.id                      JOIN document_version v ON g.document_version_id = v.id                      WHERE v.document_id = ?1 AND b.origin IN ('parser','ocr') AND b.kind = 'paragraph' AND length(b.text) > 20                      ORDER BY b.ordinal ASC LIMIT 15",
                )
                .map_err(|e| db_app_error(&e))?;

            let paragraphs: Vec<String> = stmt_p
                .query_map([&did], |r| r.get::<_, String>(0))
                .map_err(|e| db_app_error(&e))?
                .filter_map(|r| r.ok())
                .collect();

            let (domain_label, items) = generate_dynamic_prompts_from_content(&doc_name, &headings, &paragraphs);
            Ok(DocumentSuggestedPromptsResponse {
                domain_label,
                items,
            })
        })
    }
}

pub fn generate_dynamic_prompts_from_content(
    doc_name: &str,
    headings: &[String],
    paragraphs: &[String],
) -> (Option<String>, Vec<SuggestedPromptItem>) {
    let clean_title = doc_name
        .trim_end_matches(".pdf")
        .trim_end_matches(".docx")
        .trim_end_matches(".md")
        .trim();

    let mut items = Vec::new();

    // 1. 优先基于文档本身的真实各章节标题进行提问生成
    if headings.len() >= 3 {
        let h0 = sanitize_heading(&headings[0]);
        let h1 = sanitize_heading(&headings[1]);
        let h2 = sanitize_heading(&headings[2]);

        items.push(SuggestedPromptItem {
            id: "1".into(),
            icon: "💡".into(),
            prompt: format!("关于【{h0}】的核心观点与论证依据是什么？"),
        });
        items.push(SuggestedPromptItem {
            id: "2".into(),
            icon: "🔍".into(),
            prompt: format!("【{h1}】涉及的关键机制或具体方法有哪些？"),
        });
        items.push(SuggestedPromptItem {
            id: "3".into(),
            icon: "❓".into(),
            prompt: format!("【{h2}】提出了哪些重要结论或实施建议？"),
        });
        items.push(SuggestedPromptItem {
            id: "4".into(),
            icon: "✨".into(),
            prompt: format!("全面总结《{clean_title}》的论证脉络与核心结论"),
        });

        let domain = if !h0.is_empty() {
            Some(truncate_chars(&h0, 10))
        } else {
            None
        };
        return (domain, items);
    }

    if headings.len() == 2 {
        let h0 = sanitize_heading(&headings[0]);
        let h1 = sanitize_heading(&headings[1]);
        items.push(SuggestedPromptItem {
            id: "1".into(),
            icon: "💡".into(),
            prompt: format!("深入解析【{h0}】的核心背景与关键论述"),
        });
        items.push(SuggestedPromptItem {
            id: "2".into(),
            icon: "🔍".into(),
            prompt: format!("【{h1}】中提出的重点方案或理论推导"),
        });
        if let Some(p) = paragraphs.first() {
            let sentence = first_sentence(p, 25);
            items.push(SuggestedPromptItem {
                id: "3".into(),
                icon: "❓".into(),
                prompt: format!("围绕「{sentence}」文中有哪些详细说明？"),
            });
        } else {
            items.push(SuggestedPromptItem {
                id: "3".into(),
                icon: "❓".into(),
                prompt: format!("结合《{clean_title}》出 3 道典型思考题与解析"),
            });
        }
        items.push(SuggestedPromptItem {
            id: "4".into(),
            icon: "✨".into(),
            prompt: format!("总结《{clean_title}》的核心要点与复习建议"),
        });
        return (Some(truncate_chars(&h0, 10)), items);
    }

    if headings.len() == 1 {
        let h0 = sanitize_heading(&headings[0]);
        items.push(SuggestedPromptItem {
            id: "1".into(),
            icon: "💡".into(),
            prompt: format!("深入解析【{h0}】的关键论点与推导逻辑"),
        });
        if paragraphs.len() >= 2 {
            let s0 = first_sentence(&paragraphs[0], 25);
            let s1 = first_sentence(&paragraphs[1], 25);
            items.push(SuggestedPromptItem {
                id: "2".into(),
                icon: "🔍".into(),
                prompt: format!("针对「{s0}」文档给出了哪些佐证？"),
            });
            items.push(SuggestedPromptItem {
                id: "3".into(),
                icon: "❓".into(),
                prompt: format!("如何理解文中所述「{s1}」的核心含义？"),
            });
        } else {
            items.push(SuggestedPromptItem {
                id: "2".into(),
                icon: "🔍".into(),
                prompt: format!("梳理《{clean_title}》中的关键定义与重要论据"),
            });
            items.push(SuggestedPromptItem {
                id: "3".into(),
                icon: "❓".into(),
                prompt: format!("结合《{clean_title}》出 3 道核心考题并给出解析"),
            });
        }
        items.push(SuggestedPromptItem {
            id: "4".into(),
            icon: "✨".into(),
            prompt: format!("全面提炼《{clean_title}》的主旨框架与主要发现"),
        });
        return (Some(truncate_chars(&h0, 10)), items);
    }

    // 无显式标题:从正文实际段落提取主题句
    if paragraphs.len() >= 3 {
        let s0 = first_sentence(&paragraphs[0], 26);
        let s1 = first_sentence(&paragraphs[1], 26);
        let s2 = first_sentence(&paragraphs[2], 26);
        items.push(SuggestedPromptItem {
            id: "1".into(),
            icon: "💡".into(),
            prompt: format!("文中开篇提出的「{s0}」具体指的是什么？"),
        });
        items.push(SuggestedPromptItem {
            id: "2".into(),
            icon: "🔍".into(),
            prompt: format!("针对「{s1}」文中有哪些具体论据与推导？"),
        });
        items.push(SuggestedPromptItem {
            id: "3".into(),
            icon: "❓".into(),
            prompt: format!("如何看待文中所提到的「{s2}」？"),
        });
        items.push(SuggestedPromptItem {
            id: "4".into(),
            icon: "✨".into(),
            prompt: format!("全面总结《{clean_title}》的核心要点与关键结论"),
        });
        return (None, items);
    }

    // 兜底提取
    items.push(SuggestedPromptItem {
        id: "1".into(),
        icon: "💡".into(),
        prompt: format!("梳理《{clean_title}》的核心内容与主要机制"),
    });
    items.push(SuggestedPromptItem {
        id: "2".into(),
        icon: "🔍".into(),
        prompt: format!("详细阐述《{clean_title}》中的关键概念与难点"),
    });
    items.push(SuggestedPromptItem {
        id: "3".into(),
        icon: "❓".into(),
        prompt: format!("结合《{clean_title}》出 3 道典型考题与解析"),
    });
    items.push(SuggestedPromptItem {
        id: "4".into(),
        icon: "✨".into(),
        prompt: format!("总结《{clean_title}》得出的主要结论与启示"),
    });
    (None, items)
}

fn sanitize_heading(raw: &str) -> String {
    let trimmed = raw.trim();
    let re_cleaned = trimmed.trim_start_matches(|c: char| {
        c.is_ascii_digit() || c == '.' || c == '、' || c == ' ' || c == '\t' || c == '-' || c == '#'
    });
    let title = if re_cleaned.is_empty() {
        trimmed
    } else {
        re_cleaned
    };
    truncate_chars(title, 26)
}

fn first_sentence(paragraph: &str, max_len: usize) -> String {
    let s = paragraph
        .split(&['。', '！', '？', '；', '\n', '.', '!', '?'][..])
        .next()
        .unwrap_or(paragraph)
        .trim();
    truncate_chars(s, max_len)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[..max_chars].iter().collect()
    }
}
