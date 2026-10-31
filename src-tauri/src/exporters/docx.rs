//! WBS-4C 摘要 docx 导出器。
//!
//! 与 Markdown 导出器同源(同一 `ExportProjection` + 同一来源投影),保证
//! “覆盖真相、来源分层、引用位置”在两种格式中语义一致:
//! - 每个段落都带可见来源标签(原文事实 / AI 推断 / 用户观点 / 待核实),
//!   非原文事实不得伪装成原文;
//! - 覆盖不完整时明确写“部分覆盖，有缺失”,不宣称全文;
//! - 引用以 `[n]` 标注,并在“来源附录”列出原文引文、页码/章节;
//! - 不写本机绝对路径、密钥或导出时刻;时间只取版本创建时间。
//!
//! 确定性:ZIP 条目顺序由 docx-rs 固定,条目时间戳取 zip 默认值
//! (Cargo.toml 中 `zip` 关闭 `time` feature),因此同一版本重复导出字节一致。

use crate::artifacts::models::{
    CoverageReport, ExportResult, SummaryContentV1, SummarySegment, SummarySegmentKind,
};
use crate::artifacts::service::{ArtifactService, ExportProjection};
use crate::exporters::markdown::{MarkdownCitation, MarkdownSourceProjection};
use crate::shared::{new_trace_id, AppError, Clock, CoreErrorCode, IdGenerator};
use docx_rs::*;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, new_trace_id())
}

/// 段落来源标签:四种类型全部显式可见(比 Markdown 更严格:
/// document_fact 也标注,避免读者把摘要段落误当作原文引用)。
fn segment_label(kind: SummarySegmentKind) -> &'static str {
    match kind {
        SummarySegmentKind::DocumentFact => "原文事实",
        SummarySegmentKind::AiInference => "AI 推断",
        SummarySegmentKind::UserView => "用户观点",
        SummarySegmentKind::NeedsVerification => "待核实",
    }
}

