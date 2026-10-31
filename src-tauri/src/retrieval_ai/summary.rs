//! 摘要生成编排(WBS-4B):快照材料 → 结构化生成 → summary.v1 校验。
//!
//! 复用 M10 `SummaryContentV1` 的来源分层(document_fact / ai_inference /
//! user_view / needs_verification),不新造自由格式。模型输出是一个生成
//! 信封(summary.draft.v1):除 summary.v1 的字段外,额外携带 citations
//! (blockId + quotedText + claimSegmentIds),用于在落库时写入 M05 Citation。
//!
//! 校验策略:
//! - 引用必须指向本次材料块,且引文逐字/规范化匹配原文;
//! - document_fact 段过滤后没有有效引用 ⇒ 降级为 needs_verification 并
//!   写入 openQuestions,不把无据推断伪装成原文事实;
//! - 覆盖报告由调用方(M13)根据快照覆盖与材料截断计算,模型不能自称全文覆盖。

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::artifacts::models::{
    CoverageReport, SummaryContentV1, SummarySection, SummarySegment, SummarySegmentKind,
};
use crate::parse_index::models::ContentBlockView;
use crate::shared::{AppError, CoreErrorCode};

use super::models::{GenerationInput, SourceRole, StructuredGenerationRequest, TaskKind};

pub const SUMMARY_DRAFT_SCHEMA_VERSION: &str = "summary.draft.v1";
pub const SUMMARY_INSTRUCTION_VERSION: &str = "summary-instruction.v1";
/// 单次生成最多装入的原文块数;超出时缩小覆盖并在 coverage/UI 标注。
pub const MAX_SUMMARY_BLOCKS: usize = 60;
/// 材料总字符上限(保守,避免一次请求超出模型窗口)。
pub const MAX_SUMMARY_MATERIAL_CHARS: usize = 24_000;

const FALLBACK_SECTION_TITLE: &str = "内容(格式未完全校验)";
const FALLBACK_NOTE: &str = "格式未完全校验:已保留可读正文,未标注来源";
const MAX_FALLBACK_SEGMENTS: usize = 200;
const MAX_FALLBACK_SEGMENT_CHARS: usize = 4000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummaryCitationRaw {
    id: String,
    block_id: String,
    quoted_text: String,
    #[serde(default)]
    claim_segment_ids: Vec<String>,
}

/// 待写入 M05 的引用草稿(claimKey = 段落 id,供导出脚注关联)。
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryCitationDraft {
    pub claim_key: String,
    pub claim_kind: crate::evidence::models::ClaimKind,
    pub content_block_id: String,
    pub quoted_text: String,
}

#[derive(Debug, Clone)]
pub struct SummaryGenerationOutcome {
    pub content: SummaryContentV1,
    pub citations: Vec<SummaryCitationDraft>,
    /// 降级/引用被剔除等需要让用户看见的说明。
    pub notes: Vec<String>,
}

/// 材料准备:按原文顺序取块;超出预算时停止,并把 truncated=true 交给
/// 调用方写入 coverage=false / 用户可见提示(不假装全文覆盖)。
pub fn summary_materials(
    blocks: &[ContentBlockView],
) -> Result<(Vec<ContentBlockView>, bool), AppError> {
    if blocks.is_empty() {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "summary_material_empty".into()),
        );
    }
    let mut selected = Vec::new();
    let mut chars = 0usize;
    let mut truncated = false;
    for block in blocks {
        let len = block.text.chars().count();
        if selected.len() >= MAX_SUMMARY_BLOCKS || chars + len > MAX_SUMMARY_MATERIAL_CHARS {
            truncated = true;
            break;
        }
        chars += len;
        selected.push(block.clone());
    }
    Ok((selected, truncated))
}

/// 构建摘要生成请求(Port 层输入)。
#[allow(clippy::too_many_arguments)]
pub fn build_summary_request(
    task_id: &str,
    attempt_id: &str,
    document_id: &str,
    snapshot_id: &str,
    locale: &str,
    blocks: &[ContentBlockView],
    coverage_complete: bool,
) -> StructuredGenerationRequest {
    let inputs: Vec<GenerationInput> = blocks
        .iter()
        .map(|block| {
            let title = if block.heading_path.is_empty() {
                String::new()
            } else {
                format!("[heading: {}] ", block.heading_path.join(" / "))
            };
            let page = block
                .locator
                .pdf_page()
                .map(|p| format!("[page: {p}] "))
                .unwrap_or_default();
            GenerationInput {
                source_id: block.id.clone(),
                role: SourceRole::Original,
                text: format!("{title}{page}{}", block.text),
            }
        })
        .collect();
    let coverage_note = if coverage_complete {
        "所选范围覆盖完整。"
    } else {
        "所选范围覆盖不完整:不得声称全文覆盖,无法确认的内容放入 openQuestions 或标为 needs_verification。"
    };
    let question = format!(
        "请为这份文档生成结构化摘要。要求:\n\
         - 2–6 个小节,每节 2–6 段,段落短小;\n\
         - 原文事实使用 kind=document_fact,并在 citations 中给出 blockId、逐字 quotedText、claimSegmentIds;\n\
         - 推断使用 ai_inference,用户观点使用 user_view,无法确认的使用 needs_verification;\n\
         - 不要编造数字、人名或结论;\n\
         - 未解决的问题写入 openQuestions;\n\
         - 只输出 JSON(schemaVersion={SUMMARY_DRAFT_SCHEMA_VERSION})。\n{coverage_note}"
    );
    StructuredGenerationRequest {
        task_id: task_id.to_string(),
        attempt_id: attempt_id.to_string(),
        document_id: document_id.to_string(),
        snapshot_id: snapshot_id.to_string(),
        schema_version: SUMMARY_DRAFT_SCHEMA_VERSION.to_string(),
        schema: summary_draft_json_schema(),
        instruction_version: SUMMARY_INSTRUCTION_VERSION.to_string(),
        task_kind: TaskKind::SummaryMap,
        inputs,
        locale: locale.to_string(),
        question,
        history: Vec::new(),
        history_summary: None,
        attachments: Vec::new(),
        document_map: None,
        scope: crate::model_privacy::gateway::EgressScope::Task,
    }
}

/// 2B:模板只改变章节组织与写作重点,不改变 citation/事实/覆盖契约。
#[allow(clippy::too_many_arguments)]
pub fn build_summary_request_with_template(
    task_id: &str,
    attempt_id: &str,
    document_id: &str,
    snapshot_id: &str,
    locale: &str,
    blocks: &[ContentBlockView],
    coverage_complete: bool,
    template: &str,
) -> StructuredGenerationRequest {
    let mut request = build_summary_request(
        task_id,
        attempt_id,
        document_id,
        snapshot_id,
        locale,
        blocks,
        coverage_complete,
    );
    let prefix = match template {
        "study_note" => "输出学习笔记:概念解释、关键要点、术语、复习问题(仍使用 sections 结构)。",
        "executive_summary" => "输出执行摘要:先结论,再建议、风险与依据(仍使用 sections 结构)。",
        "faq" => "输出 FAQ:每个小节用问答形式覆盖一个高频问题(仍使用 sections 结构)。",
        "comparison" => "输出对比表:按维度对比文中对象;表格放在段落文本中(仍使用 sections 结构)。",
        "timeline" => "输出时间线:按时间顺序组织事件/阶段(仍使用 sections 结构)。",
        _ => "输出中性结构化摘要。",
    };
    if template != "summary" {
        request.question = format!(
            "{prefix}
{}",
            request.question
        );
        request.instruction_version = "summary-instruction-v2".to_string();
    }
    request
}

