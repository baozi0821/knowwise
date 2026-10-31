//! 2F 文档地图:标题树 + 页范围 + 每节块数。
//!
//! 长文档不能把全文塞进 Prompt。文档地图先告诉模型“文档有哪些章节、
//! 各自在哪几页”,再由检索决定读哪些块。地图是**导航信息,不是证据**:
//! - 不进入引用白名单,模型不得引用它;
//! - 只包含标题与页码,不含正文段落;
//! - 超出 token 上限时截断并显式标注(不静默丢弃)。

use crate::parse_index::models::{BlockKind, ContentBlockView};

/// 地图文本的 token 上限(约占 system 层的一小部分)。
pub const DOCUMENT_MAP_TOKEN_BUDGET: usize = 1_200;
/// 最多列出的小节数。
pub const MAX_MAP_SECTIONS: usize = 60;

/// 一个章节条目(标题 + 页范围 + 块数)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapSection {
    pub heading_path: Vec<String>,
    pub page_start: Option<i64>,
    pub page_end: Option<i64>,
    pub block_count: usize,
}

/// 从快照块构建章节条目:按标题块切分,统计每节的页范围与块数。
/// 没有标题块的文档返回空(不伪造结构)。
pub fn build_sections(blocks: &[ContentBlockView]) -> Vec<MapSection> {
    let mut sections: Vec<MapSection> = Vec::new();
    let mut current: Option<MapSection> = None;
    for block in blocks {
        let is_heading = block.kind == BlockKind::Heading && !block.heading_path.is_empty();
        if is_heading {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            current = Some(MapSection {
                heading_path: block.heading_path.clone(),
                page_start: block.locator.pdf_page(),
                page_end: block.locator.pdf_page(),
                block_count: 0,
            });
            continue;
        }
        if let Some(section) = current.as_mut() {
            section.block_count += 1;
            if let Some(page) = block.locator.pdf_page() {
                section.page_start = Some(section.page_start.map_or(page, |start| start.min(page)));
                section.page_end = Some(section.page_end.map_or(page, |end| end.max(page)));
            }
        }
    }
    if let Some(section) = current {
        sections.push(section);
    }
    sections.truncate(MAX_MAP_SECTIONS);
    sections
}

/// 渲染为紧凑 Markdown(列表形式),并标注截断与“非证据”语义。
pub fn render_map(sections: &[MapSection], truncated: bool) -> Option<String> {
    if sections.is_empty() {
        return None;
    }
    let mut lines =
        vec!["[DOCUMENT MAP — navigation only, NOT a source block, never cite it]".to_string()];
    for section in sections {
        let title = section
            .heading_path
            .last()
            .map(|title| title.split('#').next().unwrap_or(title))
            .unwrap_or("(无标题)");
        let pages = match (section.page_start, section.page_end) {
            (Some(start), Some(end)) if start == end => format!("p.{start}"),
            (Some(start), Some(end)) => format!("p.{start}-{end}"),
            _ => "位置未知".to_string(),
        };
        lines.push(format!("- {title} · {pages} · {} 块", section.block_count));
    }
    if truncated {
        lines.push("- …(地图已截断,仅列出前面的小节)".to_string());
    }
    Some(lines.join("\n"))
}

/// 按 token 上限裁剪章节列表,返回 (文本, 是否截断)。
pub fn document_map_for(blocks: &[ContentBlockView]) -> Option<(String, bool)> {
    let sections = build_sections(blocks);
    if sections.is_empty() {
        return None;
    }
    let mut keep = sections.len();
    loop {
        let rendered = render_map(&sections[..keep], keep < sections.len())?;
        if super::budget::estimate_tokens(&rendered) <= DOCUMENT_MAP_TOKEN_BUDGET || keep <= 1 {
            return Some((rendered, keep < sections.len()));
        }
        keep -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_index::models::BlockCompleteness;
    use crate::shared::locator::{PageRotation, PdfRect, SourceLocator};

    fn block(kind: BlockKind, heading: Vec<&str>, text: &str, page: i64) -> ContentBlockView {
        ContentBlockView {
            id: format!("b{page}-{text}"),
            document_version_id: "v1".into(),
            parse_generation_id: Some("g1".into()),
            origin: crate::parse_index::models::BlockOrigin::Parser,
            block_key: text.to_string(),
            ordinal: Some(page),
            kind,
            text: text.to_string(),
            heading_path: heading.into_iter().map(str::to_string).collect(),
            locator: SourceLocator::pdf(
                page,
                vec![PdfRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                }],
                PageRotation::R0,
            ),
            completeness: BlockCompleteness::Complete,
            indexable: true,
            ocr_confidence: None,
        }
    }

    #[test]
    fn builds_sections_with_page_ranges_and_counts() {
        let blocks = vec![
            block(BlockKind::Heading, vec!["第一章"], "第一章", 1),
            block(BlockKind::Paragraph, vec!["第一章"], "正文", 1),
            block(BlockKind::Paragraph, vec!["第一章"], "正文 2", 2),
            block(BlockKind::Heading, vec!["第二章"], "第二章", 5),
            block(BlockKind::Paragraph, vec!["第二章"], "正文", 5),
        ];
        let sections = build_sections(&blocks);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].page_start, Some(1));
        assert_eq!(sections[0].page_end, Some(2));
        assert_eq!(sections[0].block_count, 2);
        assert_eq!(sections[1].page_start, Some(5));
        assert_eq!(sections[1].block_count, 1);
    }

    #[test]
    fn map_marks_itself_as_non_evidence_and_lists_pages() {
        let blocks = vec![
            block(BlockKind::Heading, vec!["预算"], "预算", 3),
            block(BlockKind::Paragraph, vec!["预算"], "正文", 4),
        ];
        let (map, truncated) = document_map_for(&blocks).expect("map");
        assert!(!truncated);
        assert!(map.contains("NOT a source block"), "地图必须声明不是证据");
        assert!(map.contains("预算 · p.3-4"), "实际:{map}");
    }

    #[test]
    fn documents_without_headings_have_no_map() {
        let blocks = vec![block(BlockKind::Paragraph, vec![], "纯段落", 1)];
        assert!(document_map_for(&blocks).is_none(), "不伪造章节结构");
    }
}
