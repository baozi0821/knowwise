//! 笔记 Markdown 导出(capture 实现,`notes_export_markdown`)。
//!
//! 前端流程(DESIGN §2/§3):`dialogSavePath()` 选路径 →
//! `client.call("notes_export_markdown", { documentId, path, scope, noteId?, includeAnnotations? })`。
//!
//! 内容:中文 Markdown,含文档标题、导出时间、笔记标题(正文首行派生)、
//! 创建/更新时间、状态、来源、标签、正文、出处引用与(可选)文档标注引文。
//!
//! 数据只经 `NotesService` / `DocumentService` 的既有公开方法读取:
//! - 文档标题:`DocumentService::get_document`;
//! - 笔记:`NotesService::list_notes`(游标分页,逐页取全)/ `get_note`;
//! - 标注:`NotesService::list_annotations`。
//!
//! Markdown 生成与时间格式化是不依赖 Tauri 的纯函数,默认特性下即可单测;
//! `#[tauri::command]` 包装只在 `desktop` feature 下编译。

use serde::{Deserialize, Serialize};

use super::models::{
    AnnotationColor, AnnotationView, NoteOrigin, NoteStatus, NoteView, SourceRefType,
    SourceReference,
};
use crate::shared::error::CoreErrorCode;
use crate::shared::AppError;

/// 导出范围:整篇文档的全部笔记,或单条笔记。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotesExportScope {
    Document,
    Note,
}

/// `notes_export_markdown` 请求(IPC 为 camelCase)。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesExportRequest {
    pub document_id: String,
    /// 用户经保存对话框选定的目标路径(只来自用户选择)。
    pub path: String,
    pub scope: NotesExportScope,
    #[serde(default)]
    pub note_id: Option<String>,
    /// 是否附带文档标注引文;文档范围默认 true,单条笔记默认 false。
    #[serde(default)]
    pub include_annotations: Option<bool>,
}

/// 导出结果:实际写入字节数与目标路径。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesExportResult {
    pub bytes_written: u64,
    pub path: String,
}

const TRACE: &str = "notes_export_markdown";

fn invalid_request(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, TRACE).with_detail("reason", reason.into())
}

#[cfg(feature = "desktop")]
fn not_found(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::NotFound, TRACE).with_detail("reason", reason.into())
}

#[cfg(feature = "desktop")]
fn write_error(error: &std::io::Error) -> AppError {
    let code = match error.kind() {
        std::io::ErrorKind::PermissionDenied => CoreErrorCode::ExportPermissionDenied,
        // ERROR_DISK_FULL / ERROR_HANDLE_DISK_FULL(Windows)。
        _ if matches!(error.raw_os_error(), Some(112) | Some(39)) => CoreErrorCode::ExportDiskFull,
        _ => CoreErrorCode::Internal,
    };
    let reason = match error.kind() {
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        std::io::ErrorKind::NotFound => "parent_not_found",
        _ if matches!(error.raw_os_error(), Some(112) | Some(39)) => "disk_full",
        _ => "write_failed",
    };
    AppError::new(code, TRACE).with_detail("reason", reason.into())
}

/// 校验请求(不触碰数据库与文件系统,便于单测)。
pub fn validate_request(request: &NotesExportRequest) -> Result<(), AppError> {
    if request.document_id.trim().is_empty() {
        return Err(invalid_request("missing_document_id"));
    }
    if request.path.trim().is_empty() {
        return Err(invalid_request("missing_path"));
    }
    if request.path.contains('\0') {
        return Err(invalid_request("invalid_path"));
    }
    if request.scope == NotesExportScope::Note
        && request
            .note_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
    {
        return Err(invalid_request("note_scope_requires_note_id"));
    }
    Ok(())
}

