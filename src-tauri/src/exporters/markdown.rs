//! M10 Markdown 导出器(《M10》§8)。
//!
//! 确定性输出:同一 ArtifactVersion 与来源投影总是生成相同字节
//! (时间字段取版本创建时间,不取导出时刻)。用户笔记与 AI 推断
//! 使用明确标签,不把 AI 内容伪装为原文;不包含本机绝对路径。

use crate::artifacts::models::{
    CoverageReport, SummaryContentV1, SummarySection, SummarySegmentKind,
};
use crate::artifacts::service::ExportProjection;
use crate::shared::{AppError, Clock, CoreErrorCode};
use std::path::Path;

/// 导出前统一清洗:导出文件里同样不得出现 SOURCE 标记、字面换行转义或 HTML 实体。
fn sanitize(text: &str) -> String {
    crate::shared::sanitize::sanitize_generated_markdown(text)
}

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

/// 段落来源标签(§8:AI 推断与用户观点显式标注)。
fn segment_label(kind: SummarySegmentKind) -> &'static str {
    match kind {
        SummarySegmentKind::DocumentFact => "",
        SummarySegmentKind::AiInference => "AI 推断",
        SummarySegmentKind::UserView => "用户观点",
        SummarySegmentKind::NeedsVerification => "待核实",
    }
}

/// 来源面板投影:引用快照(原文块)与用户笔记分开显示(§7/§8)。
#[derive(Debug, Clone, Default)]
pub struct MarkdownSourceProjection {
    /// citation_id → (原文页码可选, 标题路径, 引文)。
    pub citations: Vec<MarkdownCitation>,
    /// 用户笔记来源(区别于 AI/原文;显示为「用户笔记」)。
    pub notes: Vec<MarkdownNoteSource>,
}

#[derive(Debug, Clone)]
pub struct MarkdownCitation {
    pub citation_id: String,
    /// 结论段标识(= summary segment id);脚注按它关联,不按本地草案 ID。
    pub claim_key: String,
    pub page_number: Option<i64>,
    pub heading_path: Vec<String>,
    pub quoted_text: String,
}

#[derive(Debug, Clone)]
pub struct MarkdownNoteSource {
    pub note_id: String,
    pub text: String,
}

/// 覆盖行:完整标注范围;partial 不宣称全文覆盖(§5/§8)。
fn format_coverage(coverage: &CoverageReport) -> String {
    if coverage.regions.is_empty() {
        return if coverage.complete {
            "全文(完整)".into()
        } else {
            "部分内容,有缺失".into()
        };
    }
    let labels: Vec<String> = coverage.regions.iter().map(|r| r.label.clone()).collect();
    let joined = labels.join("、");
    if coverage.complete {
        format!("{joined}(完整)")
    } else {
        format!("{joined}(部分,有缺失)")
    }
}

fn section_heading(title: &str) -> String {
    format!("## {title}")
}

/// quiz.v1/v2 → Markdown:题目、选项、答案、解析与引用(无正文编辑)。
fn render_quiz_markdown(p: &ExportProjection, value: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", p.artifact_title));
    out.push_str(&format!("> 文档:{}\n", p.document_title));
    out.push_str(&format!("> 版本:第 {} 版\n", p.version_no));
    out.push_str(&format!(
        "> 生成:{}\n\n",
        crate::exporters::utc_date(p.created_at)
    ));
    let Some(questions) = value.get("questions").and_then(|v| v.as_array()) else {
        return out;
    };
    for (index, question) in questions.iter().enumerate() {
        let prompt = question
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(sanitize)
            .unwrap_or_else(|| "(缺少题干)".to_string());
        let kind = question
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("single_choice");
        out.push_str(&format!(
            "## 第 {} 题 · {}\n\n",
            index + 1,
            quiz_type_label(kind)
        ));
        out.push_str(&format!("{prompt}\n\n"));
        if let Some(options) = question.get("options").and_then(|v| v.as_array()) {
            for (option_index, option) in options.iter().enumerate() {
                let text = option
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(sanitize)
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- {}. {}\n",
                    (b'A' + option_index as u8) as char,
                    text
                ));
            }
            out.push('\n');
        }
        if let Some(blanks) = question.get("blanks").and_then(|v| v.as_array()) {
            for (blank_index, blank) in blanks.iter().enumerate() {
                let accepted = blank
                    .get("acceptedAnswers")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    })
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- 空 {}:____ (可接受:{accepted})\n",
                    blank_index + 1
                ));
            }
            out.push('\n');
        }
        let answer = sanitize(&quiz_answer_text(question));
        if !answer.is_empty() {
            out.push_str(&format!("**答案**:{answer}\n\n"));
        }
        if let Some(segments) = question
            .get("explanationSegments")
            .and_then(|v| v.as_array())
        {
            if !segments.is_empty() {
                out.push_str("**解析**:\n\n");
                for segment in segments {
                    if let Some(text) = segment.get("text").and_then(|v| v.as_str()) {
                        out.push_str(&format!("- {text}\n"));
                    }
                }
                out.push('\n');
            }
        }
        if let Some(citations) = question.get("citations").and_then(|v| v.as_array()) {
            if !citations.is_empty() {
                out.push_str("**引用**:\n\n");
                for citation in citations {
                    if let Some(quote) = citation.get("quotedText").and_then(|v| v.as_str()) {
                        out.push_str(&format!("- 「{quote}」\n"));
                    }
                }
                out.push('\n');
            }
        }
    }
    out
}

