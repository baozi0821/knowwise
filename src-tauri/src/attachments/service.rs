//! 图片附件服务(《WBS-1》):落盘、读取、配额、sha256 校验与孤儿扫描。
//!
//! 安全边界:
//! - 只接受 JPEG/PNG/WebP 且校验文件魔数;单张 ≤1.5MB;绑定单条消息 ≤3 张、合计 ≤5MB;
//! - 数据库只存相对 `storage_key` 与摘要,不存 base64 与绝对路径;
//! - 读取时重新计算 sha256,与库中摘要不一致即拒绝;
//! - 错误只返回稳定 reason,不携带路径、图片内容或文件系统原文。

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rusqlite::{params, params_from_iter, OptionalExtension};

use crate::persistence::{db_app_error, Db};
use crate::retrieval_ai::models::GenerationAttachment;
use crate::shared::textnorm::sha256_hex;
use crate::shared::{AppError, Clock, CoreErrorCode, IdGenerator};

use super::models::*;

fn invalid(reason: &'static str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
        .with_detail("reason", reason.into())
}

fn internal(reason: &'static str) -> AppError {
    AppError::new(CoreErrorCode::Internal, crate::shared::new_trace_id())
        .with_detail("reason", reason.into())
}

fn not_found() -> AppError {
    AppError::new(CoreErrorCode::NotFound, crate::shared::new_trace_id())
}

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttachmentRow> {
    Ok(AttachmentRow {
        id: row.get("id")?,
        document_id: row.get("document_id")?,
        conversation_id: row.get("conversation_id")?,
        message_id: row.get("message_id")?,
        mime: row.get("mime")?,
        width: row.get("width")?,
        height: row.get("height")?,
        sha256: row.get("sha256")?,
        byte_size: row.get("byte_size")?,
        storage_key: row.get("storage_key")?,
        name: row.get("name")?,
        created_at: row.get("created_at")?,
    })
}

/// 从文件头推断图片 MIME;不信任扩展名/客户端声明。
pub fn infer_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some("image/png");
    }
    if bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF] {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

fn validate_dimensions(width: Option<i64>, height: Option<i64>) -> Result<(), AppError> {
    for value in [width, height].into_iter().flatten() {
        if value <= 0 || value > MAX_IMAGE_EDGE {
            return Err(invalid("attachment_dimensions_invalid"));
        }
    }
    Ok(())
}

fn validate_mime_and_bytes(mime: &str, bytes: &[u8]) -> Result<&'static str, AppError> {
    if !ALLOWED_MIMES.contains(&mime) {
        return Err(invalid("attachment_mime_not_allowed"));
    }
    let inferred = infer_image_mime(bytes).ok_or_else(|| invalid("attachment_not_image"))?;
    if inferred != mime {
        return Err(invalid("attachment_mime_mismatch"));
    }
    Ok(inferred)
}

fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", BASE64.encode(bytes))
}

/// 读取用户经系统对话框选择的本地图片(仅图片;不写库)。
/// 调用方负责确认路径来自对话框;错误不包含路径。
pub fn read_local_image(path: &str) -> Result<LocalImageData, AppError> {
    let path = Path::new(path);
    let metadata = std::fs::metadata(path).map_err(|_| invalid("attachment_read_failed"))?;
    if !metadata.is_file() || metadata.len() > MAX_LOCAL_IMAGE_BYTES {
        return Err(invalid("attachment_file_too_large"));
    }
    let bytes = std::fs::read(path).map_err(|_| invalid("attachment_read_failed"))?;
    let mime = infer_image_mime(&bytes).ok_or_else(|| invalid("attachment_not_image"))?;
    Ok(LocalImageData {
        mime: mime.to_string(),
        data_url: data_url(mime, &bytes),
    })
}

/// 上传输入(字节只在内存;不进入日志)。
pub struct UploadAttachment<'a> {
    pub document_id: &'a str,
    pub mime: &'a str,
    pub bytes: &'a [u8],
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub name: Option<&'a str>,
}

pub struct AttachmentService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    root: PathBuf,
}