/// 逐页读取文档全部笔记(游标分页,每页 200 条)。
///
/// `NotesService::list_notes` 的游标指向「多取一条」的探测行,下一页的
/// 严格小于语义会跳过该行;这里用游标内嵌的 id 通过 `get_note` 补回,
/// 避免导出漏笔记(按 id 去重,顺序在渲染时重排)。
#[cfg(feature = "desktop")]
fn load_all_notes(
    notes: &super::NotesService,
    document_id: &str,
) -> Result<Vec<NoteView>, AppError> {
    const PAGE: i64 = 200;
    const MAX_NOTES: usize = 5_000;

    let mut all: Vec<NoteView> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = notes.list_notes(
            document_id,
            &super::models::NoteListFilter::default(),
            crate::shared::paging::PageRequest {
                limit: PAGE,
                cursor: cursor.clone(),
            },
        )?;
        for note in page.items {
            if seen.insert(note.id.clone()) {
                all.push(note);
            }
        }
        let Some(next) = page.next_cursor.clone() else {
            break;
        };
        if Some(&next) == cursor.as_ref() {
            break; // 防御:游标不前进
        }
        // 补回被分页探测跳过的边界行(仅当能解出 id)。
        if let Some(value) = crate::shared::paging::decode_cursor::<serde_json::Value>(&next) {
            if let Some(id) = value.get("id").and_then(|v| v.as_str()) {
                if !seen.contains(id) {
                    if let Ok(note) = notes.get_note(id) {
                        if note.document_id == document_id && seen.insert(note.id.clone()) {
                            all.push(note);
                        }
                    }
                }
            }
        }
        cursor = Some(next);
        if all.len() >= MAX_NOTES {
            break;
        }
    }
    Ok(all)
}

// —— 时间格式化(不依赖 chrono;本地偏移由 Win32 GetLocalTime 差值得到)——

/// 以分钟为单位的本地时区偏移(东八区 = 480)。非 Windows 返回 0(UTC)。
#[cfg(windows)]
pub fn current_local_offset_minutes() -> i32 {
    #[repr(C)]
    struct SystemTimeRaw {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(out: *mut SystemTimeRaw);
        fn GetSystemTime(out: *mut SystemTimeRaw);
    }

    fn minutes_of_day(raw: &SystemTimeRaw) -> i32 {
        raw.hour as i32 * 60 + raw.minute as i32
    }

    unsafe {
        let mut local = SystemTimeRaw {
            year: 0,
            month: 0,
            day_of_week: 0,
            day: 0,
            hour: 0,
            minute: 0,
            second: 0,
            milliseconds: 0,
        };
        let mut utc = SystemTimeRaw {
            year: 0,
            month: 0,
            day_of_week: 0,
            day: 0,
            hour: 0,
            minute: 0,
            second: 0,
            milliseconds: 0,
        };
        GetLocalTime(&mut local);
        GetSystemTime(&mut utc);
        let mut diff = minutes_of_day(&local) - minutes_of_day(&utc);
        if diff > 12 * 60 {
            diff -= 24 * 60;
        } else if diff < -12 * 60 {
            diff += 24 * 60;
        }
        diff
    }
}

#[cfg(not(windows))]
pub fn current_local_offset_minutes() -> i32 {
    0
}

/// 公历日期换算(Howard Hinnant civil_from_days 算法,无时区/闰年表)。
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// epoch 毫秒 → `YYYY-MM-DD HH:mm`(按 `offset_minutes` 的本地时区)。
pub fn format_epoch_ms(ms: i64, offset_minutes: i32) -> String {
    let local_ms = ms + offset_minutes as i64 * 60_000;
    let days = local_ms.div_euclid(86_400_000);
    let day_ms = local_ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = day_ms / 3_600_000;
    let minute = (day_ms % 3_600_000) / 60_000;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

// —— 中文展示映射 ——

fn origin_label(origin: NoteOrigin) -> &'static str {
    match origin {
        NoteOrigin::User => "用户笔记",
        NoteOrigin::AiDraft => "AI 草稿",
        NoteOrigin::UserEditedAi => "AI 草稿(已编辑)",
    }
}

