//! M03 阅读器 DTO:渲染计划、范围读取与阅读位置(《02》§3.1、《M03》§3/§7)。
//! 阅读位置不是掌握度,不参与学习画像;定位不含缩放/CSS 像素。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::dto::AccessTokenDescriptor;
use crate::shared::locator::SourceLocator;

use super::models::DocumentFormat;

/// 阅读器打开一个版本的渲染计划:访问信息与格式边界,不等待语义索引。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct RenderPlan {
    pub document_id: String,
    pub version_id: String,
    pub format: DocumentFormat,
    #[ts(type = "number")]
    pub file_size: i64,
    pub access_token: Option<AccessTokenDescriptor>,
    /// reader_read_range 建议分块(PDF range transport,约 1 MiB)。
    #[ts(type = "number")]
    pub chunk_size: i64,
    /// 是否支持字节范围读取(PDF;Markdown/DOCX 由前端整读或分块读)。
    pub range_read_supported: bool,
}

/// 阅读位置:按文档唯一保存,带乐观并发修订号。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ReadingPosition {
    pub document_id: String,
    pub version_id: String,
    pub locator: SourceLocator,
    #[ts(type = "number")]
    pub revision: i64,
    #[ts(type = "number")]
    pub saved_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReaderReadRangeRequest {
    pub access_token: String,
    #[serde(default)]
    pub offset: i64,
    pub length: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReaderSavePositionRequest {
    pub document_id: String,
    pub version_id: String,
    pub locator: SourceLocator,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReaderRenderPlanRequest {
    pub document_id: String,
    pub version_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReaderGetPositionRequest {
    pub document_id: String,
}