impl AttachmentService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        root: PathBuf,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            root,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn ensure_root(&self) -> Result<(), AppError> {
        std::fs::create_dir_all(&self.root).map_err(|_| internal("attachment_root_unavailable"))
    }

    /// storage_key 必须是我们生成的相对路径;任何绝对/父级路径直接拒绝。
    fn storage_path(&self, key: &str) -> Result<PathBuf, AppError> {
        let path = Path::new(key);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(internal("attachment_storage_key_invalid"));
        }
        Ok(self.root.join(path))
    }

    fn document_exists(&self, document_id: &str) -> Result<bool, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            conn.query_row("SELECT 1 FROM document WHERE id=?1", [&did], |_| Ok(()))
                .optional()
                .map(|row| row.is_some())
                .map_err(|e| db_app_error(&e))
        })
    }

    /// 上传:校验 → 落盘 → 写元数据(未绑定消息)。
    pub fn upload(&self, input: UploadAttachment<'_>) -> Result<AttachmentView, AppError> {
        if input.bytes.is_empty() || input.bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(invalid("attachment_too_large"));
        }
        if !self.document_exists(input.document_id)? {
            return Err(not_found());
        }
        validate_dimensions(input.width, input.height)?;
        let mime = validate_mime_and_bytes(input.mime, input.bytes)?;
        self.ensure_root()?;

        let id = self.ids.new_entity_id();
        let sha256 = sha256_hex(input.bytes);
        let shard = sha256.get(..2).unwrap_or("00");
        let storage_key = format!("{shard}/{id}.{}", extension_for(mime));
        let path = self.storage_path(&storage_key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| internal("attachment_root_unavailable"))?;
        }
        let temp = path.with_extension(format!(
            "{}.part",
            path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
        ));
        std::fs::write(&temp, input.bytes).map_err(|_| internal("attachment_write_failed"))?;
        if std::fs::rename(&temp, &path).is_err() {
            let _ = std::fs::remove_file(&temp);
            return Err(internal("attachment_write_failed"));
        }

        let now = self.clock.now_ms();
        let name = input.name.map(|n| {
            n.rsplit(['/', '\\'])
                .next()
                .unwrap_or(n)
                .trim()
                .chars()
                .take(120)
                .collect::<String>()
        });
        let name = name.filter(|n| !n.is_empty());
        let insert = (
            id.clone(),
            input.document_id.to_string(),
            mime.to_string(),
            input.width,
            input.height,
            sha256.clone(),
            input.bytes.len() as i64,
            storage_key,
            name,
            now,
        );
        let result = self.db.write(move |conn| {
            let (id, document_id, mime, width, height, sha256, byte_size, storage_key, name, now) =
                insert;
            conn.execute(
                "INSERT INTO attachment(id, document_id, conversation_id, message_id, mime, \
                 width, height, sha256, byte_size, storage_key, name, created_at) \
                 VALUES (?1,?2,NULL,NULL,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    id,
                    document_id,
                    mime,
                    width,
                    height,
                    sha256,
                    byte_size,
                    storage_key,
                    name,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        });
        if let Err(e) = result {
            // 数据库失败时不留下无主文件。
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        self.get_view(&id)
    }

    fn get_view(&self, id: &str) -> Result<AttachmentView, AppError> {
        let row = self.get_row(id)?;
        Ok(row.to_view())
    }

    /// 图片记忆:复制一张历史附件的字节为新的未绑定附件。
    ///
    /// 不改变原消息的附件绑定;用户显式选择后才调用,默认从不自动重发。
    pub fn clone_unbound(&self, id: &str) -> Result<AttachmentView, AppError> {
        let source = self.get_row(id)?;
        if source.byte_size > MAX_ATTACHMENT_BYTES as i64 {
            return Err(invalid("attachment_too_large"));
        }
        let bytes = self.verified_bytes(&source)?;
        let mime = validate_mime_and_bytes(&source.mime, &bytes)?;
        self.ensure_root()?;
        let new_id = self.ids.new_entity_id();
        let shard = source.sha256.get(..2).unwrap_or("00");
        let storage_key = format!("{shard}/{new_id}.{}", extension_for(mime));
        let path = self.storage_path(&storage_key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| internal("attachment_root_unavailable"))?;
        }
        let temp = path.with_extension(format!(
            "{}.part",
            path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
        ));
        std::fs::write(&temp, &bytes).map_err(|_| internal("attachment_write_failed"))?;
        if std::fs::rename(&temp, &path).is_err() {
            let _ = std::fs::remove_file(&temp);
            return Err(internal("attachment_write_failed"));
        }
        let now = self.clock.now_ms();
        let insert = (
            new_id.clone(),
            source.document_id.clone(),
            source.conversation_id.clone(),
            mime.to_string(),
            source.width,
            source.height,
            source.sha256.clone(),
            bytes.len() as i64,
            storage_key.clone(),
            source.name.clone(),
            now,
        );
        let result = self.db.write(move |conn| {
            let (
                id,
                document_id,
                conversation_id,
                mime,
                width,
                height,
                sha256,
                byte_size,
                storage_key,
                name,
                now,
            ) = insert;
            conn.execute(
                "INSERT INTO attachment(id, document_id, conversation_id, message_id, mime, \
                 width, height, sha256, byte_size, storage_key, name, created_at) \
                 VALUES (?1,?2,?3,NULL,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    id,
                    document_id,
                    conversation_id,
                    mime,
                    width,
                    height,
                    sha256,
                    byte_size,
                    storage_key,
                    name,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        });
        if let Err(e) = result {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        self.get_view(&new_id)
    }

    fn get_row(&self, id: &str) -> Result<AttachmentRow, AppError> {
        let id = id.to_string();
        self.db
            .read(move |conn| {
                conn.query_row(
                    &format!("SELECT {ATTACHMENT_COLS} FROM attachment WHERE id=?1"),
                    [&id],
                    row_from,
                )
                .optional()
                .map_err(|e| db_app_error(&e))
            })?
            .ok_or_else(not_found)
    }

    fn rows_for_ids(&self, ids: &[String]) -> Result<Vec<AttachmentRow>, AppError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique = HashSet::new();
        for id in ids {
            if !unique.insert(id) {
                return Err(invalid("attachment_duplicate"));
            }
        }
        if ids.len() > MAX_ATTACHMENTS_PER_MESSAGE {
            return Err(invalid("attachment_count_exceeded"));
        }
        let expected = ids.len();
        let order = ids.to_vec();
        let query_ids = ids.to_vec();
        let mut rows: Vec<AttachmentRow> = self.db.read(move |conn| {
            let placeholders = std::iter::repeat_n("?", query_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql =
                format!("SELECT {ATTACHMENT_COLS} FROM attachment WHERE id IN ({placeholders})");
            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map(params_from_iter(query_ids.iter()), row_from)
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(rows)
        })?;
        if rows.len() != expected {
            return Err(not_found());
        }
        // 保持调用方顺序,便于用户消息引用顺序稳定。
        rows.sort_by_key(|row| {
            order
                .iter()
                .position(|id| id == &row.id)
                .unwrap_or(usize::MAX)
        });
        Ok(rows)
    }

    /// 发送前预检(用户消息写入之前):只读,不改变绑定状态。
    /// 确保失败发生在任何消息/任务落库之前。
    pub fn validate_for_ask(
        &self,
        document_id: &str,
        conversation_id: &str,
        ids: &[String],
    ) -> Result<Vec<AttachmentView>, AppError> {
        let rows = self.rows_for_ids(ids)?;
        let mut total = 0i64;
        for row in &rows {
            if row.document_id != document_id {
                return Err(invalid("attachment_document_mismatch"));
            }
            if row
                .conversation_id
                .as_deref()
                .is_some_and(|cid| cid != conversation_id)
            {
                return Err(invalid("attachment_conversation_mismatch"));
            }
            if row.byte_size > MAX_ATTACHMENT_BYTES as i64 {
                return Err(invalid("attachment_too_large"));
            }
            total += row.byte_size;
        }
        if total > MAX_TOTAL_ATTACHMENT_BYTES as i64 {
            return Err(invalid("attachment_total_too_large"));
        }
        Ok(rows.iter().map(AttachmentRow::to_view).collect())
    }

    /// 发送前绑定:附件必须属于当前文档/会话;不得抢占已绑定到其他消息的附件。
    /// 幂等:重放同一消息的绑定成功。
    pub fn bind_to_message(
        &self,
        document_id: &str,
        conversation_id: &str,
        message_id: &str,
        ids: &[String],
    ) -> Result<Vec<AttachmentView>, AppError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.rows_for_ids(ids)?;
        let (msg_doc, msg_conv): (String, String) = {
            let mid = message_id.to_string();
            self.db
                .read(move |conn| {
                    conn.query_row(
                        "SELECT c.document_id, m.conversation_id FROM message m \
                         JOIN conversation c ON c.id=m.conversation_id WHERE m.id=?1",
                        [&mid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()
                    .map_err(|e| db_app_error(&e))
                })?
                .ok_or_else(|| {
                    AppError::new(CoreErrorCode::NotFound, crate::shared::new_trace_id())
                })?
        };
        if msg_doc != document_id || msg_conv != conversation_id {
            return Err(invalid("attachment_message_mismatch"));
        }
        let mut total = 0i64;
        for row in &rows {
            if row.document_id != document_id {
                return Err(invalid("attachment_document_mismatch"));
            }
            if row
                .conversation_id
                .as_deref()
                .is_some_and(|cid| cid != conversation_id)
            {
                return Err(invalid("attachment_conversation_mismatch"));
            }
            if row
                .message_id
                .as_deref()
                .is_some_and(|mid| mid != message_id)
            {
                return Err(invalid("attachment_already_bound"));
            }
            if row.byte_size > MAX_ATTACHMENT_BYTES as i64 {
                return Err(invalid("attachment_too_large"));
            }
            total += row.byte_size;
        }
        if total > MAX_TOTAL_ATTACHMENT_BYTES as i64 {
            return Err(invalid("attachment_total_too_large"));
        }

        let ids = ids.to_vec();
        let view_ids = ids.clone();
        let doc = document_id.to_string();
        let conv = conversation_id.to_string();
        let mid = message_id.to_string();
        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            for id in &ids {
                let changed = tx
                    .execute(
                        "UPDATE attachment SET message_id=?1, conversation_id=?2 \
                         WHERE id=?3 AND document_id=?4 \
                         AND (message_id IS NULL OR message_id=?1) \
                         AND (conversation_id IS NULL OR conversation_id=?2)",
                        params![mid, conv, id, doc],
                    )
                    .map_err(|e| db_app_error(&e))?;
                if changed != 1 {
                    return Err(invalid("attachment_bind_conflict"));
                }
            }
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(())
        })?;
        // 回读绑定后的行;不能用更新前的快照。
        view_ids.iter().map(|id| self.get_view(id)).collect()
    }

    fn verified_bytes(&self, row: &AttachmentRow) -> Result<Vec<u8>, AppError> {
        let path = self.storage_path(&row.storage_key)?;
        let bytes = std::fs::read(&path).map_err(|_| internal("attachment_read_failed"))?;
        if bytes.len() as i64 != row.byte_size || sha256_hex(&bytes) != row.sha256 {
            return Err(internal("attachment_integrity_failed"));
        }
        Ok(bytes)
    }

    /// 生成前读取附件字节(必须已绑定消息);任何摘要不一致都拒绝。
    pub fn load_for_generation(
        &self,
        document_id: &str,
        ids: &[String],
    ) -> Result<Vec<GenerationAttachment>, AppError> {
        let rows = self.rows_for_ids(ids)?;
        let mut out = Vec::with_capacity(rows.len());
        let mut total = 0i64;
        for row in &rows {
            if row.document_id != document_id {
                return Err(invalid("attachment_document_mismatch"));
            }
            if row.message_id.is_none() {
                return Err(invalid("attachment_not_bound"));
            }
            if row.byte_size > MAX_ATTACHMENT_BYTES as i64 {
                return Err(invalid("attachment_too_large"));
            }
            total += row.byte_size;
            if total > MAX_TOTAL_ATTACHMENT_BYTES as i64 {
                return Err(invalid("attachment_total_too_large"));
            }
            let bytes = self.verified_bytes(row)?;
            out.push(GenerationAttachment {
                attachment_id: row.id.clone(),
                mime: row.mime.clone(),
                sha256: row.sha256.clone(),
                bytes,
            });
        }
        Ok(out)
    }

    /// 展示用缩略图/原图 data URL(只经 IPC;不写库)。
    pub fn thumbnail(&self, id: &str) -> Result<AttachmentThumbnail, AppError> {
        let row = self.get_row(id)?;
        let bytes = self.verified_bytes(&row)?;
        Ok(AttachmentThumbnail {
            id: row.id,
            mime: row.mime.clone(),
            data_url: data_url(&row.mime, &bytes),
        })
    }

    /// 删除尚未发送的附件;已绑定消息的附件只能随消息/会话/文档级联删除。
    pub fn delete_unbound(&self, id: &str) -> Result<bool, AppError> {
        let row = self.get_row(id)?;
        if row.message_id.is_some() {
            return Err(invalid("attachment_already_bound"));
        }
        let id_owned = id.to_string();
        let deleted = self.db.write(move |conn| {
            let changed = conn
                .execute(
                    "DELETE FROM attachment WHERE id=?1 AND message_id IS NULL",
                    [&id_owned],
                )
                .map_err(|e| db_app_error(&e))?;
            Ok(changed == 1)
        })?;
        if deleted {
            if let Ok(path) = self.storage_path(&row.storage_key) {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(deleted)
    }

    pub fn storage_keys_for_document(&self, document_id: &str) -> Result<Vec<String>, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare("SELECT storage_key FROM attachment WHERE document_id=?1")
                .map_err(|e| db_app_error(&e))?;
            let keys = stmt
                .query_map([&did], |r| r.get::<_, String>(0))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(keys)
        })
    }

    /// 会话导出的附件元数据(按绑定顺序);不含字节与 storage_key。
    pub fn list_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<AttachmentView>, AppError> {
        let cid = conversation_id.to_string();
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {ATTACHMENT_COLS} FROM attachment WHERE conversation_id=?1 \
                     ORDER BY created_at, id"
                ))
                .map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map([&cid], row_from)
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(rows.iter().map(AttachmentRow::to_view).collect())
        })
    }

    pub fn storage_keys_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<String>, AppError> {
        let cid = conversation_id.to_string();
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare("SELECT storage_key FROM attachment WHERE conversation_id=?1")
                .map_err(|e| db_app_error(&e))?;
            let keys = stmt
                .query_map([&cid], |r| r.get::<_, String>(0))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(keys)
        })
    }

    /// 删除已被数据库删除的附件文件;缺失文件视为已清理。
    pub fn remove_files(&self, keys: &[String]) -> Result<usize, AppError> {
        let mut removed = 0;
        for key in keys {
            let path = self.storage_path(key)?;
            match std::fs::remove_file(path) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(internal("attachment_file_delete_failed")),
            }
        }
        Ok(removed)
    }

    /// 孤儿扫描:
    /// 1) 超过 TTL 的未绑定附件 → 删记录 + 文件;
    /// 2) 磁盘上未被任何记录引用的文件 → 删除。
    ///
    /// 只在启动时调用,不和上传并发。
    pub fn sweep_orphans(&self, cutoff_ms: i64) -> Result<usize, AppError> {
        self.ensure_root()?;
        let stale: Vec<(String, String)> = self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, storage_key FROM attachment \
                     WHERE message_id IS NULL AND created_at < ?1",
                )
                .map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map([cutoff_ms], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(rows)
        })?;
        let mut removed = 0;
        for (id, key) in stale {
            let deleted = self.db.write(move |conn| {
                let changed = conn
                    .execute(
                        "DELETE FROM attachment WHERE id=?1 AND message_id IS NULL",
                        [&id],
                    )
                    .map_err(|e| db_app_error(&e))?;
                Ok(changed == 1)
            })?;
            if deleted {
                if let Ok(path) = self.storage_path(&key) {
                    match std::fs::remove_file(path) {
                        Ok(()) => removed += 1,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => {}
                    }
                }
            }
        }

        let referenced: HashSet<String> = self.db.read(|conn| {
            let mut stmt = conn
                .prepare("SELECT storage_key FROM attachment")
                .map_err(|e| db_app_error(&e))?;
            let keys = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<HashSet<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            Ok(keys)
        })?;
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let path = entry.path();
                let Ok(relative) = path.strip_prefix(&self.root) else {
                    continue;
                };
                let Some(key) = relative.to_str() else {
                    continue;
                };
                let normalized = key.replace('\\', "/");
                if referenced.contains(&normalized) {
                    continue;
                }
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_magic_is_authoritative() {
        assert_eq!(
            infer_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(
            infer_image_mime(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Some("image/png")
        );
        assert_eq!(
            infer_image_mime(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(infer_image_mime(b"not an image"), None);
        assert!(validate_mime_and_bytes("image/png", b"not an image").is_err());
        assert!(validate_mime_and_bytes("image/jpeg", &[0xFF, 0xD8, 0xFF]).is_ok());
        assert!(validate_mime_and_bytes("image/gif", &[0xFF, 0xD8, 0xFF]).is_err());
    }
}