fn status_label(status: NoteStatus) -> &'static str {
    match status {
        NoteStatus::Open => "待整理",
        NoteStatus::Confirmed => "已确认",
        NoteStatus::NeedsClarification => "待澄清",
    }
}

fn color_label(color: AnnotationColor) -> &'static str {
    match color {
        AnnotationColor::Yellow => "黄色",
        AnnotationColor::Green => "绿色",
        AnnotationColor::Blue => "蓝色",
        AnnotationColor::Pink => "粉色",
        AnnotationColor::Purple => "紫色",
    }
}

fn source_type_label(kind: SourceRefType) -> &'static str {
    match kind {
        SourceRefType::ContentBlock => "文档片段",
        SourceRefType::Note => "笔记",
        SourceRefType::Message => "对话消息",
        SourceRefType::ArtifactVersion => "产物版本",
        SourceRefType::Selection => "选中文本",
        SourceRefType::Annotation => "标注",
    }
}

/// 笔记标题:正文首行去掉 Markdown 标记;空正文回退「无标题笔记」。
pub fn derive_note_title(body: &str) -> String {
    let first_line = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut title = first_line.trim();
    // 去掉标题/引用/列表/加粗等行首标记(最多剥几层,如 "> ## 标题")。
    loop {
        let stripped = title
            .trim_start_matches(|c: char| {
                matches!(
                    c,
                    '#' | '>' | '*' | '-' | '+' | '_' | '`' | '=' | ' ' | '\t'
                ) || c.is_whitespace()
            })
            .trim_start();
        if stripped.len() == title.len() {
            break;
        }
        title = stripped;
    }
    let title = title.trim_end_matches(['*', '_', '`', ' ', '\t']).trim();
    if title.is_empty() {
        return "无标题笔记".to_string();
    }
    let mut chars = title.chars();
    let truncated: String = chars.by_ref().take(60).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn inline_code(text: &str) -> String {
    format!("`{}`", text.trim().replace('`', "'").replace('\n', " "))
}

fn render_tags(tags: &[String]) -> String {
    tags.iter()
        .filter(|tag| !tag.trim().is_empty())
        .map(|tag| inline_code(tag))
        .collect::<Vec<_>>()
        .join("、")
}

/// 多行文本渲染为 Markdown 引用块。
fn block_quote(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {}", line.trim_end()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_source_refs(note: &NoteView) -> Option<String> {
    if note.source_refs.is_empty() {
        return None;
    }
    let lines: Vec<String> = note
        .source_refs
        .iter()
        .enumerate()
        .map(|(index, source): (usize, &SourceReference)| {
            let available = note
                .source_refs_available
                .get(index)
                .copied()
                .unwrap_or(true);
            let label = source
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| source_type_label(source.source_type).to_string());
            let mut line = format!("- 出处:{label}");
            if !source.citation_ids.is_empty() {
                line.push_str(&format!("(引用 {} 处)", source.citation_ids.len()));
            }
            if !available {
                line.push_str("(来源已不可用)");
            }
            line
        })
        .collect();
    Some(lines.join("\n"))
}

fn render_annotations_section(out: &mut String, annotations: &[AnnotationView]) {
    if annotations.is_empty() {
        return;
    }
    out.push_str("## 标注\n\n");
    for (index, annotation) in annotations.iter().enumerate() {
        out.push_str(&format!("### 标注 {}\n\n", index + 1));
        if !annotation.quote_text.trim().is_empty() {
            out.push_str(&block_quote(&annotation.quote_text));
            out.push_str("\n\n");
        }
        let mut meta = format!("- 颜色:{}", color_label(annotation.color));
        if let Some(page) = annotation.page_number {
            meta.push_str(&format!(" · 页码:{page}"));
        }
        meta.push_str(&format!(" · 状态:{}", annotation.anchor_status.as_str()));
        out.push_str(&meta);
        out.push('\n');
        if !annotation.note_text.trim().is_empty() {
            out.push_str(&format!(
                "- 批注:{}\n",
                annotation.note_text.trim().replace('\n', " ")
            ));
        }
        out.push('\n');
    }
}

