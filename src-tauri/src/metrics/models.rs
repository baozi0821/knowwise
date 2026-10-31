use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 命令层传入的一次运行指标(不含正文)。
#[derive(Debug, Clone)]
pub struct RunMetricsInput {
    pub task_id: String,
    pub message_id: Option<String>,
    pub mode: String,
    pub status: String,
    pub first_token_ms: Option<i64>,
    pub first_visible_answer_ms: Option<i64>,
    pub reasoning_first_event_ms: Option<i64>,
    pub max_event_gap_ms: i64,
    pub first_answer_ms: Option<i64>,
    pub total_ms: Option<i64>,
    pub fallback_used: bool,
    pub citation_valid_rate: Option<f64>,
}

/// 诊断面板读取的运行指标投影。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct RunMetricsView {
    pub id: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message_id: Option<String>,
    pub mode: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub first_token_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub first_visible_answer_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub reasoning_first_event_ms: Option<i64>,
    #[ts(type = "number")]
    pub max_event_gap_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub first_answer_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number | null")]
    pub total_ms: Option<i64>,
    pub fallback_used: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub citation_valid_rate: Option<f64>,
    #[ts(type = "number")]
    pub created_at: i64,
}
