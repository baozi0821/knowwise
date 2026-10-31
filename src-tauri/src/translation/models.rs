//! 2C translation.v1:段落级双语、术语、置信度与翻译记忆。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const TRANSLATION_SCHEMA_VERSION: &str = "translation.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum TranslationStatus {
    Translated,
    LowConfidence,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum TranslationConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GlossaryEntryV1 {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct TranslationSegmentV1 {
    pub id: String,
    pub source_block_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub page_number: Option<i64>,
    pub source_text_hash: String,
    pub source_text: String,
    pub translated_text: String,
    pub status: TranslationStatus,
    /// page / section / selection 等来源范围。
    pub scope: String,
    pub engine: String,
    pub confidence: TranslationConfidence,
    #[serde(default)]
    pub glossary_hits: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct TranslationCoverageV1 {
    pub complete: bool,
    #[ts(type = "number")]
    pub total_segments: i64,
    #[ts(type = "number")]
    pub translated_segments: i64,
    #[ts(type = "number")]
    pub failed_segments: i64,
    pub pages: Vec<i64>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct TranslationV1 {
    pub schema_version: String,
    pub source_language: String,
    pub target_language: String,
    pub style: String,
    pub segments: Vec<TranslationSegmentV1>,
    pub glossary: Vec<GlossaryEntryV1>,
    pub coverage: TranslationCoverageV1,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationGenerateRequest {
    pub document_id: String,
    pub snapshot_id: String,
    pub target_language: String,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub glossary: Vec<GlossaryEntryV1>,
    #[serde(default)]
    pub title: Option<String>,
    pub idempotency_key: String,
}

/// 写入前 Codec:结构完整、段落来源可追溯;不要求每个段都成功。
pub struct TranslationV1Codec;

impl TranslationV1Codec {
    pub fn kind(&self) -> &str {
        "translation"
    }
    pub fn schema_version(&self) -> &str {
        TRANSLATION_SCHEMA_VERSION
    }
    pub fn validate(&self, content: &serde_json::Value) -> bool {
        let Ok(parsed) = serde_json::from_value::<TranslationV1>(content.clone()) else {
            return false;
        };
        if parsed.schema_version != TRANSLATION_SCHEMA_VERSION || parsed.segments.is_empty() {
            return false;
        }
        parsed.segments.iter().all(|segment| {
            !segment.id.trim().is_empty()
                && !segment.source_block_id.trim().is_empty()
                && !segment.source_text_hash.trim().is_empty()
                && !segment.source_text.trim().is_empty()
        })
    }
}