fn render_note_block(
    out: &mut String,
    heading: Option<usize>,
    note: &NoteView,
    offset_minutes: i32,
) {
    let title = derive_note_title(&note.body);
    match heading {
        Some(index) => out.push_str(&format!("## {index}. {title}\n\n")),
        None => out.push_str(&format!("# {title}\n\n")),
    }
    out.push_str(&format!(
        "- 创建时间:{}\n",
        format_epoch_ms(note.created_at, offset_minutes)
    ));
    out.push_str(&format!(
        "- 更新时间:{}\n",
        format_epoch_ms(note.updated_at, offset_minutes)
    ));
    out.push_str(&format!("- 状态:{}\n", status_label(note.status)));
    out.push_str(&format!("- 来源:{}\n", origin_label(note.origin)));
    if !note.tags.is_empty() {
        out.push_str(&format!("- 标签:{}\n", render_tags(&note.tags)));
    }
    if let Some(sources) = render_source_refs(note) {
        out.push_str(&sources);
        out.push('\n');
    }
    out.push('\n');
    let body = note.body.trim();
    if body.is_empty() {
        out.push_str("_（正文为空）_\n\n");
    } else {
        out.push_str(body);
        out.push_str("\n\n");
    }
    if let Some(anchor) = &note.anchor_id {
        out.push_str(&format!("> 锚点:{}\n\n", inline_code(anchor)));
    }
    out.push_str("---\n\n");
}

fn document_title_or_default(document_title: &str) -> &str {
    let trimmed = document_title.trim();
    if trimmed.is_empty() {
        "未命名文档"
    } else {
        trimmed
    }
}

fn export_header(
    out: &mut String,
    document_title: &str,
    note_count: usize,
    annotation_count: usize,
    exported_at_ms: i64,
    offset_minutes: i32,
) {
    out.push_str(&format!("# {document_title} · 笔记导出\n\n"));
    out.push_str(&format!(
        "- 导出时间:{}\n",
        format_epoch_ms(exported_at_ms, offset_minutes)
    ));
    out.push_str(&format!("- 文档:{document_title}\n"));
    out.push_str(&format!("- 笔记数量:{note_count}\n"));
    if annotation_count > 0 {
        out.push_str(&format!("- 标注数量:{annotation_count}\n"));
    }
    out.push_str("\n---\n\n");
}

/// 文档范围 Markdown(全部笔记 + 可选文档标注附录)。
pub fn render_document_markdown(
    document_title: &str,
    notes: &[NoteView],
    annotations: &[AnnotationView],
    exported_at_ms: i64,
    offset_minutes: i32,
) -> String {
    let title = document_title_or_default(document_title);
    let mut out = String::new();
    export_header(
        &mut out,
        title,
        notes.len(),
        annotations.len(),
        exported_at_ms,
        offset_minutes,
    );
    if notes.is_empty() {
        out.push_str("_该文档暂无笔记。_\n\n");
    }
    for (index, note) in notes.iter().enumerate() {
        render_note_block(&mut out, Some(index + 1), note, offset_minutes);
    }
    if !annotations.is_empty() {
        render_annotations_section(&mut out, annotations);
    }
    out.trim_end().to_string() + "\n"
}