fn quiz_type_label(kind: &str) -> &'static str {
    match kind {
        "multiple_choice" => "多选",
        "true_false" => "判断",
        "fill_blank" => "填空",
        "short_answer" => "问答",
        _ => "单选",
    }
}

fn quiz_answer_text(question: &serde_json::Value) -> String {
    let options = question
        .get("options")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let option_text = |id: &str| {
        options
            .iter()
            .find(|option| option.get("id").and_then(|v| v.as_str()) == Some(id))
            .and_then(|option| option.get("text").and_then(|v| v.as_str()))
            .unwrap_or(id)
            .to_string()
    };
    if let Some(answer) = question.get("answer") {
        if let Some(ids) = answer.get("correctOptionIds").and_then(|v| v.as_array()) {
            if !ids.is_empty() {
                return ids
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(option_text)
                    .collect::<Vec<_>>()
                    .join(" / ");
            }
        }
        if let Some(reference) = answer
            .get("referenceAnswer")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        {
            return reference.to_string();
        }
    }
    if let Some(id) = question.get("correctOptionId").and_then(|v| v.as_str()) {
        return option_text(id);
    }
    String::new()
}

/// translation.v1 → Markdown:双语(默认)或仅译文;失败段显式说明。
fn render_translation_markdown(
    p: &ExportProjection,
    value: &serde_json::Value,
    variant: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", p.artifact_title));
    out.push_str(&format!("> 文档:{}\n", p.document_title));
    out.push_str(&format!("> 版本:第 {} 版\n", p.version_no));
    out.push_str(&format!(
        "> 生成:{}\n",
        crate::exporters::utc_date(p.created_at)
    ));
    let source_language = value
        .get("sourceLanguage")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let target_language = value
        .get("targetLanguage")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    out.push_str(&format!("> 语言:{source_language} → {target_language}\n\n"));
    let only_translation = variant == "translation_only";
    if let Some(segments) = value.get("segments").and_then(|v| v.as_array()) {
        for segment in segments {
            let page = segment.get("pageNumber").and_then(|v| v.as_i64());
            let status = segment
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("translated");
            let confidence = segment
                .get("confidence")
                .and_then(|v| v.as_str())
                .unwrap_or("medium");
            out.push_str(&format!(
                "## {}\n\n",
                page.map(|p| format!("p.{p}"))
                    .unwrap_or_else(|| "段落".into())
            ));
            out.push_str(&format!("> 置信度:{confidence} · 状态:{status}\n\n"));
            if !only_translation {
                if let Some(source) = segment.get("sourceText").and_then(|v| v.as_str()) {
                    out.push_str(&format!("**原文**:{}\n\n", sanitize(source)));
                }
            }
            match segment
                .get("translatedText")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                Some(translated) => out.push_str(&format!("**译文**:{}\n\n", sanitize(translated))),
                None => out.push_str("**译文**:未完成(翻译失败;原文已保留)\n\n"),
            }
        }
    }
    if let Some(glossary) = value.get("glossary").and_then(|v| v.as_array()) {
        if !glossary.is_empty() {
            out.push_str("## 术语表\n\n");
            for entry in glossary {
                let source = entry.get("source").and_then(|v| v.as_str()).unwrap_or("");
                let target = entry.get("target").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!("- {} → {}\n", sanitize(source), sanitize(target)));
            }
        }
    }
    out
}
/// 渲染 Markdown 正文(纯函数,黄金样例测试入口)。
pub fn render_markdown(
    p: &ExportProjection,
    sources: &MarkdownSourceProjection,
) -> Result<String, AppError> {
    render_markdown_with_variant(p, sources, "bilingual")
}

