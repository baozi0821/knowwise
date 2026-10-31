//! 导出器根(M10 摘要 Markdown/docx、M12 图形 SVG)。各模块按子目录拥有。

pub mod docx;
pub mod graph;
pub mod markdown;

use crate::artifacts::models::ExportResult;
use crate::artifacts::service::ArtifactService;
use crate::shared::{new_trace_id, AppError, Clock, CoreErrorCode, IdGenerator};
use std::path::Path;
use std::sync::Arc;

/// 导出格式白名单(WBS-4C)。新增格式必须同时提供渲染器与测试;
/// 未列入的格式在命令层就被稳定拒绝,不会触碰文件系统或数据库。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Docx,
}

impl ExportFormat {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "markdown" => Some(Self::Markdown),
            "docx" => Some(Self::Docx),
            _ => None,
        }
    }
}

/// 格式校验(命令层与导出门面共用):未知格式 ⇒ INVALID_REQUEST。
pub fn parse_export_format(raw: &str) -> Result<ExportFormat, AppError> {
    ExportFormat::parse(raw).ok_or_else(|| {
        AppError::new(CoreErrorCode::InvalidRequest, "export")
            .with_detail("reason", "unsupported_format".into())
            .with_detail("format", raw.to_string().into())
    })
}

/// 导出依赖(格式无关;各导出器转换为自己的 deps)。
pub struct ExportDeps<'a> {
    pub service: &'a ArtifactService,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    /// translation 导出变体:bilingual(默认) / translation_only。
    pub variant: &'a str,
}

/// 导出门面:命令层唯一入口,格式分支集中于此。
pub fn export_artifact(
    deps: &ExportDeps<'_>,
    artifact_id: &str,
    version_id: &str,
    format: &str,
    path: &Path,
) -> Result<ExportResult, AppError> {
    match parse_export_format(format)? {
        ExportFormat::Markdown => {
            let deps = markdown::MarkdownExportDeps {
                service: deps.service,
                clock: deps.clock.clone(),
                ids: deps.ids.clone(),
                variant: deps.variant,
            };
            markdown::export_artifact_markdown(&deps, artifact_id, version_id, path)
        }
        ExportFormat::Docx => {
            let deps = docx::DocxExportDeps {
                service: deps.service,
                clock: deps.clock.clone(),
                ids: deps.ids.clone(),
                variant: deps.variant,
            };
            docx::export_artifact_docx(&deps, artifact_id, version_id, path)
        }
    }
}

/// 目标文件写入:已存在绝不覆盖;权限/磁盘错误映射稳定错误码。
/// Markdown 与 docx 共用,保证两种格式的落盘语义一致(《M10》§8/§10)。
pub(crate) fn write_export_file(path: &Path, bytes: &[u8]) -> Result<i64, AppError> {
    if path.exists() {
        return Err(
            AppError::new(CoreErrorCode::ExportTargetExists, new_trace_id())
                .with_detail("reason", "target_exists".into()),
        );
    }
    if path.as_os_str().is_empty() {
        return Err(AppError::new(CoreErrorCode::InvalidRequest, new_trace_id())
            .with_detail("reason", "export_path_empty".into()));
    }
    match std::fs::write(path, bytes) {
        Ok(()) => Ok(bytes.len() as i64),
        Err(e) => match e.kind() {
            std::io::ErrorKind::PermissionDenied => Err(AppError::new(
                CoreErrorCode::ExportPermissionDenied,
                new_trace_id(),
            )
            .with_detail("reason", "permission_denied".into())),
            std::io::ErrorKind::StorageFull => {
                Err(AppError::new(CoreErrorCode::ExportDiskFull, new_trace_id())
                    .with_detail("reason", "disk_full".into()))
            }
            _ => Err(AppError::new(CoreErrorCode::Internal, new_trace_id())
                .with_detail("reason", e.to_string().into())),
        },
    }
}

/// 确定性日期(UTC,1970 起的天数 → 年月日 civil 算法;不依赖本机时区)。
/// Markdown 与 docx 共用,时间只取版本创建时间。
pub(crate) fn utc_date(epoch_ms: i64) -> String {
    let secs = epoch_ms / 1000;
    let days = secs / 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::error::DetailValue;

    #[test]
    fn known_formats_are_whitelisted() {
        assert_eq!(
            parse_export_format("markdown").unwrap(),
            ExportFormat::Markdown
        );
        assert_eq!(parse_export_format("docx").unwrap(), ExportFormat::Docx);
    }

    #[test]
    fn unknown_format_is_rejected_with_stable_code() {
        let e = parse_export_format("pdf").unwrap_err();
        assert_eq!(e.code, "INVALID_REQUEST");
        let details = e.details.expect("details");
        assert_eq!(
            details.get("reason"),
            Some(&DetailValue::Str("unsupported_format".into()))
        );
        assert_eq!(details.get("format"), Some(&DetailValue::Str("pdf".into())));
    }

    #[test]
    fn utc_date_is_stable_across_timezones() {
        // 2025-10-23T00:00:00Z
        assert_eq!(utc_date(1_761_177_600_000), "2025-10-23");
        assert_eq!(utc_date(0), "1970-01-01");
    }
}
