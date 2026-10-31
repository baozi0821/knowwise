//! M03 阅读器服务:渲染计划、令牌门控范围读取与阅读位置(《M03》§3/§7)。
//!
//! 关键不变量:
//! - 阅读不等待语义索引与指纹完成:渲染计划对 computing 版本也签发
//!   临时只读令牌(《M02》§3 首屏不等哈希的延伸)。
//! - `reader_read_range` 只经 M02 访问令牌读原文件字节;原文件只读。
//! - 持久定位只允许 SourceLocator(user space/UTF-16 偏移/OOXML 顺序),
//!   禁止缩放比例、Canvas 像素或 DPR(《M03》§3)。
//! - 阅读位置按文档唯一;定位格式必须与文档格式匹配;保存即修订号递增。

use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};

use crate::persistence::{db_app_error, Db};
use crate::shared::{AppError, Clock, CoreErrorCode, EventSink, IdGenerator};

use super::models::{DocumentFormat, DocumentRow};
use super::reader_models::{ReadingPosition, RenderPlan};
use super::service::{DocumentService, TokenBinding};

const DOC_COLS: &str =
    "id, display_name, format, current_version_id, revision, created_at, updated_at, last_opened_at";

/// 渲染计划分块建议:约 1 MiB(《M03》§3 PDF range transport)。
pub const RENDER_CHUNK_BYTES: i64 = 1024 * 1024;
/// 单次范围读取上限(超过截断;与解析桥读取一致)。
pub const MAX_RANGE_BYTES: i64 = 1024 * 1024;

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

fn load_doc(conn: &Connection, id: &str) -> Result<Option<DocumentRow>, AppError> {
    let sql = format!("SELECT {DOC_COLS} FROM document WHERE id=?1");
    conn.query_row(&sql, [id], |r| DocumentRow::try_from(r))
        .optional()
        .map_err(|e| db_app_error(&e))
}

fn locator_kind(locator: &crate::shared::locator::SourceLocator) -> &'static str {
    use crate::shared::locator::SourceLocator;
    match locator {
        SourceLocator::Pdf { .. } => "pdf",
        SourceLocator::Markdown { .. } => "markdown",
        SourceLocator::Docx { .. } => "docx",
        SourceLocator::Composite { parts, .. } => {
            parts.first().map(locator_kind).unwrap_or("composite")
        }
    }
}

pub struct ReaderService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    _ids: Arc<dyn IdGenerator>,
    _events: Arc<dyn EventSink>,
    documents: Arc<DocumentService>,
}

