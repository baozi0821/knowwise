//! 2A 文档画像与动态卡片(insight_card.v1)。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::locator::SourceLocator;

pub const DOCUMENT_PROFILE_SCHEMA_VERSION: &str = "document_profile.v1";
pub const INSIGHT_CARD_SCHEMA_VERSION: &str = "insight_card.v1";

/// 文档画像(可重建缓存;版本/解析代次变化即失效)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct DocumentProfileView {
    pub schema_version: String,
    pub document_id: String,
    pub document_version_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parse_generation_id: Option<String>,
    pub profile_version: String,
    /// pdf / markdown / docx
    pub format: String,
    /// zh / en / mixed
    pub language: String,
    /// paper / contract / manual / report / textbook / general
    pub document_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub page_count: Option<i64>,
    #[ts(type = "number")]
    pub section_count: i64,
    pub title_tree: Vec<ProfileSection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ProfileSection {
    pub title: String,
    pub heading_path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub page_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub page_end: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum InsightItemKind {
    DocumentFact,
    AiInference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum InsightGeneratedBy {
    Ai,
    Extractive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum InsightConfidence {
    High,
    Medium,
    Low,
}

/// 卡片内嵌来源证据(不是 M05 Citation;不能冒充已校验引用)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct CardSourceRef {
    pub block_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub page: Option<i64>,
    #[serde(default)]
    pub heading_path: Vec<String>,
    /// 必须来自该块的原文(卡片校验会核对子串)。
    pub quote: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub locator: Option<SourceLocator>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct InsightCardItem {
    pub text: String,
    pub kind: InsightItemKind,
    pub source_refs: Vec<CardSourceRef>,
    #[serde(default)]
    pub citation_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct InsightCardAction {
    pub id: String,
    pub label: String,
    /// ask / show_evidence(后续工具入口在此扩展,未实现的动作不会出现)。
    pub tool: String,
    #[serde(default)]
    pub args: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct PageRange {
    #[ts(type = "number")]
    pub start: i64,
    #[ts(type = "number")]
    pub end: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct InsightCardV1 {
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    pub id: String,
    /// question_starters（当前唯一卡片类型）
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub subtitle: Option<String>,
    pub items: Vec<InsightCardItem>,
    pub actions: Vec<InsightCardAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_page_range: Option<PageRange>,
    pub generated_by: InsightGeneratedBy,
    #[ts(type = "number")]
    pub generated_at: i64,
    pub confidence: InsightConfidence,
}

impl InsightCardV1 {
    /// AI 只允许提交这些字段;id/kind/generated/时间由服务端覆盖。
    pub fn ai_submission_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["schemaVersion", "title", "items"],
            "properties": {
                "schemaVersion": {"const": INSIGHT_CARD_SCHEMA_VERSION},
                "title": {"type": "string"},
                "subtitle": {"type": "string"},
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 6,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["text", "kind", "sourceRefs"],
                        "properties": {
                            "text": {"type": "string"},
                            "kind": {"enum": ["document_fact", "ai_inference"]},
                            "sourceRefs": {
                                "type": "array",
                                "minItems": 1,
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["blockId", "quote"],
                                    "properties": {
                                        "blockId": {"type": "string"},
                                        "quote": {"type": "string"}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
    }
}

/// `insight_cards_get` 请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightCardsRequest {
    pub document_id: String,
    pub version_id: String,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub heading_path: Option<Vec<String>>,
    #[serde(default)]
    pub block_ids: Option<Vec<String>>,
}