pub fn render_markdown_with_variant(
    p: &ExportProjection,
    sources: &MarkdownSourceProjection,
    variant: &str,
) -> Result<String, AppError> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&p.content_json) {
        if matches!(
            value.get("schemaVersion").and_then(|v| v.as_str()),
            Some("quiz.v1") | Some("quiz.v2")
        ) {
            return Ok(render_quiz_markdown(p, &value));
        }
        if value.get("schemaVersion").and_then(|v| v.as_str()) == Some("translation.v1") {
            return Ok(render_translation_markdown(p, &value, variant));
        }
    }
    let content: SummaryContentV1 = serde_json::from_str(&p.content_json).map_err(|e| {
        err(CoreErrorCode::ArtifactSchemaInvalid)
            .with_detail("reason", format!("content parse failed: {e}").into())
    })?;
    if content.schema_version != "summary.v1" {
        return Err(err(CoreErrorCode::ArtifactSchemaInvalid)
            .with_detail("reason", "unsupported schema_version".into()));
    }
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", p.artifact_title));
    out.push_str(&format!("> 文档:{}\n", p.document_title));
    out.push_str(&format!("> 版本:第 {} 版\n", p.version_no));
    out.push_str(&format!(
        "> 生成:{}\n",
        crate::exporters::utc_date(p.created_at)
    ));
    out.push_str(&format!(
        "> 覆盖:{}\n\n",
        format_coverage(&content.coverage)
    ));

    let mut footnote_idx = 0usize;
    let mut footnotes = String::new();
    // 脚注关联:summary.v1 的 citationIds 是生成时的本地草案 ID;
    // 落库后的稳定关联是 Citation.claimKey = segment.id(《M10》§6)。
    let mut citation_by_claim: std::collections::HashMap<&str, Vec<&MarkdownCitation>> =
        std::collections::HashMap::new();
    for c in &sources.citations {
        citation_by_claim
            .entry(c.claim_key.as_str())
            .or_default()
            .push(c);
    }

    for section in &content.sections {
        out.push_str(&section_heading(&sanitize(&section.title)));
        out.push_str("\n\n");
        for seg in &section.segments {
            let label = segment_label(seg.kind);
            let mut markers: Vec<String> = Vec::new();
            if let Some(cits) = citation_by_claim.get(seg.id.as_str()) {
                for c in cits {
                    footnote_idx += 1;
                    let page = c
                        .page_number
                        .map(|p| format!("第 {p} 页"))
                        .unwrap_or_else(|| "位置未知".into());
                    let path = if c.heading_path.is_empty() {
                        String::new()
                    } else {
                        format!("“{}”", c.heading_path.join(" / "))
                    };
                    let sep = if path.is_empty() { "" } else { "," };
                    footnotes.push_str(&format!(
                        "[^{}]: 原文引文 — {page}{sep}{path}:“{}”\n",
                        footnote_idx,
                        sanitize(&c.quoted_text)
                    ));
                    markers.push(format!("[^{}]", footnote_idx));
                }
            }
            let marker_suffix = markers.join("");
            let text = sanitize(&seg.text);
            if label.is_empty() {
                out.push_str(&format!("{text}{marker_suffix}\n\n"));
            } else {
                // 非原文事实段显式标注来源,不伪装为原文(§8)。
                out.push_str(&format!("**({label})** {text}{marker_suffix}\n\n"));
            }
        }
    }

    if !content.open_questions.is_empty() {
        out.push_str("## 待澄清\n\n");
        for q in &content.open_questions {
            out.push_str(&format!("- {}\n", sanitize(q)));
        }
        out.push('\n');
    }

    if !sources.notes.is_empty() {
        out.push_str("## 用户笔记(来源)\n\n");
        for n in &sources.notes {
            let escaped = sanitize(&n.text).replace('\n', " ");
            out.push_str(&format!("- {escaped}\n"));
        }
        out.push('\n');
    }

    if footnote_idx > 0 {
        out.push_str("## 引用\n\n");
        out.push_str(&footnotes);
    }
    // 末尾恰好一个换行(确定性)。
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// 默认文件名:标题 + 版本号(§8;用户仍显式选择目标)。
pub fn default_filename(title: &str, version_no: i64) -> String {
    // Windows 保留字符替换;确定性。
    let sanitized: String = title
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ => c,
        })
        .collect();
    let trimmed = sanitized.trim().trim_matches('-');
    let base = if trimmed.is_empty() {
        "摘要"
    } else {
        trimmed
    };
    format!("{base}-v{version_no}.md")
}