/// 模型未按 schema 输出时的可见降级摘要(标注 needs_verification,无引用)。
///
/// 只要还能提取到可读正文就返回产物;完全无内容才返回 None。
pub fn summary_from_plain_text(
    raw: &serde_json::Value,
    coverage: CoverageReport,
) -> Option<SummaryGenerationOutcome> {
    let text = extract_text_payload(raw)?;
    fallback_outcome(&text, coverage)
}

/// 解析并校验模型输出:结构容错、引用白名单、逐字匹配、事实段降级。
///
/// 只有完全提取不到可读内容时才返回错误;字段缺失、代码围栏、别名、
/// 截断 JSON 都降级为可读文档,不把原始 JSON 展示给用户。
pub fn parse_summary_generation(
    raw: serde_json::Value,
    blocks: &[ContentBlockView],
    coverage: CoverageReport,
) -> Result<SummaryGenerationOutcome, AppError> {
    let mut parser = DraftParser::default();
    let mut sections = parser.collect_sections(&raw, 0);
    let mut citations = parser.take_inline_citations();
    citations.extend(parser.collect_top_level_citations(&raw, 0));
    let mut open_questions = parser.collect_open_questions(&raw, 0);
    dedupe_citations(&mut citations, &mut sections);

    for section in &mut sections {
        section.title = clean_text(&section.title);
        for segment in &mut section.segments {
            segment.text = clean_text(&segment.text);
        }
    }
    sections.retain(|section| !section.title.trim().is_empty() && !section.segments.is_empty());
    dedupe_section_and_segment_ids(&mut sections);

    if sections.is_empty() {
        let text = extract_text_payload(&raw).unwrap_or_default();
        return fallback_outcome(&text, coverage)
            .ok_or_else(|| invalid("summary_readable_content_missing"));
    }

    let block_map: HashMap<&str, &str> = blocks
        .iter()
        .map(|b| (b.id.as_str(), b.text.as_str()))
        .collect();
    let mut valid_citations: Vec<SummaryCitationRaw> = Vec::new();
    let mut dropped_citations = 0usize;
    for citation in citations {
        if !citation.id.trim().is_empty()
            && block_map
                .get(citation.block_id.as_str())
                .map(|text| quote_matches(text, &citation.quoted_text))
                .unwrap_or(false)
        {
            valid_citations.push(citation);
        } else {
            dropped_citations += 1;
        }
    }
    let valid_ids: HashSet<String> = valid_citations.iter().map(|c| c.id.clone()).collect();

    // 去重阶段已合并双向关联;此处只保留逐字核验通过的引用。
    let mut downgraded = 0usize;
    for section in &mut sections {
        for segment in &mut section.segments {
            segment.citation_ids.retain(|id| valid_ids.contains(id));
            if segment.kind == SummarySegmentKind::DocumentFact && segment.citation_ids.is_empty() {
                segment.kind = SummarySegmentKind::NeedsVerification;
                downgraded += 1;
            }
        }
    }

    let mut notes = Vec::new();
    if parser.derived_from_text {
        notes.push(FALLBACK_NOTE.to_string());
        if !open_questions
            .iter()
            .any(|question| question == FALLBACK_NOTE)
        {
            open_questions.push(FALLBACK_NOTE.to_string());
        }
    }
    if dropped_citations > 0 {
        notes.push(format!("剔除 {dropped_citations} 条无法核验的引用"));
    }
    if downgraded > 0 {
        notes.push(format!(
            "{downgraded} 个原文事实段缺少可核验引用,已标为待核实"
        ));
        open_questions.push(format!(
            "有 {downgraded} 个段落缺少可核验的原文引用,已标为待核实"
        ));
    }

    // 引用草稿:一条 Citation 可支持多个段落;按段落(claimKey)逐条保存,
    // 以便 Markdown 导出的脚注按段落关联。
    let mut segment_kinds: HashMap<&str, SummarySegmentKind> = HashMap::new();
    for section in &sections {
        for segment in &section.segments {
            segment_kinds.insert(segment.id.as_str(), segment.kind);
        }
    }
    let mut citation_drafts = Vec::new();
    for citation in &valid_citations {
        // 使用最终段落 id 和合并后的关联,避免旧 claim id 或单向声明丢失草稿。
        let claims = sections
            .iter()
            .flat_map(|s| s.segments.iter())
            .filter(|s| s.citation_ids.contains(&citation.id))
            .map(|s| s.id.clone());
        for claim in claims {
            let claim_kind = match segment_kinds.get(claim.as_str()) {
                Some(SummarySegmentKind::DocumentFact) => {
                    crate::evidence::models::ClaimKind::DocumentFact
                }
                _ => crate::evidence::models::ClaimKind::Inference,
            };
            citation_drafts.push(SummaryCitationDraft {
                claim_key: claim,
                claim_kind,
                content_block_id: citation.block_id.clone(),
                quoted_text: citation.quoted_text.clone(),
            });
        }
    }

    let content = SummaryContentV1 {
        schema_version: "summary.v1".into(),
        sections,
        open_questions,
        coverage: coverage.clone(),
    };
    if crate::artifacts::models::validate_summary_v1(
        &serde_json::to_value(&content)
            .map_err(|e| invalid(format!("summary_encode_failed: {e}")))?,
    )
    .is_err()
    {
        // 结构校验失败不整轮丢弃:把已整理内容降级为待核实正文。
        let readable = content
            .sections
            .iter()
            .flat_map(|section| section.segments.iter())
            .map(|segment| segment.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        if let Some(fallback) = fallback_outcome(&readable, coverage) {
            return Ok(fallback);
        }
        return Err(invalid("summary_invalid"));
    }
    Ok(SummaryGenerationOutcome {
        content,
        citations: citation_drafts,
        notes,
    })
}

/// 纯文本兜底:把可读 Markdown 整理为 summary.v1 的待核实段。
pub fn sections_from_plain_text(text: &str) -> Vec<SummarySection> {
    sections_from_markdown_with_title(text, FALLBACK_SECTION_TITLE)
}

fn fallback_outcome(text: &str, coverage: CoverageReport) -> Option<SummaryGenerationOutcome> {
    let sections = sections_from_plain_text(text);
    if sections.is_empty() {
        return None;
    }
    Some(SummaryGenerationOutcome {
        content: SummaryContentV1 {
            schema_version: "summary.v1".into(),
            sections,
            open_questions: vec![FALLBACK_NOTE.to_string()],
            coverage,
        },
        citations: Vec::new(),
        notes: vec![FALLBACK_NOTE.to_string()],
    })
}

fn extract_text_payload(raw: &serde_json::Value) -> Option<String> {
    extract_text_payload_inner(raw, 0)
}

fn extract_text_payload_inner(value: &serde_json::Value, depth: usize) -> Option<String> {
    if depth > 4 {
        return None;
    }
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Some(parsed) = crate::shared::sanitize::parse_json_like(trimmed) {
                if parsed.is_object() || parsed.is_array() {
                    if let Some(nested) = extract_text_payload_inner(&parsed, depth + 1) {
                        return Some(nested);
                    }
                }
            }
            Some(trimmed.to_string())
        }
        serde_json::Value::Object(map) => {
            if let Some(text) = string_in(
                map,
                &[
                    "text",
                    "markdown",
                    "content",
                    "summary",
                    "body",
                    "answer",
                    "draft",
                    "conclusion",
                    "message",
                ],
            ) {
                return Some(text);
            }
            for key in [
                "data", "result", "output", "payload", "response", "artifact",
            ] {
                if let Some(nested) = obj_get(map, &[key]) {
                    if let Some(text) = extract_text_payload_inner(nested, depth + 1) {
                        return Some(text);
                    }
                }
            }
            None
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(|item| extract_text_payload_inner(item, depth + 1))
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n\n"))
            }
        }
        _ => None,
    }
}