/// XML 1.0 不允许控制字符;用户文本中的控制字符替换为空格,不丢段落。
fn clean_inline(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn text_lines(text: &str) -> Vec<String> {
    text.trim_end_matches(['\r', '\n'])
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(clean_inline)
        .collect()
}

/// 覆盖行:complete=false 时必须出现“部分覆盖，有缺失”,不得宣称全文。
fn coverage_text(coverage: &CoverageReport) -> String {
    let labels: Vec<String> = coverage
        .regions
        .iter()
        .map(|r| clean_inline(&r.label))
        .collect();
    let joined = labels.join("、");
    if coverage.complete {
        if joined.is_empty() {
            "全文（完整）".into()
        } else {
            format!("{joined}（完整）")
        }
    } else if joined.is_empty() {
        "部分覆盖，有缺失".into()
    } else {
        format!("{joined}（部分覆盖，有缺失）")
    }
}

fn docx_styles() -> Styles {
    // 默认样式只有 Normal/Title;补 Heading1(带 outlineLvl,Word 大纲可识别)。
    Styles::new().add_style(
        Style::new("Heading1", StyleType::Paragraph)
            .name("heading 1")
            .based_on("Normal")
            .next("Normal")
            .q_format(true)
            .size(28)
            .bold()
            .outline_lvl(0),
    )
}

fn heading_paragraph(title: &str) -> Paragraph {
    Paragraph::new()
        .style("Heading1")
        .add_run(Run::new().add_text(clean_inline(title)))
}

fn meta_paragraph(label: &str, value: &str) -> Paragraph {
    Paragraph::new().add_run(
        Run::new()
            .size(18)
            .color("666666")
            .add_text(format!("{label}：{}", clean_inline(value))),
    )
}

fn bullet_paragraph(text: &str) -> Paragraph {
    Paragraph::new().add_run(Run::new().add_text(format!("• {}", clean_inline(text))))
}

/// docx-rs 的段落 ID(`w14:paraId`)来自进程内递增计数器,默认不随文档重置。
/// 为了让同一版本重复导出字节一致,这里为每个段落显式分配文档内确定性 ID。
struct DocxBuilder {
    paragraphs: Vec<Paragraph>,
    next_para_id: u32,
}

impl DocxBuilder {
    fn new() -> Self {
        Self {
            paragraphs: Vec::new(),
            next_para_id: 1,
        }
    }

    fn add_paragraph(&mut self, para: Paragraph) {
        let id = self.next_para_id;
        self.next_para_id = self
            .next_para_id
            .checked_add(1)
            .expect("paragraph id space exhausted");
        self.paragraphs.push(para.id(format!("{id:08x}")));
    }

    fn into_docx(self, mut docx: Docx) -> Docx {
        for para in self.paragraphs {
            docx = docx.add_paragraph(para);
        }
        docx
    }

    /// 单段正文:标签 + 首行,后续行另起段落;引用标记追加在最后一行。
    fn add_segment(&mut self, seg: &SummarySegment, markers: &[usize]) {
        let lines = text_lines(&seg.text);
        for (idx, line) in lines.iter().enumerate() {
            let mut para = Paragraph::new();
            if idx == 0 {
                para = para.add_run(
                    Run::new()
                        .bold()
                        .add_text(format!("【{}】", segment_label(seg.kind))),
                );
            }
            if !line.is_empty() {
                para = para.add_run(Run::new().add_text(line.clone()));
            }
            if idx + 1 == lines.len() && !markers.is_empty() {
                let suffix: String = markers.iter().map(|n| format!("[{n}]")).collect();
                para = para.add_run(Run::new().add_text(suffix));
            }
            self.add_paragraph(para);
        }
    }
}

/// 来源附录条目:引用原文 + 页码/章节(与 Markdown 脚注同义)。
fn citation_line(no: usize, c: &MarkdownCitation) -> String {
    let page = c
        .page_number
        .map(|p| format!("第 {p} 页"))
        .unwrap_or_else(|| "位置未知".into());
    let path = if c.heading_path.is_empty() {
        String::new()
    } else {
        let joined = c
            .heading_path
            .iter()
            .map(|s| clean_inline(s))
            .collect::<Vec<_>>()
            .join(" / ");
        format!("，{joined}")
    };
    let quote = clean_inline(&c.quoted_text.replace(['\r', '\n'], " "));
    format!("[{no}] 原文引文 — {page}{path}：“{quote}”")
}

fn build_docx(
    p: &ExportProjection,
    content: &SummaryContentV1,
    sources: &MarkdownSourceProjection,
) -> Docx {
    let date = crate::exporters::utc_date(p.created_at);
    let docx = Docx::new()
        .styles(docx_styles())
        .default_fonts(RunFonts::new().ascii("Calibri").east_asia("宋体"))
        .created_at(&date)
        .updated_at(&date);
    let mut b = DocxBuilder::new();

    b.add_paragraph(
        Paragraph::new().style("Title").add_run(
            Run::new()
                .size(40)
                .bold()
                .add_text(clean_inline(&p.artifact_title)),
        ),
    );
    b.add_paragraph(meta_paragraph("文档", &p.document_title));
    b.add_paragraph(meta_paragraph("版本", &format!("第 {} 版", p.version_no)));
    b.add_paragraph(meta_paragraph("生成", &date));
    b.add_paragraph(meta_paragraph("覆盖", &coverage_text(&content.coverage)));
    b.add_paragraph(Paragraph::new());

    // 落库后的稳定关联是 Citation.claimKey = segment.id;不是生成时的草案 ID。
    let mut citation_by_claim: HashMap<&str, Vec<&MarkdownCitation>> = HashMap::new();
    for c in &sources.citations {
        citation_by_claim
            .entry(c.claim_key.as_str())
            .or_default()
            .push(c);
    }
    let mut appendix: Vec<(usize, &MarkdownCitation)> = Vec::new();
    let mut next_citation_no = 0usize;

    for section in &content.sections {
        b.add_paragraph(heading_paragraph(&section.title));
        for seg in &section.segments {
            let mut markers = Vec::new();
            if let Some(cits) = citation_by_claim.get(seg.id.as_str()) {
                for c in cits {
                    next_citation_no += 1;
                    markers.push(next_citation_no);
                    appendix.push((next_citation_no, c));
                }
            }
            b.add_segment(seg, &markers);
        }
    }

    if !content.open_questions.is_empty() {
        b.add_paragraph(heading_paragraph("待澄清"));
        for q in &content.open_questions {
            b.add_paragraph(bullet_paragraph(q));
        }
    }

    if !sources.notes.is_empty() {
        b.add_paragraph(heading_paragraph("用户笔记（来源）"));
        for n in &sources.notes {
            let text = clean_inline(&n.text.replace(['\r', '\n'], " "));
            b.add_paragraph(bullet_paragraph(&text));
        }
    }

    if !appendix.is_empty() {
        b.add_paragraph(heading_paragraph("来源附录"));
        for (no, c) in appendix {
            b.add_paragraph(Paragraph::new().add_run(Run::new().add_text(citation_line(no, c))));
        }
    }

    b.into_docx(docx)
}

/// quiz Markdown/docx:题目、选项、答案、解析与引用;不写路径/密钥。
fn render_quiz_docx(p: &ExportProjection, value: &serde_json::Value) -> Result<Vec<u8>, AppError> {
    let mut docx = Docx::new()
        .add_paragraph(
            Paragraph::new().add_run(
                Run::new()
                    .size(32)
                    .bold()
                    .add_text(clean_inline(&p.artifact_title)),
            ),
        )
        .add_paragraph(meta_paragraph("文档", &p.document_title))
        .add_paragraph(meta_paragraph("版本", &format!("第 {} 版", p.version_no)))
        .add_paragraph(meta_paragraph(
            "生成",
            &crate::exporters::utc_date(p.created_at),
        ));
    let Some(questions) = value.get("questions").and_then(|v| v.as_array()) else {
        let mut cursor = Cursor::new(Vec::new());
        docx.pack(&mut cursor).map_err(|_| {
            err(CoreErrorCode::Internal).with_detail("reason", "docx_pack_failed".into())
        })?;
        return Ok(cursor.into_inner());
    };
    for (index, question) in questions.iter().enumerate() {
        let prompt = question
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("(缺少题干)");
        docx = docx.add_paragraph(heading_paragraph(&format!("第 {} 题", index + 1)));
        docx =
            docx.add_paragraph(Paragraph::new().add_run(Run::new().add_text(clean_inline(prompt))));
        if let Some(options) = question.get("options").and_then(|v| v.as_array()) {
            for (option_index, option) in options.iter().enumerate() {
                let text = option.get("text").and_then(|v| v.as_str()).unwrap_or("");
                docx = docx.add_paragraph(bullet_paragraph(&format!(
                    "{}. {}",
                    (b'A' + option_index as u8) as char,
                    text
                )));
            }
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
                docx = docx.add_paragraph(bullet_paragraph(&format!(
                    "空 {}:____ (可接受:{accepted})",
                    blank_index + 1
                )));
            }
        }
        let answer = quiz_answer_text(question);
        if !answer.is_empty() {
            docx = docx.add_paragraph(meta_paragraph("答案", &answer));
        }
        if let Some(segments) = question
            .get("explanationSegments")
            .and_then(|v| v.as_array())
        {
            if !segments.is_empty() {
                docx = docx.add_paragraph(meta_paragraph("解析", ""));
                for segment in segments {
                    if let Some(text) = segment.get("text").and_then(|v| v.as_str()) {
                        docx = docx.add_paragraph(bullet_paragraph(text));
                    }
                }
            }
        }
        if let Some(citations) = question.get("citations").and_then(|v| v.as_array()) {
            if !citations.is_empty() {
                docx = docx.add_paragraph(meta_paragraph("引用", ""));
                for citation in citations {
                    if let Some(quote) = citation.get("quotedText").and_then(|v| v.as_str()) {
                        docx = docx.add_paragraph(bullet_paragraph(&format!("「{quote}」")));
                    }
                }
            }
        }
    }
    let mut cursor = Cursor::new(Vec::new());
    docx.pack(&mut cursor).map_err(|_| {
        err(CoreErrorCode::Internal).with_detail("reason", "docx_pack_failed".into())
    })?;
    Ok(cursor.into_inner())
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

/// translation.v1 → docx:双语或仅译文;失败段写明原因。
fn render_translation_docx(
    p: &ExportProjection,
    value: &serde_json::Value,
    variant: &str,
) -> Result<Vec<u8>, AppError> {
    let source_language = value
        .get("sourceLanguage")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let target_language = value
        .get("targetLanguage")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let mut docx = Docx::new()
        .add_paragraph(
            Paragraph::new().add_run(
                Run::new()
                    .size(32)
                    .bold()
                    .add_text(clean_inline(&p.artifact_title)),
            ),
        )
        .add_paragraph(meta_paragraph("文档", &p.document_title))
        .add_paragraph(meta_paragraph(
            "语言",
            &format!("{source_language} → {target_language}"),
        ));
    let only_translation = variant == "translation_only";
    if let Some(segments) = value.get("segments").and_then(|v| v.as_array()) {
        for segment in segments {
            let page = segment.get("pageNumber").and_then(|v| v.as_i64());
            docx = docx.add_paragraph(heading_paragraph(
                &page
                    .map(|p| format!("p.{p}"))
                    .unwrap_or_else(|| "段落".into()),
            ));
            if !only_translation {
                if let Some(source) = segment.get("sourceText").and_then(|v| v.as_str()) {
                    docx = docx.add_paragraph(meta_paragraph("原文", source));
                }
            }
            match segment
                .get("translatedText")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                Some(translated) => {
                    docx = docx.add_paragraph(
                        Paragraph::new().add_run(Run::new().add_text(clean_inline(translated))),
                    );
                }
                None => {
                    docx =
                        docx.add_paragraph(meta_paragraph("译文", "未完成(翻译失败;原文已保留)"));
                }
            }
        }
    }
    let mut cursor = Cursor::new(Vec::new());
    docx.pack(&mut cursor).map_err(|_| {
        err(CoreErrorCode::Internal).with_detail("reason", "docx_pack_failed".into())
    })?;
    Ok(cursor.into_inner())
}
/// 渲染 docx 字节(纯函数,确定性测试入口)。
pub fn render_docx(
    p: &ExportProjection,
    sources: &MarkdownSourceProjection,
) -> Result<Vec<u8>, AppError> {
    render_docx_with_variant(p, sources, "bilingual")
}

