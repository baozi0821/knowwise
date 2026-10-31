//! 统一定位联合 SourceLocator(M00 承载的跨模块契约,《01》§5)。
//! 封闭联合:各格式不得自行增加无版本的字段;演进必须升 locatorVersion。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct PdfRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum PageRotation {
    #[serde(rename = "0")]
    R0,
    #[serde(rename = "90")]
    R90,
    #[serde(rename = "180")]
    R180,
    #[serde(rename = "270")]
    R270,
}

/// 持久定位。PDF 矩形为未缩放 PDF user space;Markdown 偏移为
/// UTF-16 code unit offset(与 JavaScript 直接消费的约定一致,Rust 端
/// 不得当作字节索引);DOCX 为 OOXML 正文块顺序。
/// PDF 矩形坐标系标识。持久值恒为该常量;渲染时经 Viewport 变换。
pub const PDF_USER_SPACE_V1: &str = "pdf_user_space_v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SourceLocator {
    #[serde(rename_all = "camelCase")]
    Pdf {
        #[ts(type = "number")]
        page_number: i64,
        rects: Vec<PdfRect>,
        page_rotation: PageRotation,
        coordinate_space: String, // 构造时恒为 PDF_USER_SPACE_V1
    },
    #[serde(rename_all = "camelCase")]
    Markdown {
        #[ts(type = "number")]
        start_offset: i64,
        #[ts(type = "number")]
        end_offset: i64,
        mdast_node_type: String,
        heading_path: Vec<String>,
        #[ts(type = "number")]
        locator_version: i64,
    },
    #[serde(rename_all = "camelCase")]
    Docx {
        #[ts(type = "number")]
        body_block_start: i64,
        #[ts(type = "number")]
        body_block_end: i64,
        paragraph_ids: Vec<String>,
        heading_path: Vec<String>,
        #[ts(type = "number")]
        locator_version: i64,
    },
    #[serde(rename_all = "camelCase")]
    Composite {
        parts: Vec<SourceLocator>,
        #[ts(type = "number")]
        locator_version: i64,
    },
}

impl SourceLocator {
    pub fn pdf(page_number: i64, rects: Vec<PdfRect>, rotation: PageRotation) -> Self {
        Self::Pdf {
            page_number,
            rects,
            page_rotation: rotation,
            coordinate_space: PDF_USER_SPACE_V1.to_string(),
        }
    }

    pub fn markdown(
        start_offset: i64,
        end_offset: i64,
        mdast_node_type: impl Into<String>,
        heading_path: Vec<String>,
    ) -> Self {
        Self::Markdown {
            start_offset,
            end_offset,
            mdast_node_type: mdast_node_type.into(),
            heading_path,
            locator_version: 1,
        }
    }

    pub fn docx(
        body_block_start: i64,
        body_block_end: i64,
        paragraph_ids: Vec<String>,
        heading_path: Vec<String>,
    ) -> Self {
        Self::Docx {
            body_block_start,
            body_block_end,
            paragraph_ids,
            heading_path,
            locator_version: 1,
        }
    }

    /// 规范化序列化:用于确定性 ID 与快照哈希。结构体字段顺序固定 ⇒ 输出稳定。
    pub fn canonical_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// 主页面号(PDF 定位)或 None。
    pub fn pdf_page(&self) -> Option<i64> {
        match self {
            Self::Pdf { page_number, .. } => Some(*page_number),
            Self::Composite { parts, .. } => parts.iter().find_map(Self::pdf_page),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_serde_roundtrip_all_variants() {
        let cases: Vec<SourceLocator> = vec![
            SourceLocator::pdf(
                3,
                vec![PdfRect {
                    x: 1.5,
                    y: 2.0,
                    width: 10.0,
                    height: 4.0,
                }],
                PageRotation::R0,
            ),
            SourceLocator::markdown(10, 40, "paragraph", vec!["一、总则".into()]),
            SourceLocator::docx(5, 6, vec!["para-1".into()], vec![]),
            SourceLocator::Composite {
                parts: vec![SourceLocator::pdf(1, vec![], PageRotation::R90)],
                locator_version: 1,
            },
        ];
        for loc in &cases {
            let json = serde_json::to_string(loc).unwrap();
            let back: SourceLocator = serde_json::from_str(json.as_str()).unwrap();
            assert_eq!(back, *loc);
        }
    }

    #[test]
    fn tag_is_lowercase_kind() {
        let json = serde_json::to_value(SourceLocator::markdown(0, 1, "heading", vec![])).unwrap();
        assert_eq!(json["kind"], "markdown");
        assert_eq!(json["locatorVersion"], 1);
    }

    #[test]
    fn canonical_string_is_stable() {
        let a = SourceLocator::docx(1, 2, vec!["p".into()], vec!["H".into()]);
        let b = a.clone();
        assert_eq!(a.canonical_string(), b.canonical_string());
        assert!(a.canonical_string().contains("bodyBlockStart"));
    }
}