/// 写入目标:已存在 ⇒ EXPORT_TARGET_EXISTS,绝不静默覆盖(§8)。
/// 权限/磁盘满分别返回明确错误(§10)。导出失败不修改 ArtifactVersion。
/// 落盘语义与 docx 共用 `exporters::write_export_file`,避免两套行为漂移。
pub fn write_markdown_file(
    _clock: &dyn Clock,
    path: &Path,
    markdown: &str,
) -> Result<i64, AppError> {
    crate::exporters::write_export_file(path, markdown.as_bytes())
}

/// 汇总导出入口:渲染 + 落盘 + 记录(成功与失败都写 export_record)。
pub struct MarkdownExportDeps<'a> {
    pub service: &'a crate::artifacts::service::ArtifactService,
    pub clock: std::sync::Arc<dyn Clock>,
    pub ids: std::sync::Arc<dyn crate::shared::IdGenerator>,
    pub variant: &'a str,
}

pub fn export_artifact_markdown(
    deps: &MarkdownExportDeps<'_>,
    artifact_id: &str,
    version_id: &str,
    path: &Path,
) -> Result<crate::artifacts::models::ExportResult, AppError> {
    let projection = deps.service.export_projection(artifact_id, version_id)?;
    // 引用投影:owner=artifact_version 的 Citation + 用户笔记来源(§8 预览同源)。
    let sources = deps.service.markdown_sources(version_id)?;
    let markdown = render_markdown_with_variant(&projection, &sources, deps.variant)?;
    let now = deps.clock.now_ms();
    let record_id = deps.ids.new_entity_id();
    let result = write_markdown_file(deps.clock.as_ref(), path, &markdown);
    let status = if result.is_ok() {
        "succeeded"
    } else {
        "failed"
    };
    let error_code = result.as_ref().err().map(|e| e.code.clone());
    let bytes_written = result;
    let version_id2 = version_id.to_string();
    let projection2 = projection.clone();
    let record_id2 = record_id.clone();
    deps.service.db.write(move |conn| {
        crate::artifacts::service::ArtifactService::record_export_tx(
            conn,
            &record_id2,
            &projection2.document_id,
            &projection2.artifact_id,
            &version_id2,
            "markdown",
            "file",
            status,
            error_code.as_deref(),
            now,
        )
    })?;
    let bytes_written = bytes_written?;
    Ok(crate::artifacts::models::ExportResult {
        record_id,
        artifact_version_id: version_id.to_string(),
        format: "markdown".into(),
        bytes_written,
    })
}

/// 供预览(导出前展示正文/引用/覆盖)复用的渲染入口。
pub fn preview_markdown(
    service: &crate::artifacts::service::ArtifactService,
    artifact_id: &str,
    version_id: &str,
) -> Result<String, AppError> {
    preview_markdown_with_variant(service, artifact_id, version_id, "bilingual")
}

pub fn preview_markdown_with_variant(
    service: &crate::artifacts::service::ArtifactService,
    artifact_id: &str,
    version_id: &str,
    variant: &str,
) -> Result<String, AppError> {
    let projection = service.export_projection(artifact_id, version_id)?;
    let sources = service.markdown_sources(version_id)?;
    render_markdown_with_variant(&projection, &sources, variant)
}