/// 结构容错解析器:字段别名、嵌套信封、缺失 id、内联引用。
#[derive(Default)]
struct DraftParser {
    citation_seq: usize,
    inline_citations: Vec<SummaryCitationRaw>,
    derived_from_text: bool,
}

impl DraftParser {
    fn next_citation_id(&mut self) -> String {
        loop {
            self.citation_seq += 1;
            let id = format!("c{}", self.citation_seq);
            if !self
                .inline_citations
                .iter()
                .any(|citation| citation.id == id)
            {
                return id;
            }
        }
    }

    fn take_inline_citations(&mut self) -> Vec<SummaryCitationRaw> {
        std::mem::take(&mut self.inline_citations)
    }

    fn collect_sections(&mut self, value: &serde_json::Value, depth: usize) -> Vec<SummarySection> {
        if depth > 4 {
            return Vec::new();
        }
        match value {
            serde_json::Value::Object(map) => {
                if let Some(items) = obj_get(map, &["sections", "section", "chapters", "parts"])
                    .and_then(|value| value.as_array())
                {
                    let sections = self.parse_sections_array(items);
                    if !sections.is_empty() {
                        return sections;
                    }
                }
                if let Some(text) =
                    string_in(map, &["markdown", "content", "text", "summary", "body"])
                {
                    if let Some(parsed) = crate::shared::sanitize::parse_json_like(&text) {
                        if parsed.is_object() || parsed.is_array() {
                            let sections = self.collect_sections(&parsed, depth + 1);
                            if !sections.is_empty() {
                                return sections;
                            }
                        }
                    }
                    self.derived_from_text = true;
                    let sections = sections_from_markdown_with_title(&text, FALLBACK_SECTION_TITLE);
                    if !sections.is_empty() {
                        return sections;
                    }
                }
                for key in [
                    "data", "result", "output", "draft", "payload", "response", "artifact",
                    "summary", "content",
                ] {
                    if let Some(nested) = obj_get(map, &[key]) {
                        if nested.is_object() || nested.is_array() {
                            let sections = self.collect_sections(nested, depth + 1);
                            if !sections.is_empty() {
                                return sections;
                            }
                        }
                    }
                }
                Vec::new()
            }
            serde_json::Value::Array(items) => self.parse_sections_array(items),
            serde_json::Value::String(text) => {
                if let Some(parsed) = crate::shared::sanitize::parse_json_like(text) {
                    if parsed.is_object() || parsed.is_array() {
                        let sections = self.collect_sections(&parsed, depth + 1);
                        if !sections.is_empty() {
                            return sections;
                        }
                    }
                }
                self.derived_from_text = true;
                sections_from_markdown_with_title(text, FALLBACK_SECTION_TITLE)
            }
            _ => Vec::new(),
        }
    }

    fn parse_sections_array(&mut self, items: &[serde_json::Value]) -> Vec<SummarySection> {
        let mut sections = Vec::new();
        for (index, item) in items.iter().enumerate() {
            if sections.len() >= 50 {
                break;
            }
            if let Some(section) = self.parse_section(item, index) {
                sections.push(section);
            }
        }
        sections
    }

    fn parse_section(&mut self, value: &serde_json::Value, index: usize) -> Option<SummarySection> {
        let fallback_id = format!("s{}", index + 1);
        let mut id = fallback_id.clone();
        let mut title;
        let mut segments = Vec::new();
        match value {
            serde_json::Value::String(text) => {
                title = first_line(text, 40);
                if let Some(segment) = self.parse_segment_text(text, &id, 1) {
                    segments.push(segment);
                }
            }
            serde_json::Value::Object(map) => {
                if let Some(raw_id) = string_in(map, &["id", "sectionId", "key", "slug"]) {
                    id = sanitize_id(&raw_id, &fallback_id);
                }
                title = string_in(map, &["title", "heading", "name", "label"]).unwrap_or_default();
                if let Some(items) = obj_get(
                    map,
                    &[
                        "segments",
                        "paragraphs",
                        "points",
                        "items",
                        "claims",
                        "blocks",
                        "lines",
                    ],
                )
                .and_then(|value| value.as_array())
                {
                    for (segment_index, item) in items.iter().enumerate() {
                        if segments.len() >= MAX_FALLBACK_SEGMENTS {
                            break;
                        }
                        if let Some(segment) = self.parse_segment(item, &id, segment_index + 1) {
                            segments.push(segment);
                        }
                    }
                }
                if segments.is_empty() {
                    if let Some(text) =
                        string_in(map, &["text", "content", "markdown", "body", "paragraph"])
                    {
                        if let Some(segment) = self.parse_segment_text(&text, &id, 1) {
                            segments.push(segment);
                        }
                    }
                }
                if title.trim().is_empty() {
                    title = segments
                        .first()
                        .map(|segment| first_line(&segment.text, 40))
                        .unwrap_or_default();
                }
            }
            _ => return None,
        }
        if segments.is_empty() {
            return None;
        }
        let title = clean_text(&title);
        let title = if title.is_empty() {
            format!("小节 {}", index + 1)
        } else {
            title
        };
        Some(SummarySection {
            id,
            title,
            segments,
        })
    }

    fn parse_segment_text(
        &self,
        text: &str,
        section_id: &str,
        index: usize,
    ) -> Option<SummarySegment> {
        let text = clean_text(text);
        if text.is_empty() {
            return None;
        }
        Some(SummarySegment {
            id: format!("{section_id}-p{index}"),
            kind: SummarySegmentKind::NeedsVerification,
            text,
            citation_ids: Vec::new(),
        })
    }

