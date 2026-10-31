//! 格式检测(M02,见《M02》§2)。
//! 依据文件签名、ZIP 必要条目与扩展名的组合;不信任用户改名。
//! 原文件只读:检测只读字节,不写入、不修改 mtime。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::shared::{AppError, CoreErrorCode};

use super::models::DocumentFormat;

pub const PDF_MAGIC: &[u8] = b"%PDF-";
pub const CFB_MAGIC: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
pub const ZIP_MAGIC: &[u8] = &[0x50, 0x4B, 0x03, 0x04];

#[derive(Debug, Clone, Copy)]
pub struct SizeLimits {
    pub pdf_mib: u64,
    pub docx_mib: u64,
    pub markdown_mib: u64,
}

impl Default for SizeLimits {
    fn default() -> Self {
        Self {
            pdf_mib: 500,
            docx_mib: 100,
            markdown_mib: 50,
        }
    }
}

impl SizeLimits {
    pub fn for_format(&self, format: DocumentFormat) -> u64 {
        match format {
            DocumentFormat::Pdf => self.pdf_mib,
            DocumentFormat::Docx => self.docx_mib,
            DocumentFormat::Markdown => self.markdown_mib,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DetectedFile {
    pub format: DocumentFormat,
    pub size: u64,
    pub mtime_ns: i64,
}

fn ext(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn doc_err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

/// 检测文件格式与元数据。所有失败路径返回稳定错误码(M02-A1)。
pub fn detect_file(path: &Path, limits: &SizeLimits) -> Result<DetectedFile, AppError> {
    let ext = ext(path);
    let expected = match ext.as_str() {
        "pdf" => Some(DocumentFormat::Pdf),
        "md" | "markdown" => Some(DocumentFormat::Markdown),
        "docx" => Some(DocumentFormat::Docx),
        "doc" => {
            // 旧 .doc:提示转换,不在后台改写原文件。
            return Err(doc_err(CoreErrorCode::DocUnsupportedFormat)
                .with_detail("reason", "legacy_doc_convert_hint".into()));
        }
        _ => None,
    };

    let meta = std::fs::metadata(path).map_err(|_| doc_err(CoreErrorCode::DocFileMissing))?;
    if !meta.is_file() {
        return Err(doc_err(CoreErrorCode::DocUnsupportedFormat));
    }
    let size = meta.len();
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    if size == 0 {
        return Err(doc_err(CoreErrorCode::DocEmpty));
    }

    let mut head = [0u8; 8];
    {
        let mut f =
            std::fs::File::open(path).map_err(|_| doc_err(CoreErrorCode::DocFileMissing))?;
        let n = read_up_to(&mut f, &mut head).map_err(|_| doc_err(CoreErrorCode::DocCorrupt))?;
        if n == 0 {
            return Err(doc_err(CoreErrorCode::DocEmpty));
        }
    }

    // CFB 容器:加密 OOXML 或旧版二进制格式。
    if head.starts_with(CFB_MAGIC) {
        return Err(match ext.as_str() {
            "docx" => doc_err(CoreErrorCode::DocEncrypted),
            _ => doc_err(CoreErrorCode::DocUnsupportedFormat),
        });
    }

    let detected = if head.starts_with(PDF_MAGIC) {
        Some(DocumentFormat::Pdf)
    } else if head.starts_with(ZIP_MAGIC) {
        // DOCX 必须是 ZIP 且包含必要条目(《M02》§2)。
        if ext != "docx" {
            // ZIP 但扩展名不符:不冒充格式。
            None
        } else if has_docx_entries(path)? {
            Some(DocumentFormat::Docx)
        } else {
            return Err(doc_err(CoreErrorCode::DocCorrupt));
        }
    } else if matches!(ext.as_str(), "md" | "markdown") {
        // Markdown 必须可按 UTF-8 解码(BOM 接受)。
        if is_valid_utf8_file(path, size, limits.markdown_mib)? {
            Some(DocumentFormat::Markdown)
        } else {
            return Err(doc_err(CoreErrorCode::DocCorrupt));
        }
    } else {
        None
    };

    let Some(format) = detected else {
        return Err(doc_err(CoreErrorCode::DocUnsupportedFormat));
    };

    // 签名与扩展名组合必须自洽;冲突按损坏/不支持报告。
    if let Some(exp) = expected {
        if exp != format {
            return Err(match format {
                DocumentFormat::Pdf | DocumentFormat::Docx => doc_err(CoreErrorCode::DocCorrupt),
                DocumentFormat::Markdown => doc_err(CoreErrorCode::DocUnsupportedFormat),
            });
        }
    }

    // 大小上限:超限直接拒绝,不读入内存后崩溃(《M02》§2)。
    let limit = limits.for_format(format);
    if size > limit * 1024 * 1024 {
        return Err(doc_err(CoreErrorCode::DocTooLarge)
            .with_detail("sizeBytes", (size as i64).into())
            .with_detail("limitMiB", (limit as i64).into()));
    }

    Ok(DetectedFile {
        format,
        size,
        mtime_ns,
    })
}

fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        let n = f.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

/// DOCX 必要条目检查:在文件尾部的 ZIP 中央目录区寻找两个条目名。
/// 只做只读扫描,不完整解压(防 ZIP 炸弹由后续解析层负责)。
fn has_docx_entries(path: &Path) -> Result<bool, AppError> {
    const TAIL: u64 = 1024 * 1024;
    let meta = std::fs::metadata(path).map_err(|_| doc_err(CoreErrorCode::DocFileMissing))?;
    let scan_len = TAIL.min(meta.len());
    let mut f = std::fs::File::open(path).map_err(|_| doc_err(CoreErrorCode::DocFileMissing))?;
    f.seek(SeekFrom::End(-(scan_len as i64)))
        .map_err(|_| doc_err(CoreErrorCode::DocCorrupt))?;
    let mut buf = vec![0u8; scan_len as usize];
    let mut off = 0usize;
    while off < buf.len() {
        let n = f
            .read(&mut buf[off..])
            .map_err(|_| doc_err(CoreErrorCode::DocCorrupt))?;
        if n == 0 {
            break;
        }
        off += n;
    }
    buf.truncate(off);
    Ok(buf
        .windows(b"[Content_Types].xml".len())
        .any(|w| w == b"[Content_Types].xml")
        && buf
            .windows(b"word/document.xml".len())
            .any(|w| w == b"word/document.xml"))
}

fn is_valid_utf8_file(path: &Path, size: u64, limit_mib: u64) -> Result<bool, AppError> {
    use std::io::BufRead;
    if size > limit_mib * 1024 * 1024 {
        return Err(doc_err(CoreErrorCode::DocTooLarge));
    }
    let f = std::fs::File::open(path).map_err(|_| doc_err(CoreErrorCode::DocFileMissing))?;
    let mut reader = std::io::BufReader::new(f);
    let mut chunk = Vec::with_capacity(64 * 1024);
    // 逐块消费并累积未完成的字节序列判断;std 的 from_utf8 已处理。
    let mut pending: Vec<u8> = Vec::new();
    loop {
        chunk.clear();
        let n = reader
            .read_until(b'\n', &mut chunk)
            .map_err(|_| doc_err(CoreErrorCode::DocCorrupt))?;
        if n == 0 {
            break;
        }
        pending.extend_from_slice(&chunk);
        // 找到最后一个完整 UTF-8 边界,前缀校验,剩余留待下一轮。
        if let Some(valid) = valid_utf8_prefix(&pending) {
            let rest = pending.split_off(valid);
            pending = rest;
        } else {
            return Ok(false);
        }
        if pending.len() > 4 {
            // 无效序列。
            return Ok(false);
        }
    }
    if pending.is_empty() {
        Ok(true)
    } else {
        Ok(std::str::from_utf8(&pending).is_ok())
    }
}

fn valid_utf8_prefix(bytes: &[u8]) -> Option<usize> {
    match std::str::from_utf8(bytes) {
        Ok(_) => Some(bytes.len()),
        Err(e) => {
            let valid = e.valid_up_to();
            if e.error_len().is_some() {
                None // 确定无效。
            } else {
                Some(valid) // 尾部不完整,继续读。
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kw-detect-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn tiny_zip_with_entries() -> Vec<u8> {
        // 构造最小合法 ZIP:两个 STORE 条目。用简单手工编码。
        fn entry(name: &str) -> Vec<u8> {
            let mut e = Vec::new();
            e.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]); // local header
            e.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // ver/flags/method/time/date
            e.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // crc/comp/uncomp
            let n = name.len() as u16;
            e.extend_from_slice(&(n).to_le_bytes());
            e.extend_from_slice(&0u16.to_le_bytes());
            e.extend_from_slice(name.as_bytes());
            e
        }
        let mut z = entry("[Content_Types].xml");
        z.extend(entry("word/document.xml"));
        // 中央目录
        let mut cd = Vec::new();
        for name in ["[Content_Types].xml", "word/document.xml"] {
            cd.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
            cd.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0]);
            cd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // crc/sizes
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // extra/comment/disk/attrs/offset
            cd.extend_from_slice(name.as_bytes());
        }
        z.extend_from_slice(&cd);
        z.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]); // EOCD
        z.extend_from_slice(&[0; 16]);
        z
    }

    #[test]
    fn detects_markdown_pdf_docx() {
        let limits = SizeLimits::default();
        let md = tmp("ok", "note.md", "# 标题\n\n正文".as_bytes());
        let pdf = tmp("ok", "doc.pdf", b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n...");
        let docx = tmp("ok", "doc.docx", &tiny_zip_with_entries());
        assert_eq!(
            detect_file(&md, &limits).unwrap().format,
            DocumentFormat::Markdown
        );
        assert_eq!(
            detect_file(&pdf, &limits).unwrap().format,
            DocumentFormat::Pdf
        );
        assert_eq!(
            detect_file(&docx, &limits).unwrap().format,
            DocumentFormat::Docx
        );
    }

    #[test]
    fn utf8_bom_markdown_accepted() {
        let limits = SizeLimits::default();
        let md = tmp(
            "bom",
            "b.md",
            &[0xEF, 0xBB, 0xBF, b'#', b' ', 0xe4, 0xb8, 0xad],
        );
        assert_eq!(
            detect_file(&md, &limits).unwrap().format,
            DocumentFormat::Markdown
        );
    }

    #[test]
    fn error_matrix() {
        // M02-A1:空、损坏、加密、旧 doc、超限、非支持格式。
        let limits = SizeLimits {
            pdf_mib: 0,
            docx_mib: 0,
            markdown_mib: 0,
        };

        let empty = tmp("err", "e.md", b"");
        let code = |e: AppError| e.code.clone();
        assert_eq!(
            code(detect_file(&empty, &SizeLimits::default()).unwrap_err()),
            "DOC_EMPTY"
        );

        let legacy = tmp("err", "old.doc", CFB_MAGIC);
        assert_eq!(
            code(detect_file(&legacy, &SizeLimits::default()).unwrap_err()),
            "DOC_UNSUPPORTED_FORMAT"
        );

        let encrypted = tmp("err", "enc.docx", CFB_MAGIC);
        assert_eq!(
            code(detect_file(&encrypted, &SizeLimits::default()).unwrap_err()),
            "DOC_ENCRYPTED"
        );

        let broken_zip = tmp("err", "b.docx", &[0x50, 0x4B, 0x03, 0x04, 1, 2, 3]);
        assert_eq!(
            code(detect_file(&broken_zip, &SizeLimits::default()).unwrap_err()),
            "DOC_CORRUPT"
        );

        let bad_utf8 = tmp(
            "err",
            "u.md",
            &[0xff, 0xfe, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05],
        );
        assert_eq!(
            code(detect_file(&bad_utf8, &SizeLimits::default()).unwrap_err()),
            "DOC_CORRUPT"
        );

        let unknown = tmp("err", "x.txt", b"hello world");
        assert_eq!(
            code(detect_file(&unknown, &SizeLimits::default()).unwrap_err()),
            "DOC_UNSUPPORTED_FORMAT"
        );

        let big = tmp("err", "big.md", &[0x41; 4096]);
        assert_eq!(
            code(detect_file(&big, &limits).unwrap_err()),
            "DOC_TOO_LARGE"
        );

        // 签名与扩展名冲突:PDF 头但 .md 扩展名。
        let mismatch = tmp("err", "m.md", b"%PDF-1.4 fake");
        assert_eq!(
            code(detect_file(&mismatch, &SizeLimits::default()).unwrap_err()),
            "DOC_CORRUPT"
        );
    }
}