pub fn render_docx_with_variant(
    p: &ExportProjection,
    sources: &MarkdownSourceProjection,
    variant: &str,
) -> Result<Vec<u8>, AppError> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&p.content_json) {
        if matches!(
            value.get("schemaVersion").and_then(|v| v.as_str()),
            Some("quiz.v1") | Some("quiz.v2")
        ) {
            return render_quiz_docx(p, &value);
        }
        if value.get("schemaVersion").and_then(|v| v.as_str()) == Some("translation.v1") {
            return render_translation_docx(p, &value, variant);
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
    let docx = build_docx(p, &content, sources);
    let mut cursor = Cursor::new(Vec::new());
    docx.pack(&mut cursor).map_err(|_| {
        // 不把 zip 错误文本写入 IPC(可能含条目路径)。
        err(CoreErrorCode::Internal).with_detail("reason", "docx_pack_failed".into())
    })?;
    Ok(cursor.into_inner())
}

/// 汇总导出入口:渲染 + 落盘 + 记录(成功与失败都写 export_record)。
pub struct DocxExportDeps<'a> {
    pub service: &'a ArtifactService,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    pub variant: &'a str,
}

pub fn export_artifact_docx(
    deps: &DocxExportDeps<'_>,
    artifact_id: &str,
    version_id: &str,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let projection = deps.service.export_projection(artifact_id, version_id)?;
    let sources = deps.service.markdown_sources(version_id)?;
    let bytes = render_docx_with_variant(&projection, &sources, deps.variant)?;
    let now = deps.clock.now_ms();
    let record_id = deps.ids.new_entity_id();
    let result = crate::exporters::write_export_file(path, &bytes);
    let status = if result.is_ok() {
        "succeeded"
    } else {
        "failed"
    };
    let error_code = result.as_ref().err().map(|e| e.code.clone());
    let version_id2 = version_id.to_string();
    let projection2 = projection.clone();
    let record_id2 = record_id.clone();
    deps.service.db.write(move |conn| {
        ArtifactService::record_export_tx(
            conn,
            &record_id2,
            &projection2.document_id,
            &projection2.artifact_id,
            &version_id2,
            "docx",
            "file",
            status,
            error_code.as_deref(),
            now,
        )
    })?;
    let bytes_written = result?;
    Ok(ExportResult {
        record_id,
        artifact_version_id: version_id.to_string(),
        format: "docx".into(),
        bytes_written,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exporters::markdown::MarkdownNoteSource;
    use std::io::Read;

    fn projection(complete: bool) -> ExportProjection {
        let content = serde_json::json!({
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
                }, {
                    "id": "seg3",
                    "kind": "user_view",
                    "text": "用户观点内容。",
                    "citationIds": []
                }, {
                    "id": "seg4",
                    "kind": "needs_verification",
                    "text": "待核实内容。",
                    "citationIds": []
                }]
            }],
            "openQuestions": ["还需要确认附录。"],
            "coverage": {
                "complete": complete,
                "regions": [{"label": "第 2–4 节", "blockCount": 12}]
            }
        });
        ExportProjection {
            artifact_id: "a1".into(),
            document_id: "d1".into(),
            document_title: "示例文档".into(),
            artifact_title: "摘要标题".into(),
            kind: "summary".into(),
            schema_version: "summary.v1".into(),
            content_json: content.to_string(),
            coverage_json: "{}".into(),
            version_no: 3,
            created_at: 1_761_235_200_000, // 2025-10-23 UTC
        }
    }

    fn sources() -> MarkdownSourceProjection {
        MarkdownSourceProjection {
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
        }
    }

    fn read_part(bytes: &[u8], name: &str) -> String {
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("valid zip");
        let mut file = archive
            .by_name(name)
            .unwrap_or_else(|_| panic!("missing {name}"));
        let mut xml = String::new();
        file.read_to_string(&mut xml).expect("utf8 xml");
        xml
    }

    #[test]
    fn deterministic_output() {
        let p = projection(false);
        let s = sources();
        assert_eq!(render_docx(&p, &s).unwrap(), render_docx(&p, &s).unwrap());
    }

    #[test]
    fn archive_contains_expected_parts_and_version_date() {
        let bytes = render_docx(&projection(false), &sources()).unwrap();
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).expect("valid zip");
        for name in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "docProps/core.xml",
        ] {
            assert!(archive.by_name(name).is_ok(), "missing {name}");
        }
        let core = read_part(&bytes, "docProps/core.xml");
        assert!(core.contains("2025-10-23"), "版本时间缺失: {core}");
    }

    #[test]
    fn labels_are_visible_and_not_mixed_with_facts() {
        let xml = read_part(
            &render_docx(&projection(false), &sources()).unwrap(),
            "word/document.xml",
        );
        for label in ["【原文事实】", "【AI 推断】", "【用户观点】", "【待核实】"]
        {
            assert!(xml.contains(label), "缺少来源标签 {label}");
        }
        let inference = xml
            .split("</w:p>")
            .find(|p| p.contains("【AI 推断】"))
            .expect("AI 推断段落");
        assert!(inference.contains("推断内容。"));
        assert!(
            !inference.contains("正文段落。"),
            "AI 推断不得与原文事实混排: {inference}"
        );
        let fact = xml
            .split("</w:p>")
            .find(|p| p.contains("【原文事实】"))
            .expect("原文事实段落");
        assert!(fact.contains("正文段落。"));
    }

    #[test]
    fn partial_coverage_never_claims_complete() {
        let xml = read_part(
            &render_docx(&projection(false), &sources()).unwrap(),
            "word/document.xml",
        );
        assert!(
            xml.contains("部分覆盖，有缺失"),
            "partial 必须明确缺失: {xml}"
        );
        assert!(!xml.contains("全文（完整）"));
    }

    #[test]
    fn complete_coverage_is_marked_complete() {
        let xml = read_part(
            &render_docx(&projection(true), &sources()).unwrap(),
            "word/document.xml",
        );
        assert!(xml.contains("第 2–4 节（完整）"));
        assert!(!xml.contains("部分覆盖，有缺失"));
    }

    #[test]
    fn appendix_has_quote_page_and_heading() {
        let xml = read_part(
            &render_docx(&projection(false), &sources()).unwrap(),
            "word/document.xml",
        );
        assert!(xml.contains("来源附录"));
        assert!(xml.contains("[1] 原文引文"));
        assert!(xml.contains("第 12 页"));
        assert!(xml.contains("标题路径"));
        assert!(xml.contains("“引文”"));
        assert!(xml.contains("待澄清"));
        assert!(xml.contains("用户笔记（来源）"));
    }

    #[test]
    fn no_absolute_paths_or_urls_in_generated_document() {
        let xml = read_part(
            &render_docx(&projection(false), &sources()).unwrap(),
            "word/document.xml",
        );
        assert!(!xml.contains("C:\\"));
        assert!(!xml.contains("C:/"));
        assert!(!xml.contains("file://"));
    }

    #[test]
    fn control_chars_are_sanitized() {
        assert_eq!(clean_inline("a\u{1}b"), "a b");
        let mut p = projection(false);
        p.artifact_title = "标\u{1}题".into();
        let bytes = render_docx(&p, &sources()).unwrap();
        let xml = read_part(&bytes, "word/document.xml");
        assert!(xml.contains("标 题"));
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let mut p = projection(false);
        p.content_json = p.content_json.replace("summary.v1", "other.v1");
        let e = render_docx(&p, &sources()).unwrap_err();
        assert_eq!(e.code, "ARTIFACT_SCHEMA_INVALID");
    }
}