/// 单条笔记 Markdown(含文档标题与可选标注附录)。
pub fn render_note_markdown(
    document_title: &str,
    note: &NoteView,
    annotations: &[AnnotationView],
    exported_at_ms: i64,
    offset_minutes: i32,
) -> String {
    let title = document_title_or_default(document_title);
    let note_title = derive_note_title(&note.body);
    let mut out = String::new();
    out.push_str(&format!("# {note_title}\n\n"));
    out.push_str(&format!(
        "- 导出时间:{}\n",
        format_epoch_ms(exported_at_ms, offset_minutes)
    ));
    out.push_str(&format!("- 文档:{title}\n"));
    out.push_str(&format!(
        "- 创建时间:{}\n",
        format_epoch_ms(note.created_at, offset_minutes)
    ));
    out.push_str(&format!(
        "- 更新时间:{}\n",
        format_epoch_ms(note.updated_at, offset_minutes)
    ));
    out.push_str(&format!("- 状态:{}\n", status_label(note.status)));
    out.push_str(&format!("- 来源:{}\n", origin_label(note.origin)));
    if !note.tags.is_empty() {
        out.push_str(&format!("- 标签:{}\n", render_tags(&note.tags)));
    }
    if let Some(sources) = render_source_refs(note) {
        out.push_str(&sources);
        out.push('\n');
    }
    out.push_str("\n---\n\n");
    let body = note.body.trim();
    if body.is_empty() {
        out.push_str("_（正文为空）_\n\n");
    } else {
        out.push_str(body);
        out.push_str("\n\n");
    }
    if !annotations.is_empty() {
        out.push_str("> 以下为所属文档的标注,非本笔记专属。\n\n");
        render_annotations_section(&mut out, annotations);
    }
    out.trim_end().to_string() + "\n"
}

// —— 读取与写盘(仅 desktop feature 编译,命令层薄封装)——