// 保持 SummarySection 类型引用(渲染内部使用)。
#[allow(dead_code)]
fn _use_types(_: &SummarySection) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection() -> ExportProjection {
        ExportProjection {
            artifact_id: "a1".into(),
            document_id: "d1".into(),
            document_title: "示例文档".into(),
            artifact_title: "摘要标题".into(),
            kind: "summary".into(),
            schema_version: "summary.v1".into(),
            content_json: serde_json::json!({
                "schemaVersion": "summary.v1",
                "sections": [{
                    "id": "sec1",
                    "title": "第 2 节",
                    "segments": [{
                        "id": "seg1",
                        "kind": "document_fact",
                        "text": "正文段落。",
                        "citationIds": ["c1"]
                    }, {
                        "id": "seg2",
                        "kind": "ai_inference",
                        "text": "推断内容。",
                        "citationIds": []
                    }]
                }],
                "openQuestions": [],
                "coverage": { "complete": true, "regions": [
                    { "label": "第 2–4 节", "blockCount": 12 }
                ] }
            })
            .to_string(),
            coverage_json: "{}".into(),
            version_no: 3,
            created_at: 1_761_235_200_000, // 2025-10-23 UTC
        }
    }

    #[test]
    fn golden_markdown_shape() {
        let p = projection();
        let sources = MarkdownSourceProjection {
            citations: vec![MarkdownCitation {
                citation_id: "c1".into(),
                claim_key: "seg1".into(),
                page_number: Some(12),
                heading_path: vec!["标题路径".into()],
                quoted_text: "引文".into(),
            }],
            notes: vec![MarkdownNoteSource {
                note_id: "n1".into(),
                text: "我的笔记".into(),
            }],
        };
        let md = render_markdown(&p, &sources).unwrap();
        let expected = "# 摘要标题\n\n> 文档:示例文档\n> 版本:第 3 版\n> 生成:2025-10-23\n> 覆盖:第 2–4 节(完整)\n\n## 第 2 节\n\n正文段落。[^1]\n\n**(AI 推断)** 推断内容。\n\n## 用户笔记(来源)\n\n- 我的笔记\n\n## 引用\n\n[^1]: 原文引文 — 第 12 页,“标题路径”:“引文”\n";
        assert_eq!(md, expected);
    }

    #[test]
    fn partial_coverage_not_claimed_complete() {
        let mut p = projection();
        p.content_json = p
            .content_json
            .replace("\"complete\":true", "\"complete\":false");
        let md = render_markdown(&p, &MarkdownSourceProjection::default()).unwrap();
        assert!(md.contains("有缺失"), "partial 不得宣称完整:{}", md);
        assert!(!md.contains("(完整)"));
    }

    #[test]
    fn deterministic_output() {
        let p = projection();
        let sources = MarkdownSourceProjection::default();
        assert_eq!(
            render_markdown(&p, &sources).unwrap(),
            render_markdown(&p, &sources).unwrap()
        );
    }

    #[test]
    fn default_filename_sanitized() {
        assert_eq!(default_filename("摘要/标题: v1", 2), "摘要-标题- v1-v2.md");
    }

    #[test]
    fn refuse_overwrite_existing_file() {
        let dir = std::env::temp_dir().join(format!("kw-m10-export-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("out.md");
        std::fs::write(&target, "old").unwrap();
        let clock = crate::shared::clock::TestClock::new(0);
        let e = write_markdown_file(&clock, &target, "# new").unwrap_err();
        assert_eq!(e.code, "EXPORT_TARGET_EXISTS");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod quiz_export_tests {
    use super::*;
    use crate::artifacts::service::ExportProjection;

    #[test]
    fn quiz_v2_exports_questions_answers_and_citations_without_raw_json() {
        let content = serde_json::json!({
            "schemaVersion": "quiz.v2",
            "questions": [{
                "id": "q1",
                "type": "fill_blank",
                "prompt": "快照是否可修改?",
                "options": [],
                "blanks": [{"id": "b1", "acceptedAnswers": ["不可变"], "caseSensitive": false}],
                "answer": {"correctOptionIds": [], "referenceAnswer": "", "rubric": []},
                "explanationSegments": [{"id": "e1", "kind": "document_fact", "text": "快照不可变", "citationIds": ["c1"]}],
                "citations": [{"id": "c1", "blockId": "blk", "quotedText": "快照不可变", "claimKey": "explanation"}],
                "difficulty": "easy",
                "validity": "valid"
            }]
        });
        let p = ExportProjection {
            artifact_id: "a".into(),
            document_id: "d".into(),
            document_title: "文档".into(),
            artifact_title: "练习题".into(),
            kind: "quiz".into(),
            schema_version: "quiz.v2".into(),
            content_json: content.to_string(),
            coverage_json: "{}".into(),
            version_no: 1,
            created_at: 0,
        };
        let sources = MarkdownSourceProjection {
            citations: Vec::new(),
            notes: Vec::new(),
        };
        let markdown = render_markdown(&p, &sources).unwrap();
        assert!(markdown.contains("第 1 题 · 填空"));
        assert!(markdown.contains("快照不可变"));
        assert!(markdown.contains("引文") || markdown.contains("「快照不可变」"));
        assert!(!markdown.contains("\"schemaVersion\""));
    }
}

#[cfg(test)]
mod translation_export_tests {
    use super::*;
    use crate::artifacts::service::ExportProjection;

    fn projection() -> ExportProjection {
        ExportProjection {
            artifact_id: "a".into(),
            document_id: "d".into(),
            document_title: "文档".into(),
            artifact_title: "译文".into(),
            kind: "translation".into(),
            schema_version: "translation.v1".into(),
            content_json: serde_json::json!({
                "schemaVersion": "translation.v1",
                "sourceLanguage": "en",
                "targetLanguage": "zh-CN",
                "style": "faithful",
                "segments": [{
                    "id": "t1",
                    "sourceBlockId": "b1",
                    "pageNumber": 2,
                    "sourceTextHash": "h",
                    "sourceText": "Snapshot is immutable.",
                    "translatedText": "快照不可变。",
                    "status": "translated",
                    "scope": "pages",
                    "engine": "model",
                    "confidence": "high",
                    "glossaryHits": []
                }],
                "glossary": [],
                "coverage": {
                    "complete": true,
                    "totalSegments": 1,
                    "translatedSegments": 1,
                    "failedSegments": 0,
                    "pages": [2],
                    "notes": []
                }
            })
            .to_string(),
            coverage_json: "{}".into(),
            version_no: 1,
            created_at: 0,
        }
    }

    #[test]
    fn bilingual_and_translation_only_variants_differ() {
        let p = projection();
        let sources = MarkdownSourceProjection {
            citations: Vec::new(),
            notes: Vec::new(),
        };
        let bilingual = render_markdown_with_variant(&p, &sources, "bilingual").unwrap();
        let only = render_markdown_with_variant(&p, &sources, "translation_only").unwrap();
        assert!(bilingual.contains("Snapshot is immutable."));
        assert!(bilingual.contains("快照不可变。"));
        assert!(!only.contains("Snapshot is immutable."));
        assert!(only.contains("快照不可变。"));
    }
}

#[cfg(test)]
mod sanitize_export_tests {
    use super::*;
    use crate::artifacts::models::{SummaryContentV1, SummarySection, SummarySegmentKind};

    fn projection(content: serde_json::Value) -> ExportProjection {
        ExportProjection {
            artifact_id: "a1".into(),
            document_id: "d1".into(),
            document_title: "文档".into(),
            artifact_title: "摘要".into(),
            kind: "summary".into(),
            schema_version: "summary.v1".into(),
            content_json: content.to_string(),
            coverage_json: "{}".into(),
            version_no: 1,
            created_at: 1_700_000_000_000,
        }
    }

    /// 导出文件里不得出现内部标记:SOURCE 标记、字面换行转义、HTML 实体。
    #[test]
    fn exported_markdown_never_contains_internal_markers() {
        let content = SummaryContentV1 {
            schema_version: "summary.v1".into(),
            sections: vec![SummarySection {
                id: "s1".into(),
                title: "小节[SOURCE blk_1]".into(),
                segments: vec![crate::artifacts::models::SummarySegment {
                    id: "seg1".into(),
                    kind: SummarySegmentKind::NeedsVerification,
                    text: "第一行[SOURCE blk_1]\\n第二行 &amp; 结尾".into(),
                    citation_ids: vec![],
                }],
            }],
            open_questions: vec!["待确认 &quot;范围&quot;".into()],
            coverage: crate::artifacts::models::CoverageReport {
                complete: true,
                regions: Vec::new(),
            },
        };
        let json = serde_json::to_value(&content).unwrap();
        let out = render_markdown(&projection(json), &MarkdownSourceProjection::default()).unwrap();
        assert!(!out.contains("[SOURCE"), "导出文件不得含内部来源标记");
        assert!(!out.contains("\\n"), "字面换行必须还原");
        assert!(
            !out.contains("&amp;") && !out.contains("&quot;"),
            "HTML 实体必须解码"
        );
        assert!(out.contains("第一行"));
        assert!(out.contains("第二行 & 结尾"));
    }
}