impl ReaderService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        events: Arc<dyn EventSink>,
        documents: Arc<DocumentService>,
    ) -> Self {
        Self {
            db,
            clock,
            _ids: ids,
            _events: events,
            documents,
        }
    }

    /// `reader_get_render_plan`:访问信息与格式边界,不等待语义索引。
    pub fn get_render_plan(
        &self,
        document_id: &str,
        version_id: &str,
    ) -> Result<RenderPlan, AppError> {
        let did = document_id.to_string();
        let vid = version_id.to_string();
        let (doc, file_size) = self.db.read(move |conn| {
            let doc = load_doc(conn, &did)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
            let ver_owner: Option<String> = conn
                .query_row(
                    "SELECT document_id FROM document_version WHERE id=?1",
                    [&vid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if ver_owner.as_deref() != Some(did.as_str()) {
                return Err(err(CoreErrorCode::NotFound));
            }
            let size: Option<i64> = conn
                .query_row(
                    "SELECT file_size FROM document_version WHERE id=?1",
                    [&vid],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            Ok((doc, size.unwrap_or(0)))
        })?;

        // 签发短期只读令牌(指纹未完成也可临时打开,复用 M02 语义)。
        let open = self.documents.open_document(document_id)?;
        Ok(RenderPlan {
            document_id: doc.id,
            version_id: version_id.to_string(),
            format: doc.format,
            file_size,
            access_token: Some(crate::shared::dto::AccessTokenDescriptor {
                token: open.token,
                expires_at: open.expires_at,
            }),
            chunk_size: RENDER_CHUNK_BYTES,
            range_read_supported: doc.format == DocumentFormat::Pdf,
        })
    }

    /// `reader_read_range`:访问令牌门控的字节范围读取(《M03》§3)。
    /// 令牌绑定版本与大小;越界拒绝,超限截断;原文件只读。
    pub fn read_range(
        &self,
        access_token: &str,
        offset: i64,
        length: i64,
    ) -> Result<crate::shared::dto::BinaryChunk, AppError> {
        let binding: TokenBinding = self.documents.validate_token(access_token)?;
        if offset < 0 {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        if offset >= binding.size {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "offset_beyond_eof".into()));
        }
        let length = length.clamp(0, MAX_RANGE_BYTES).min(binding.size - offset);
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

    /// `reader_save_position`:防抖后的有效位置保存。
    /// 定位格式必须匹配;同文档重复保存递增修订号(幂等键由 UI 防抖承载)。
    pub fn save_position(
        &self,
        document_id: &str,
        version_id: &str,
        locator: &crate::shared::locator::SourceLocator,
    ) -> Result<ReadingPosition, AppError> {
        let did = document_id.to_string();
        // 校验文档、版本归属与定位格式。
        let format = self.db.read({
            let did = did.clone();
            let vid = version_id.to_string();
            move |conn| {
                let doc = load_doc(conn, &did)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
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
                Ok(doc.format)
            }
        })?;
        if locator_kind(locator) != format.as_str() {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "locator_format_mismatch".into()));
        }

        let now = self.clock.now_ms();
        let locator_json = serde_json::to_string(locator).unwrap_or_default();
        let locator_back = locator.clone();
        let heading_path = match locator {
            crate::shared::locator::SourceLocator::Markdown { heading_path, .. }
            | crate::shared::locator::SourceLocator::Docx { heading_path, .. } => {
                serde_json::to_string(heading_path).ok()
            }
            _ => None,
        };
        let vid = version_id.to_string();
        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT revision FROM reading_position WHERE document_id=?1",
                    [&did],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let revision = match existing {
                Some(rev) => {
                    tx.execute(
                        "UPDATE reading_position SET document_version_id=?2, locator_json=?3, \
                         heading_path_json=?4, revision=revision+1, saved_at=?5 \
                         WHERE document_id=?1",
                        params![did, vid, locator_json, heading_path, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    rev + 1
                }
                None => {
                    tx.execute(
                        "INSERT INTO reading_position(document_id, document_version_id, \
                         locator_json, heading_path_json, revision, saved_at) \
                         VALUES (?1,?2,?3,?4,1,?5)",
                        params![did, vid, locator_json, heading_path, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                    1
                }
            };
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(ReadingPosition {
                document_id: did,
                version_id: vid,
                locator: locator_back,
                revision,
                saved_at: now,
            })
        })
    }

    /// `reader_get_position`:恢复时读取(《M03》§7)。
    pub fn get_position(&self, document_id: &str) -> Result<Option<ReadingPosition>, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            let row: Option<(String, String, i64, i64)> = conn
                .query_row(
                    "SELECT document_version_id, locator_json, revision, saved_at \
                     FROM reading_position WHERE document_id=?1",
                    [&did],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            Ok(row.and_then(|(vid, loc, rev, saved)| {
                Some(ReadingPosition {
                    document_id: did,
                    version_id: vid,
                    locator: serde_json::from_str(&loc).ok()?,
                    revision: rev,
                    saved_at: saved,
                })
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locator_kind_matrix() {
        use crate::shared::locator::{PageRotation, SourceLocator};
        assert_eq!(
            locator_kind(&SourceLocator::pdf(1, vec![], PageRotation::R0)),
            "pdf"
        );
        assert_eq!(
            locator_kind(&SourceLocator::markdown(0, 1, "paragraph", vec![])),
            "markdown"
        );
        assert_eq!(
            locator_kind(&SourceLocator::docx(0, 1, vec![], vec![])),
            "docx"
        );
        let comp = SourceLocator::Composite {
            parts: vec![SourceLocator::pdf(1, vec![], PageRotation::R0)],
            locator_version: 1,
        };
        assert_eq!(locator_kind(&comp), "pdf");
    }
}