#[cfg(feature = "desktop")]
#[tauri::command]
pub fn notes_export_markdown(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: NotesExportRequest,
) -> Result<NotesExportResult, AppError> {
    validate_request(&request)?;
    let detail = state.documents.get_document(&request.document_id)?;
    let document_title = detail.summary.display_name;
    let include_annotations = request
        .include_annotations
        .unwrap_or(request.scope == NotesExportScope::Document);
    let annotations = if include_annotations {
        state.notes.list_annotations(&request.document_id)?
    } else {
        Vec::new()
    };
    let exported_at_ms = state.core.clock.now_ms();
    let offset_minutes = current_local_offset_minutes();

    let markdown = match request.scope {
        NotesExportScope::Document => {
            let notes = load_all_notes(&state.notes, &request.document_id)?;
            render_document_markdown(
                &document_title,
                &notes,
                &annotations,
                exported_at_ms,
                offset_minutes,
            )
        }
        NotesExportScope::Note => {
            let note_id = request.note_id.as_deref().unwrap_or_default();
            let note = state.notes.get_note(note_id)?;
            if note.document_id != request.document_id {
                return Err(not_found("note_document_mismatch"));
            }
            render_note_markdown(
                &document_title,
                &note,
                &annotations,
                exported_at_ms,
                offset_minutes,
            )
        }
    };

    let bytes = markdown.into_bytes();
    std::fs::write(&request.path, &bytes).map_err(|error| write_error(&error))?;
    Ok(NotesExportResult {
        bytes_written: bytes.len() as u64,
        path: request.path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::models::AnchorStatus;

    fn note(
        id: &str,
        body: &str,
        tags: &[&str],
        origin: NoteOrigin,
        status: NoteStatus,
    ) -> NoteView {
        NoteView {
            id: id.to_string(),
            document_id: "doc-1".to_string(),
            document_version_id: Some("ver-1".to_string()),
            anchor_id: None,
            anchor_status: Some(AnchorStatus::Active),
            origin,
            body: body.to_string(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            status,
            locator: None,
            source_refs: vec![SourceReference {
                source_type: SourceRefType::ContentBlock,
                source_id: "blk-1".to_string(),
                source_role: crate::notes::models::SourceRole::Original,
                display_name: Some("第一章 · 导论".to_string()),
                citation_ids: vec!["cit-1".to_string()],
                snapshot_available: true,
            }],
            source_refs_available: vec![true],
            revision: 1,
            created_at: 1_767_225_600_000,
            updated_at: 1_767_229_200_000,
        }
    }

    fn annotation() -> AnnotationView {
        AnnotationView {
            id: "ann-1".to_string(),
            document_id: "doc-1".to_string(),
            anchor_id: "anchor-1".to_string(),
            color: AnnotationColor::Yellow,
            note_text: "这里是重点".to_string(),
            remark: String::new(),
            anchor_status: AnchorStatus::Active,
            status_hint: "可回跳".to_string(),
            quote_text: "原文引用第一行\n第二行".to_string(),
            locator: None,
            page_number: Some(12),
            revision: 1,
            created_at: 1_767_225_600_000,
            updated_at: 1_767_229_200_000,
        }
    }

    #[test]
    fn result_serializes_camel_case() {
        let json = serde_json::to_value(NotesExportResult {
            bytes_written: 42,
            path: "C:\\tmp\\笔记.md".to_string(),
        })
        .unwrap();
        assert_eq!(json["bytesWritten"], 42);
        assert_eq!(json["path"], "C:\\tmp\\笔记.md");
        assert!(json.get("bytes_written").is_none());
    }

    #[test]
    fn request_deserializes_camel_case_with_optional_fields() {
        let request: NotesExportRequest = serde_json::from_str(
            r#"{"documentId":"d1","path":"C:\\n.md","scope":"note","noteId":"n1","includeAnnotations":true}"#,
        )
        .unwrap();
        assert_eq!(request.scope, NotesExportScope::Note);
        assert_eq!(request.note_id.as_deref(), Some("n1"));
        assert_eq!(request.include_annotations, Some(true));

        let request: NotesExportRequest =
            serde_json::from_str(r#"{"documentId":"d1","path":"a.md","scope":"document"}"#)
                .unwrap();
        assert_eq!(request.scope, NotesExportScope::Document);
        assert_eq!(request.note_id, None);
        assert_eq!(request.include_annotations, None);
    }

    #[test]
    fn validate_rejects_bad_requests() {
        let base = NotesExportRequest {
            document_id: "doc-1".to_string(),
            path: "C:\\tmp\\a.md".to_string(),
            scope: NotesExportScope::Document,
            note_id: None,
            include_annotations: None,
        };
        assert!(validate_request(&base).is_ok());

        let mut bad = NotesExportRequest { ..base };
        bad.document_id = "  ".to_string();
        assert!(validate_request(&bad).is_err());

        let mut bad = NotesExportRequest {
            document_id: "doc-1".to_string(),
            path: String::new(),
            scope: NotesExportScope::Document,
            note_id: None,
            include_annotations: None,
        };
        assert!(validate_request(&bad).is_err());

        bad.path = "C:\\tmp\\a.md".to_string();
        bad.scope = NotesExportScope::Note;
        assert!(validate_request(&bad).is_err());
        bad.note_id = Some("n1".to_string());
        assert!(validate_request(&bad).is_ok());
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn write_error_maps_permission_and_disk_full() {
        let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let mapped = write_error(&error);
        assert_eq!(mapped.code, "EXPORT_PERMISSION_DENIED");
        assert_eq!(mapped.user_action, crate::shared::error::UserAction::None);

        let error = std::io::Error::other("full");
        let mapped = write_error(&error);
        assert_eq!(mapped.code, "INTERNAL");
    }

    #[test]
    fn format_epoch_ms_formats_utc_and_offsets() {
        assert_eq!(format_epoch_ms(0, 0), "1970-01-01 00:00");
        // 2026-01-01T00:00:00Z,东八区 → 08:00
        assert_eq!(format_epoch_ms(1_767_225_600_000, 480), "2026-01-01 08:00");
        // 闰日 2024-02-29T00:00:00Z
        assert_eq!(format_epoch_ms(1_709_164_800_000, 0), "2024-02-29 00:00");
        // 负偏移(西五区):2026-01-01T00:00Z → 2025-12-31 19:00
        assert_eq!(format_epoch_ms(1_767_225_600_000, -300), "2025-12-31 19:00");
        // epoch 之前
        assert_eq!(format_epoch_ms(-86_400_000, 0), "1969-12-31 00:00");
    }

    #[test]
    fn derive_note_title_strips_markdown_markers() {
        assert_eq!(derive_note_title("# 标题\n正文"), "标题");
        assert_eq!(derive_note_title("> ## 引用标题"), "引用标题");
        assert_eq!(derive_note_title("**加粗首行**"), "加粗首行");
        assert_eq!(derive_note_title("- 列表项"), "列表项");
        assert_eq!(derive_note_title("\n\n  \n"), "无标题笔记");
        assert_eq!(derive_note_title(""), "无标题笔记");
    }

    #[test]
    fn derive_note_title_truncates_long_first_line() {
        let body = "很".repeat(80);
        let title = derive_note_title(&body);
        assert_eq!(title.chars().count(), 61);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn document_markdown_contains_title_time_tags_body_and_annotations() {
        let notes = vec![
            note(
                "n1",
                "# 阅读笔记\n\n这是一段**正文**。",
                &["方法", "待复习"],
                NoteOrigin::User,
                NoteStatus::Open,
            ),
            note(
                "n2",
                "AI 生成的摘要",
                &[],
                NoteOrigin::AiDraft,
                NoteStatus::Confirmed,
            ),
        ];
        let markdown = render_document_markdown(
            "认知心理学",
            &notes,
            &[annotation()],
            1_767_229_260_000,
            480,
        );
        assert!(markdown.starts_with("# 认知心理学 · 笔记导出"));
        assert!(markdown.contains("- 笔记数量:2"));
        assert!(markdown.contains("- 标注数量:1"));
        assert!(markdown.contains("## 1. 阅读笔记"));
        assert!(markdown.contains("## 2. AI 生成的摘要"));
        assert!(markdown.contains("- 状态:待整理"));
        assert!(markdown.contains("- 来源:AI 草稿"));
        assert!(markdown.contains("- 标签:`方法`、`待复习`"));
        assert!(markdown.contains("> 原文引用第一行\n> 第二行"));
        assert!(markdown.contains("这里是重点"));
        assert!(markdown.contains("- 出处:第一章 · 导论(引用 1 处)"));
        // 时区:exportedAt 1_767_229_260_000 = 2026-01-01 01:01 UTC → 09:01 东八区
        assert!(markdown.contains("2026-01-01 09:01"));
    }

    #[test]
    fn document_markdown_handles_empty_notes() {
        let markdown = render_document_markdown("空文档", &[], &[], 0, 0);
        assert!(markdown.contains("_该文档暂无笔记。_"));
        assert!(!markdown.contains("标注"));
    }

    #[test]
    fn note_markdown_renders_single_note_with_source() {
        let view = note(
            "n1",
            "单条笔记正文",
            &["标签A"],
            NoteOrigin::UserEditedAi,
            NoteStatus::NeedsClarification,
        );
        let markdown = render_note_markdown("认知心理学", &view, &[annotation()], 0, 0);
        assert!(markdown.starts_with("# 单条笔记正文"));
        assert!(markdown.contains("- 文档:认知心理学"));
        assert!(markdown.contains("- 状态:待澄清"));
        assert!(markdown.contains("- 来源:AI 草稿(已编辑)"));
        assert!(markdown.contains("非本笔记专属"));
        assert!(markdown.contains("## 标注"));
    }

    #[test]
    fn unavailable_source_is_marked() {
        let mut view = note("n1", "正文", &[], NoteOrigin::User, NoteStatus::Open);
        view.source_refs_available = vec![false];
        let markdown = render_note_markdown("文档", &view, &[], 0, 0);
        assert!(markdown.contains("(来源已不可用)"));
    }

    #[test]
    fn block_quote_prefixes_every_line() {
        assert_eq!(block_quote("a\nb"), "> a\n> b");
        assert_eq!(block_quote("单行"), "> 单行");
    }
}