    fn parse_segment(
        &mut self,
        value: &serde_json::Value,
        section_id: &str,
        index: usize,
    ) -> Option<SummarySegment> {
        let fallback_id = format!("{section_id}-p{index}");
        match value {
            serde_json::Value::String(text) => self.parse_segment_text(text, section_id, index),
            serde_json::Value::Object(map) => {
                let id = string_in(map, &["id", "segmentId", "key", "segment_id"])
                    .map(|raw| sanitize_id(&raw, &fallback_id))
                    .unwrap_or_else(|| fallback_id.clone());
                let kind = parse_kind(obj_get(
                    map,
                    &["kind", "type", "segmentKind", "category", "role"],
                ));
                let text = string_in(
                    map,
                    &[
                        "text",
                        "content",
                        "markdown",
                        "paragraph",
                        "body",
                        "value",
                        "claim",
                        "point",
                        "description",
                        "sentence",
                    ],
                )
                .unwrap_or_default();
                let text = clean_text(&text);
                if text.is_empty() {
                    return None;
                }
                let mut citation_ids = Vec::new();
                if let Some(value) = obj_get(
                    map,
                    &[
                        "citationIds",
                        "citation_ids",
                        "citations",
                        "refs",
                        "sources",
                        "evidenceIds",
                        "evidence_ids",
                    ],
                ) {
                    match value {
                        serde_json::Value::String(citation_id) => {
                            let citation_id = citation_id.trim();
                            if !citation_id.is_empty() {
                                citation_ids.push(citation_id.to_string());
                            }
                        }
                        serde_json::Value::Array(items) => {
                            for item in items {
                                match item {
                                    serde_json::Value::String(citation_id) => {
                                        let citation_id = citation_id.trim();
                                        if !citation_id.is_empty() {
                                            citation_ids.push(citation_id.to_string());
                                        }
                                    }
                                    serde_json::Value::Object(citation) => {
                                        let citation_id =
                                            string_in(citation, &["id", "citationId", "key"])
                                                .unwrap_or_else(|| self.next_citation_id());
                                        if let Some(raw) = citation_from_object(
                                            citation,
                                            citation_id.clone(),
                                            Some(id.clone()),
                                        ) {
                                            self.inline_citations.push(raw);
                                            if !citation_ids.contains(&citation_id) {
                                                citation_ids.push(citation_id);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let mut seen = HashSet::new();
                citation_ids.retain(|citation_id| seen.insert(citation_id.clone()));
                Some(SummarySegment {
                    id,
                    kind,
                    text,
                    citation_ids,
                })
            }
            _ => None,
        }
    }

    fn collect_top_level_citations(
        &mut self,
        value: &serde_json::Value,
        depth: usize,
    ) -> Vec<SummaryCitationRaw> {
        if depth > 4 {
            return Vec::new();
        }
        match value {
            serde_json::Value::Object(map) => {
                if let Some(items) =
                    obj_get(map, &["citations", "references", "sources", "evidence"])
                        .and_then(|value| value.as_array())
                {
                    let mut citations = Vec::new();
                    for item in items {
                        if let serde_json::Value::Object(citation) = item {
                            let id = string_in(citation, &["id", "citationId", "key"])
                                .unwrap_or_else(|| self.next_citation_id());
                            if let Some(raw) = citation_from_object(citation, id, None) {
                                citations.push(raw);
                            }
                        }
                    }
                    if !citations.is_empty() {
                        return citations;
                    }
                }
                for key in [
                    "data", "result", "output", "draft", "payload", "response", "artifact",
                    "summary", "content",
                ] {
                    if let Some(nested) = obj_get(map, &[key]) {
                        let citations = self.collect_top_level_citations(nested, depth + 1);
                        if !citations.is_empty() {
                            return citations;
                        }
                    }
                }
                Vec::new()
            }
            serde_json::Value::String(text) => {
                if let Some(parsed) = crate::shared::sanitize::parse_json_like(text) {
                    return self.collect_top_level_citations(&parsed, depth + 1);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn collect_open_questions(&mut self, value: &serde_json::Value, depth: usize) -> Vec<String> {
        if depth > 4 {
            return Vec::new();
        }
        if let serde_json::Value::String(text) = value {
            if let Some(parsed) = crate::shared::sanitize::parse_json_like(text) {
                return self.collect_open_questions(&parsed, depth + 1);
            }
        }
        if let serde_json::Value::Object(map) = value {
            if let Some(items) = obj_get(
                map,
                &[
                    "openQuestions",
                    "open_questions",
                    "followUps",
                    "follow_ups",
                    "todos",
                    "unresolved",
                    "remainingQuestions",
                ],
            ) {
                let questions: Vec<String> = match items {
                    serde_json::Value::Array(items) => items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(str::to_string)
                        .collect(),
                    serde_json::Value::String(text) => {
                        let text = text.trim();
                        if text.is_empty() {
                            Vec::new()
                        } else {
                            vec![text.to_string()]
                        }
                    }
                    _ => Vec::new(),
                };
                if !questions.is_empty() {
                    return questions;
                }
            }
            for key in [
                "data", "result", "output", "draft", "payload", "response", "artifact", "summary",
            ] {
                if let Some(nested) = obj_get(map, &[key]) {
                    let questions = self.collect_open_questions(nested, depth + 1);
                    if !questions.is_empty() {
                        return questions;
                    }
                }
            }
        }
        Vec::new()
    }
}

fn sections_from_markdown_with_title(markdown: &str, default_title: &str) -> Vec<SummarySection> {
    let cleaned = clean_text(markdown);
    if cleaned.trim().is_empty() {
        return Vec::new();
    }
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut current_title = default_title.to_string();
    let mut paragraphs: Vec<String> = Vec::new();
    let mut paragraph = String::new();
    let mut fence: Option<(char, usize)> = None;
    let mut math_end: Option<&str> = None;

    for line in cleaned.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some((marker, length)) = fence {
            paragraph.push_str(line);
            if let Some((close_marker, close_length)) = markdown_fence(line) {
                if close_marker == marker
                    && close_length >= length
                    && trimmed[close_length..].trim().is_empty()
                {
                    fence = None;
                }
            }
            continue;
        }
        if let Some(close) = math_end {
            paragraph.push_str(line);
            if trimmed.contains(close) {
                math_end = None;
            }
            continue;
        }
        if let Some(open) = markdown_fence(line) {
            fence = Some(open);
            paragraph.push_str(line);
            continue;
        }
        for (open, close) in [("$$", "$$"), (r"\[", r"\]")] {
            if let Some(rest) = trimmed.strip_prefix(open) {
                if !rest.contains(close) {
                    math_end = Some(close);
                }
                break;
            }
        }
        if math_end.is_some() {
            paragraph.push_str(line);
            continue;
        }
        let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
        // ATX 标题需 1–6 个 # 和空白;代码中的 # 及普通 #标签不拆节。
        if line.len() - line.trim_start().len() <= 3
            && (1..=6).contains(&hashes)
            && trimmed[hashes..].starts_with(char::is_whitespace)
        {
            let title = trimmed[hashes..].trim();
            if !title.is_empty() {
                push_paragraph(&mut paragraph, &mut paragraphs);
                if !paragraphs.is_empty() {
                    groups.push((
                        std::mem::take(&mut current_title),
                        std::mem::take(&mut paragraphs),
                    ));
                }
                current_title = title.to_string();
                continue;
            }
        }
        if trimmed.is_empty() {
            push_paragraph(&mut paragraph, &mut paragraphs);
        } else {
            paragraph.push_str(line);
        }
    }
    push_paragraph(&mut paragraph, &mut paragraphs);
    if !paragraphs.is_empty() {
        groups.push((current_title, paragraphs));
    }
    if groups.is_empty() {
        let fallback = cleaned.trim();
        if fallback.is_empty() {
            return Vec::new();
        }
        groups.push((default_title.to_string(), vec![fallback.to_string()]));
    }

    let mut sections = Vec::new();
    for (index, (title, paragraphs)) in groups.into_iter().enumerate() {
        // 分段预算是软限制,不得因达到数量上限而静默丢弃正文。
        let section_id = format!("s{}", index + 1);
        let title = if title.trim().is_empty() {
            default_title.to_string()
        } else {
            clean_text(&title)
        };
        let mut segments = Vec::new();
        for paragraph in paragraphs {
            for chunk in split_markdown_paragraph(&paragraph) {
                segments.push(SummarySegment {
                    id: format!("{section_id}-p{}", segments.len() + 1),
                    kind: SummarySegmentKind::NeedsVerification,
                    text: chunk,
                    citation_ids: Vec::new(),
                });
            }
        }
        if segments.is_empty() {
            continue;
        }
        sections.push(SummarySection {
            id: section_id,
            title,
            segments,
        });
    }
    sections
}

fn push_paragraph(paragraph: &mut String, paragraphs: &mut Vec<String>) {
    let text = paragraph.trim_matches('\n');
    if !text.trim().is_empty() {
        paragraphs.push(text.to_string());
    }
    paragraph.clear();
}

fn markdown_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = trimmed.chars().take_while(|ch| *ch == marker).count();
    (length >= 3).then_some((marker, length))
}

fn split_markdown_paragraph(text: &str) -> Vec<String> {
    // 4000 字仅约束普通长段。围栏、公式、表格等完整 Markdown 单元
    // 不能硬切,否则各段独立渲染时会丢失闭合符、表头或数学语法。
    let has_markup = text.contains(['`', '$', '|', '*', '[', '_'])
        || text.contains(r"\(")
        || text.lines().any(|line| {
            markdown_fence(line).is_some()
                || line.starts_with("    ")
                || line.starts_with('\t')
                || line.trim_start().starts_with(['-', '+', '>'])
        });
    if has_markup {
        vec![text.to_string()]
    } else {
        split_chars(text, MAX_FALLBACK_SEGMENT_CHARS)
    }
}

fn split_chars(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    for ch in text.chars() {
        if count >= max {
            chunks.push(std::mem::take(&mut current));
            count = 0;
        }
        current.push(ch);
        count += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn clean_text(text: &str) -> String {
    let cleaned = crate::shared::sanitize::sanitize_generated_markdown(text);
    if looks_like_protocol_json(&cleaned) {
        let salvaged = crate::shared::sanitize::salvage_readable_markdown(&cleaned);
        if salvaged.trim().is_empty() || looks_like_protocol_json(&salvaged) {
            return String::new();
        }
        return salvaged;
    }
    cleaned
}

fn looks_like_protocol_json(text: &str) -> bool {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with("```")) {
        return false;
    }
    [
        "\"schemaVersion\"",
        "\"blockId\"",
        "\"citationIds\"",
        "\"sections\"",
        "\"segments\"",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|ch| !matches!(ch, '_' | '-' | ' '))
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn obj_get<'a>(
    map: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a serde_json::Value> {
    for key in keys {
        if let Some(value) = map.get(*key) {
            return Some(value);
        }
    }
    let wanted: Vec<String> = keys.iter().map(|key| normalize_key(key)).collect();
    map.iter()
        .find(|(key, _)| wanted.iter().any(|want| want == &normalize_key(key)))
        .map(|(_, value)| value)
}

fn string_in(map: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<String> {
    match obj_get(map, keys)? {
        serde_json::Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn citation_ids_at(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(id) => {
            let id = id.trim();
            if id.is_empty() {
                Vec::new()
            } else {
                vec![id.to_string()]
            }
        }
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::String(id) => {
                    let id = id.trim();
                    (!id.is_empty()).then(|| id.to_string())
                }
                serde_json::Value::Object(map) => {
                    string_in(map, &["id", "citationId", "key", "segmentId"])
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn citation_from_object(
    map: &serde_json::Map<String, serde_json::Value>,
    id: String,
    claim: Option<String>,
) -> Option<SummaryCitationRaw> {
    let block_id = string_in(
        map,
        &[
            "blockId",
            "block_id",
            "block",
            "sourceId",
            "source_id",
            "contentBlockId",
            "chunkId",
            "documentBlockId",
        ],
    )?;
    let quoted_text = string_in(
        map,
        &[
            "quotedText",
            "quoted_text",
            "quote",
            "quoteText",
            "excerpt",
            "snippet",
            "text",
            "evidence",
            "verbatim",
            "sourceText",
        ],
    )?;
    let mut claim_segment_ids = obj_get(
        map,
        &[
            "claimSegmentIds",
            "claim_segment_ids",
            "segmentIds",
            "segment_ids",
            "claimIds",
            "claims",
        ],
    )
    .map(citation_ids_at)
    .unwrap_or_default();
    if claim_segment_ids.is_empty() {
        if let Some(claim) = claim {
            claim_segment_ids.push(claim);
        }
    }
    Some(SummaryCitationRaw {
        id,
        block_id,
        quoted_text,
        claim_segment_ids,
    })
}

fn parse_kind(value: Option<&serde_json::Value>) -> SummarySegmentKind {
    let raw = value
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let compact: String = raw
        .chars()
        .filter(|ch| !matches!(ch, '_' | '-' | ' '))
        .collect();
    match compact.as_str() {
        "documentfact" | "fact" | "factual" | "evidence" | "citation" | "原文事实" | "原文" => {
            SummarySegmentKind::DocumentFact
        }
        "aiinference" | "inference" | "推断" | "ai推断" | "analysis" => {
            SummarySegmentKind::AiInference
        }
        "userview" | "user" | "用户观点" | "opinion" => SummarySegmentKind::UserView,
        _ => SummarySegmentKind::NeedsVerification,
    }
}

fn dedupe_citations(citations: &mut Vec<SummaryCitationRaw>, sections: &mut [SummarySection]) {
    let originals = std::mem::take(citations);
    let mut id_contents: HashMap<String, HashSet<(String, String)>> = HashMap::new();
    for citation in &originals {
        id_contents
            .entry(citation.id.trim().to_string())
            .or_default()
            .insert((citation.block_id.clone(), citation.quoted_text.clone()));
    }
    let reserved_ids: HashSet<_> = id_contents.keys().cloned().collect();
    let mut content_ids = HashMap::new();
    let mut used_ids = HashSet::new();
    let mut mapped_ids = Vec::new();
    let mut next = 1usize;
    for citation in &originals {
        let key = (citation.block_id.clone(), citation.quoted_text.clone());
        let id = content_ids.entry(key).or_insert_with(|| {
            let mut id = citation.id.trim().to_string();
            if id.is_empty() || used_ids.contains(&id) {
                loop {
                    let candidate = format!("c{next}");
                    next += 1;
                    if !used_ids.contains(&candidate) && !reserved_ids.contains(&candidate) {
                        id = candidate;
                        break;
                    }
                }
            }
            used_ids.insert(id.clone());
            let mut canonical = citation.clone();
            canonical.id = id.clone();
            // 关联先落到段落对象,此后段落重命名也不会失联。
            canonical.claim_segment_ids.clear();
            citations.push(canonical);
            id
        });
        mapped_ids.push(id.clone());
    }

    let mut segment_counts = HashMap::new();
    for segment in sections.iter().flat_map(|s| &s.segments) {
        *segment_counts.entry(segment.id.clone()).or_insert(0usize) += 1;
    }
    for segment in sections.iter_mut().flat_map(|s| &mut s.segments) {
        let mut linked = Vec::new();
        for (citation, mapped_id) in originals.iter().zip(&mapped_ids) {
            let old_id = citation.id.trim();
            let by_ref =
                !old_id.is_empty() && segment.citation_ids.iter().any(|id| id.trim() == old_id);
            let by_claim = citation
                .claim_segment_ids
                .iter()
                .any(|id| sanitize_id(id, "") == segment.id)
                && (segment_counts[&segment.id] == 1 || by_ref);
            // 冲突 id 无法单凭 citationIds 猜关联;仅明确 claim 可消歧。
            let ambiguous = id_contents[old_id].len() > 1;
            if (by_claim || (by_ref && !ambiguous)) && !linked.contains(mapped_id) {
                linked.push(mapped_id.clone());
            }
        }
        segment.citation_ids = linked;
    }
}

fn dedupe_section_and_segment_ids(sections: &mut [SummarySection]) {
    let mut section_ids = HashSet::new();
    for (index, section) in sections.iter_mut().enumerate() {
        let fallback = format!("s{}", index + 1);
        let mut id = section.id.trim().to_string();
        if id.is_empty() || !section_ids.insert(id.clone()) {
            let base = if id.is_empty() { fallback } else { id.clone() };
            let mut suffix = 2usize;
            loop {
                let candidate = format!("{base}-{suffix}");
                suffix += 1;
                if section_ids.insert(candidate.clone()) {
                    id = candidate;
                    break;
                }
            }
        }
        section.id = id;
    }
    // summary.v1 的 section/segment 共用 id 命名空间。
    let mut segment_ids = section_ids;
    for section in sections.iter_mut() {
        for (index, segment) in section.segments.iter_mut().enumerate() {
            let fallback = format!("{}-p{}", section.id, index + 1);
            let mut id = segment.id.trim().to_string();
            if id.is_empty() || !segment_ids.insert(id.clone()) {
                let base = if id.is_empty() { fallback } else { id.clone() };
                let mut suffix = 2usize;
                loop {
                    let candidate = format!("{base}-{suffix}");
                    suffix += 1;
                    if segment_ids.insert(candidate.clone()) {
                        id = candidate;
                        break;
                    }
                }
            }
            segment.id = id;
        }
    }
}

fn sanitize_id(raw: &str, fallback: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    trimmed.chars().take(80).collect()
}

fn first_line(text: &str, max_chars: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    line.chars().take(max_chars).collect()
}

/// 引文匹配:精确 → 去首尾空白 → 规范化匹配(与 M05/M11 同策略)。
fn quote_matches(block_text: &str, quoted_text: &str) -> bool {
    if quoted_text.trim().is_empty() {
        return false;
    }
    if block_text.contains(quoted_text) {
        return true;
    }
    let trimmed = quoted_text.trim();
    if block_text.contains(trimmed) {
        return true;
    }
    let hay = crate::shared::normalize_v1(block_text);
    let needle = crate::shared::normalize_v1(trimmed);
    hay.find_original_range(&needle).is_some()
}

fn invalid(reason: impl Into<String>) -> AppError {
    super::error::model_output_invalid(&reason.into())
}

/// 摘要生成信封 JSON Schema(summary.draft.v1)。
pub fn summary_draft_json_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["schemaVersion", "sections", "openQuestions", "citations"],
        "additionalProperties": false,
        "properties": {
            "schemaVersion": {"const": SUMMARY_DRAFT_SCHEMA_VERSION},
            "sections": {"type": "array", "minItems": 1, "items": {"$ref": "#/definitions/section"}},
            "openQuestions": {"type": "array", "items": {"type": "string"}},
            "citations": {"type": "array", "items": {"$ref": "#/definitions/citation"}}
        },
        "definitions": {
            "section": {
                "type": "object",
                "required": ["id", "title", "segments"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"},
                    "title": {"type": "string"},
                    "segments": {"type": "array", "minItems": 1, "items": {"$ref": "#/definitions/segment"}}
                }
            },
            "segment": {
                "type": "object",
                "required": ["id", "kind", "text", "citationIds"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"},
                    "kind": {"enum": ["document_fact", "ai_inference", "user_view", "needs_verification"]},
                    "text": {"type": "string"},
                    "citationIds": {"type": "array", "items": {"type": "string"}}
                }
            },
            "citation": {
                "type": "object",
                "required": ["id", "blockId", "quotedText", "claimSegmentIds"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"},
                    "blockId": {"type": "string"},
                    "quotedText": {"type": "string"},
                    "claimSegmentIds": {"type": "array", "items": {"type": "string"}}
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_index::models::{BlockCompleteness, BlockKind, BlockOrigin};
    use crate::shared::locator::SourceLocator;

    fn block(id: &str, text: &str) -> ContentBlockView {
        ContentBlockView {
            id: id.into(),
            document_version_id: "v1".into(),
            parse_generation_id: Some("g1".into()),
            origin: BlockOrigin::Parser,
            block_key: format!("k{id}"),
            ordinal: Some(1),
            kind: BlockKind::Paragraph,
            text: text.into(),
            heading_path: vec!["第一章".into()],
            locator: SourceLocator::Markdown {
                start_offset: 0,
                end_offset: 4,
                mdast_node_type: "paragraph".into(),
                heading_path: vec![],
                locator_version: 1,
            },
            completeness: BlockCompleteness::Complete,
            indexable: true,
            ocr_confidence: None,
        }
    }

    fn coverage() -> CoverageReport {
        CoverageReport {
            complete: true,
            regions: vec![],
        }
    }

    #[test]
    fn valid_citation_is_kept_and_mapped_to_segment_claim() {
        let blocks = vec![block("b1", "快照不可变,追问复用同一快照。")];
        let raw = serde_json::json!({
            "schemaVersion": SUMMARY_DRAFT_SCHEMA_VERSION,
            "sections": [{
                "id": "s1", "title": "要点", "segments": [{
                    "id": "seg1", "kind": "document_fact",
                    "text": "快照不可变。", "citationIds": ["c1"]
                }]
            }],
            "openQuestions": [],
            "citations": [{
                "id": "c1", "blockId": "b1",
                "quotedText": "快照不可变", "claimSegmentIds": ["seg1"]
            }]
        });
        let out = parse_summary_generation(raw, &blocks, coverage()).unwrap();
        assert_eq!(
            out.content.sections[0].segments[0].kind,
            SummarySegmentKind::DocumentFact
        );
        assert_eq!(out.citations.len(), 1);
        assert_eq!(out.citations[0].claim_key, "seg1");
        assert!(out.notes.is_empty());
    }

    #[test]
    fn bad_quote_downgrades_fact_instead_of_claiming_it() {
        let blocks = vec![block("b1", "快照不可变,追问复用同一快照。")];
        let raw = serde_json::json!({
            "schemaVersion": SUMMARY_DRAFT_SCHEMA_VERSION,
            "sections": [{
                "id": "s1", "title": "要点", "segments": [{
                    "id": "seg1", "kind": "document_fact",
                    "text": "原文没有这句话。", "citationIds": ["c1"]
                }]
            }],
            "openQuestions": [],
            "citations": [{
                "id": "c1", "blockId": "b1",
                "quotedText": "原文没有这句话", "claimSegmentIds": ["seg1"]
            }]
        });
        let out = parse_summary_generation(raw, &blocks, coverage()).unwrap();
        assert_eq!(
            out.content.sections[0].segments[0].kind,
            SummarySegmentKind::NeedsVerification
        );
        assert!(out.content.sections[0].segments[0].citation_ids.is_empty());
        assert_eq!(out.content.sections[0].segments[0].text, "原文没有这句话。");
        assert!(!out.notes.is_empty());
        assert!(!out.content.open_questions.is_empty());
        assert!(out.citations.is_empty());
        assert!(crate::artifacts::models::validate_summary_v1(
            &serde_json::to_value(&out.content).unwrap()
        )
        .is_ok());
    }

    fn cited_raw() -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": SUMMARY_DRAFT_SCHEMA_VERSION,
            "sections": [{"id": "s1", "title": "要点", "segments": [{
                "id": "p1", "kind": "document_fact", "text": "快照不可变。",
                "citationIds": ["c1"]
            }]}],
            "openQuestions": ["如何更新快照？"],
            "citations": [{"id": "c1", "blockId": "b1", "quotedText": "快照不可变",
                "claimSegmentIds": ["p1"]}]
        })
    }

    fn assert_valid(out: &SummaryGenerationOutcome) {
        crate::artifacts::models::validate_summary_v1(&serde_json::to_value(&out.content).unwrap())
            .unwrap();
    }

    fn assert_uncited_fallback(out: &SummaryGenerationOutcome) {
        assert_valid(out);
        assert!(out.citations.is_empty());
        assert!(out
            .content
            .sections
            .iter()
            .flat_map(|s| &s.segments)
            .all(|s| {
                s.kind == SummarySegmentKind::NeedsVerification && s.citation_ids.is_empty()
            }));
        // 提示必须跟随持久化内容,不能仅存在任务 notes 中。
        let saved = serde_json::to_value(&out.content).unwrap();
        assert!(saved["openQuestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|q| q.as_str().unwrap().contains("格式未完全校验")));
    }

    #[test]
    fn fenced_json_ignores_unknown_fields_and_keeps_questions_and_citations() {
        let mut raw = cited_raw();
        raw["unknown"] = serde_json::json!({"blockId": "not-a-source"});
        raw["sections"][0]["segments"][0]["extra"] = serde_json::json!(true);
        let out = parse_summary_generation(
            serde_json::json!(format!("```json\n{raw}\n```")),
            &[block("b1", "快照不可变。")],
            coverage(),
        )
        .unwrap();
        assert_valid(&out);
        assert_eq!(out.content.open_questions, ["如何更新快照？"]);
        assert_eq!(
            out.citations,
            vec![SummaryCitationDraft {
                claim_key: "p1".into(),
                claim_kind: crate::evidence::models::ClaimKind::DocumentFact,
                content_block_id: "b1".into(),
                quoted_text: "快照不可变".into(),
            }]
        );
        assert!(out.notes.is_empty());
    }

    #[test]
    fn missing_optional_fields_and_ids_still_produce_valid_content() {
        for missing in [
            "openQuestions",
            "citations",
            "segment.id",
            "section.id",
            "all",
        ] {
            let mut raw = cited_raw();
            if missing == "all" || missing == "openQuestions" {
                raw.as_object_mut().unwrap().remove("openQuestions");
            }
            if missing == "all" || missing == "citations" {
                raw.as_object_mut().unwrap().remove("citations");
            }
            if missing == "all" || missing == "segment.id" {
                raw["sections"][0]["segments"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("id");
            }
            if missing == "all" || missing == "section.id" {
                raw["sections"][0].as_object_mut().unwrap().remove("id");
            }
            let out =
                parse_summary_generation(raw, &[block("b1", "快照不可变。")], coverage()).unwrap();
            assert_valid(&out);
            let segment = &out.content.sections[0].segments[0];
            assert_eq!(segment.text, "快照不可变。", "{missing}");
            if missing == "citations" || missing == "all" {
                assert_eq!(segment.kind, SummarySegmentKind::NeedsVerification);
                assert!(out.citations.is_empty());
            } else {
                assert_eq!(segment.kind, SummarySegmentKind::DocumentFact);
                assert_eq!(out.citations.len(), 1);
                assert_eq!(out.citations[0].claim_key, segment.id);
            }
        }
    }

    #[test]
    fn aliases_and_inline_citations_keep_source_and_claim_association() {
        let raw = serde_json::json!({"result": {
            "chapters": [{"key": "chapter", "heading": "要点", "paragraphs": [{
                "segment_id": "claim", "type": "fact", "body": "快照不可变。",
                "sources": [{"source_id": "b1", "excerpt": "快照不可变"}]
            }, {"content": "这是补充说明,并非原文事实。"}]}],
            "follow_ups": "还有哪些限制？"
        }});
        let out =
            parse_summary_generation(raw, &[block("b1", "快照不可变。")], coverage()).unwrap();
        assert_valid(&out);
        assert_eq!(out.content.sections[0].title, "要点");
        assert_eq!(
            out.content.sections[0].segments[0].kind,
            SummarySegmentKind::DocumentFact
        );
        assert_eq!(
            out.content.sections[0].segments[1].kind,
            SummarySegmentKind::NeedsVerification
        );
        assert!(out.content.sections[0].segments[1].citation_ids.is_empty());
        assert_eq!(out.citations.len(), 1);
        assert_eq!(out.citations[0].claim_key, "claim");
        assert_eq!(out.citations[0].content_block_id, "b1");
        assert_eq!(out.content.open_questions, ["还有哪些限制？"]);
    }

    #[test]
    fn markdown_strings_and_plain_text_persist_compact_fallback_notice() {
        let text = "# 自选标题\n\n这是说明,不是带有来源的原文事实。\n\n## 后续\n\n- 保留自由组织";
        for raw in [
            serde_json::json!(text),
            serde_json::json!({"markdown": text}),
            serde_json::json!({"content": text}),
        ] {
            let out = parse_summary_generation(raw.clone(), &[], coverage()).unwrap();
            assert_uncited_fallback(&out);
            assert_eq!(out.content.sections[0].title, "自选标题");
            assert_eq!(out.content.sections[1].title, "后续");
            assert_eq!(out.content.sections[1].segments[0].text, "- 保留自由组织");
            assert_uncited_fallback(&summary_from_plain_text(&raw, coverage()).unwrap());
        }
    }

    #[test]
    fn truncated_json_keeps_readable_text_without_protocol_or_sources() {
        let raw = serde_json::json!(
            r#"{"schemaVersion":"summary.draft.v1","sections":[{"title":"要点","segments":[{"text":"完整段落","kind":"document_fact","citationIds":["c1"]},{"text":"截断前的正文"}],"citations":[{"blockId":"private-block","quotedText":"不应成为引用""#
        );
        for out in [
            parse_summary_generation(raw.clone(), &[], coverage()).unwrap(),
            summary_from_plain_text(&raw, coverage()).unwrap(),
        ] {
            assert_uncited_fallback(&out);
            let readable = out
                .content
                .sections
                .iter()
                .flat_map(|s| &s.segments)
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(readable.contains("完整段落"));
            assert!(readable.contains("截断前的正文"));
            for marker in ["schemaVersion", "blockId", "citationIds", "private-block"] {
                assert!(!readable.contains(marker), "{readable}");
            }
        }
    }

    #[test]
    fn protocol_without_readable_text_is_not_a_summary() {
        let raw = serde_json::json!(r#"{"schemaVersion":"summary.draft.v1","blockId":"b1""#);
        assert!(parse_summary_generation(raw.clone(), &[], coverage()).is_err());
        assert!(summary_from_plain_text(&raw, coverage()).is_none());
    }

    #[test]
    fn fenced_code_preserves_blank_lines_comments_and_long_content() {
        for fence in ["```", "~~~~"] {
            let code = format!(
                "{fence}python\n# 代码注释不是标题\n\nprint(\"a\\nb\")\n{}\n{fence}",
                "# 保留这行代码\n".repeat(600)
            );
            let out = parse_summary_generation(
                serde_json::json!(format!("# 示例\n\n{code}\n\n## 结束\n\n结语")),
                &[],
                coverage(),
            )
            .unwrap();
            assert_uncited_fallback(&out);
            assert_eq!(out.content.sections.len(), 2);
            assert_eq!(out.content.sections[0].segments.len(), 1);
            assert_eq!(out.content.sections[0].segments[0].text, code);
            assert_eq!(out.content.sections[1].title, "结束");
        }
    }

    #[test]
    fn long_display_and_inline_math_are_not_split() {
        for (open, close) in [("$$", "$$"), (r"\[", r"\]")] {
            let math = format!("{open}\n\\nabla x\n\n{}x\n{close}", "x + ".repeat(1100));
            let out = parse_summary_generation(
                serde_json::json!(format!("# 公式\n\n{math}")),
                &[],
                coverage(),
            )
            .unwrap();
            assert_uncited_fallback(&out);
            assert_eq!(out.content.sections[0].segments.len(), 1);
            assert_eq!(out.content.sections[0].segments[0].text, math);
        }
        let inline = format!("{} $\\nabla x + y$ 结束", "甲".repeat(3998));
        let out = sections_from_plain_text(&inline);
        assert_eq!(out[0].segments.len(), 1);
        assert_eq!(out[0].segments[0].text, inline);
    }

    #[test]
    fn long_markdown_table_remains_one_renderable_block() {
        let table = format!(
            "| 列一 | 列二 |\n| --- | --- |\n{}",
            "| 内容 | 数据 |\n".repeat(700)
        );
        let out = parse_summary_generation(
            serde_json::json!(format!("# 表格\n\n{table}")),
            &[],
            coverage(),
        )
        .unwrap();
        assert_uncited_fallback(&out);
        assert_eq!(out.content.sections[0].segments.len(), 1);
        assert_eq!(out.content.sections[0].segments[0].text, table.trim());
    }

    #[test]
    fn long_prose_splits_without_losing_unicode_or_tail_content() {
        let text = format!(
            "{}终点",
            "甲乙".repeat(MAX_FALLBACK_SEGMENT_CHARS * MAX_FALLBACK_SEGMENTS / 2 + 10)
        );
        let sections = sections_from_plain_text(&text);
        let segments: Vec<_> = sections.iter().flat_map(|s| &s.segments).collect();
        assert!(segments.len() > 1);
        assert_eq!(
            segments.iter().map(|s| s.text.as_str()).collect::<String>(),
            text
        );
    }

    #[test]
    fn many_markdown_sections_do_not_silently_drop_the_tail() {
        let text = (0..55)
            .map(|i| format!("# 标题{i}\n\n正文{i}\n\n"))
            .collect::<String>();
        let sections = sections_from_plain_text(&text);
        for i in 0..55 {
            assert!(sections.iter().any(|s| s.title == format!("标题{i}")
                && s.segments.iter().any(|p| p.text == format!("正文{i}"))));
        }
    }

    #[test]
    fn duplicate_quotes_merge_claims_and_remap_all_segment_references() {
        let raw = serde_json::json!({"sections": [{"title": "共享来源", "segments": [
            {"id": "p1", "kind": "fact", "text": "第一点", "citationIds": ["c1"]},
            {"id": "p2", "kind": "fact", "text": "第二点", "citationIds": ["c2"]},
            {"id": "p3", "kind": "fact", "text": "第三点"}
        ]}], "citations": [
            {"id": "c1", "blockId": "b1", "quotedText": "原文", "claimSegmentIds": ["p1"]},
            {"id": "c2", "blockId": "b1", "quotedText": "原文", "claimSegmentIds": ["p3", "p3"]}
        ]});
        let out = parse_summary_generation(raw, &[block("b1", "原文")], coverage()).unwrap();
        assert_valid(&out);
        assert_eq!(out.citations.len(), 3);
        let segments = &out.content.sections[0].segments;
        for segment in segments {
            assert_eq!(segment.kind, SummarySegmentKind::DocumentFact);
            assert_eq!(segment.citation_ids.len(), 1);
            assert_eq!(segment.citation_ids, segments[0].citation_ids);
            assert_eq!(
                out.citations
                    .iter()
                    .filter(|c| c.claim_key == segment.id)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn reused_citation_ids_keep_distinct_claims_and_do_not_steal_reserved_ids() {
        let raw = serde_json::json!({"sections": [{"title": "引用", "segments": [
            {"id": "p1", "kind": "fact", "text": "甲", "citationIds": ["c1"]},
            {"id": "p2", "kind": "fact", "text": "乙", "citationIds": ["c1"]},
            {"id": "p3", "kind": "fact", "text": "丙", "citationIds": ["c2"]}
        ]}], "citations": [
            {"id": "c1", "blockId": "b1", "quotedText": "甲", "claimSegmentIds": ["p1"]},
            {"id": "c1", "blockId": "b2", "quotedText": "乙", "claimSegmentIds": ["p2"]},
            {"id": "c2", "blockId": "b3", "quotedText": "丙"}
        ]});
        let out = parse_summary_generation(
            raw,
            &[block("b1", "甲"), block("b2", "乙"), block("b3", "丙")],
            coverage(),
        )
        .unwrap();
        assert_valid(&out);
        assert_eq!(out.citations.len(), 3);
        for i in 1..=3 {
            let citation = out
                .citations
                .iter()
                .find(|c| c.claim_key == format!("p{i}"))
                .unwrap();
            assert_eq!(citation.content_block_id, format!("b{i}"));
            assert_eq!(
                out.content.sections[0].segments[i - 1].citation_ids.len(),
                1
            );
        }
        assert_eq!(out.content.sections[0].segments[2].citation_ids, ["c2"]);
    }

    #[test]
    fn renamed_segment_ids_keep_inline_citation_drafts() {
        let raw = serde_json::json!({"sections": [{"id": "p1", "title": "引用", "segments": [
            {"id": "p1", "kind": "fact", "text": "甲", "citations": [{"id": "a", "blockId": "b1", "quote": "甲"}]},
            {"id": "p1", "kind": "fact", "text": "乙", "citations": [{"id": "b", "blockId": "b2", "quote": "乙"}]}
        ]}]});
        let out =
            parse_summary_generation(raw, &[block("b1", "甲"), block("b2", "乙")], coverage())
                .unwrap();
        assert_valid(&out);
        assert_eq!(out.citations.len(), 2);
        for (index, segment) in out.content.sections[0].segments.iter().enumerate() {
            assert_eq!(segment.kind, SummarySegmentKind::DocumentFact);
            assert_eq!(segment.citation_ids.len(), 1);
            let citation = out
                .citations
                .iter()
                .find(|c| c.claim_key == segment.id)
                .unwrap();
            assert_eq!(citation.content_block_id, format!("b{}", index + 1));
        }
    }

    #[test]
    fn unknown_blocks_and_paraphrases_cannot_support_facts() {
        for (source, quote) in [("b2", "快照不可变"), ("b1", "快照不会改变")] {
            let mut raw = cited_raw();
            raw["citations"][0]["blockId"] = serde_json::json!(source);
            raw["citations"][0]["quotedText"] = serde_json::json!(quote);
            let out =
                parse_summary_generation(raw, &[block("b1", "快照不可变。")], coverage()).unwrap();
            assert_valid(&out);
            assert!(out.citations.is_empty());
            assert_eq!(
                out.content.sections[0].segments[0].kind,
                SummarySegmentKind::NeedsVerification
            );
            assert!(out.content.sections[0].segments[0].citation_ids.is_empty());
            assert!(out.content.open_questions.len() > 1);
        }
    }

    #[test]
    fn materials_over_budget_mark_partial_instead_of_pretending_full() {
        let mut blocks = Vec::new();
        for i in 0..(MAX_SUMMARY_BLOCKS + 1) {
            blocks.push(block(&format!("b{i}"), "短文本"));
        }
        let (selected, truncated) = summary_materials(&blocks).unwrap();
        assert!(truncated);
        assert_eq!(selected.len(), MAX_SUMMARY_BLOCKS);
    }
}
