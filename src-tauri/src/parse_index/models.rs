//! M04 领域对象与对外 DTO(《01》§4.2、《M04》§8)。
//! 契约:M04 独占 ContentBlock 与索引写入;覆盖状态是派生真相,
//! 不存在单独手写的"可问答"布尔。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::locator::SourceLocator;

/// 解析块类型(《01》§4.2 kind 封闭集)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum BlockKind {
    Heading,
    Paragraph,
    ListItem,
    Quote,
    Code,
    Table,
    TableRow,
    TableCell,
    Selection,
}

impl BlockKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Heading => "heading",
            Self::Paragraph => "paragraph",
            Self::ListItem => "list_item",
            Self::Quote => "quote",
            Self::Code => "code",
            Self::Table => "table",
            Self::TableRow => "table_row",
            Self::TableCell => "table_cell",
            Self::Selection => "selection",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "heading" => Self::Heading,
            "paragraph" => Self::Paragraph,
            "list_item" => Self::ListItem,
            "quote" => Self::Quote,
            "code" => Self::Code,
            "table" => Self::Table,
            "table_row" => Self::TableRow,
            "table_cell" => Self::TableCell,
            "selection" => Self::Selection,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum BlockOrigin {
    Parser,
    OnDemand,
    Ocr,
}

impl BlockOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Parser => "parser",
            Self::OnDemand => "on_demand",
            Self::Ocr => "ocr",
        }
    }

    pub fn parse_db(value: &str) -> Option<Self> {
        match value {
            "parser" => Some(Self::Parser),
            "on_demand" => Some(Self::OnDemand),
            "ocr" => Some(Self::Ocr),
            _ => None,
        }
    }
}

/// 块完整性:partial=有块但不完整;restricted=跨受限对象/结构受限。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum BlockCompleteness {
    Complete,
    Partial,
    Restricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum RegionKind {
    Page,
    Body,
    Heading,
    Object,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum RegionStatus {
    Pending,
    Partial,
    Complete,
    Failed,
    NoTextLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum OrderConfidence {
    High,
    Low,
}

/// 解析代次状态(《M04》§2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ParseState {
    /// 未开始(无代次或 queued)。
    NotStarted,
    InProgress,
    Complete,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum FtsState {
    NotBuilt,
    Building,
    Complete,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum VectorState {
    NotConfigured,
    Building,
    Complete,
    Partial,
    Failed,
    /// 向量代次属于旧解析代次,不可与新代次混查。
    Stale,
}

/// AI 能力从覆盖状态派生(《M04》§8 capability)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum Capability {
    /// 仅查看:无可用解析数据。
    ViewOnly,
    /// 无文本层(扫描件):仅选区按需解析问答。
    SelectionQa,
    /// 部分区域可用:局部问答,不得表述为全文完整。
    PartialQa,
    /// FTS-only:未配置嵌入或向量构建失败。
    FtsOnly,
    /// 完整混合检索。
    HybridFull,
    Failed,
}

// —— Worker 桥输入(不受信,服务端全量校验,《02》§5、《M04》§3)——

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ParserBlockInput {
    /// 代次内稳定;与 generationId 一起决定确定性块 ID。
    pub block_key: String,
    #[ts(type = "number")]
    pub ordinal: i64,
    /// 父块(表格行/单元格)的 blockKey;必须已在本批或更早批次出现。
    #[serde(default)]
    #[ts(optional)]
    pub parent_block_key: Option<String>,
    pub kind: BlockKind,
    /// 原文文本;规范化与哈希由 Rust 服务端计算,不信任 Worker。
    pub text: String,
    #[serde(default)]
    pub heading_path: Vec<String>,
    pub locator: SourceLocator,
    #[serde(default = "default_completeness")]
    pub completeness: BlockCompleteness,
    /// 按需块恒为 false;解析块默认 true。
    #[serde(default = "default_true")]
    pub indexable: bool,
    /// 块来源:文本层解析 / 页级 OCR。默认 parser(旧 Worker 不受影响)。
    #[serde(default = "default_origin")]
    pub origin: BlockOrigin,
    /// OCR 行/段落置信度(0..1);仅 origin=ocr 时有效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ocr_confidence: Option<f64>,
}

fn default_completeness() -> BlockCompleteness {
    BlockCompleteness::Complete
}
fn default_true() -> bool {
    true
}
fn default_origin() -> BlockOrigin {
    BlockOrigin::Parser
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ParserRegionInput {
    pub region_key: String,
    pub region_kind: RegionKind,
    pub status: RegionStatus,
    #[ts(type = "number")]
    #[ts(type = "number")]
    pub block_count: i64,
    #[serde(default = "default_order_confidence")]
    pub order_confidence: OrderConfidence,
    #[serde(default)]
    #[ts(optional)]
    pub error_code: Option<String>,
}

fn default_order_confidence() -> OrderConfidence {
    OrderConfidence::High
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ParserUnsupportedInput {
    pub region_key: String,
    /// 文本框/修订/批注/浮动对象等。
    pub object_type: String,
    pub locator: SourceLocator,
    pub reason_code: String,
}

/// 一个批次:默认 ≤32 块或 1 MiB(《M04》§3)。全部写入在一个事务中。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ParserBatchInput {
    #[ts(type = "number")]
    pub batch_seq: i64,
    #[serde(default)]
    pub blocks: Vec<ParserBlockInput>,
    #[serde(default)]
    pub regions: Vec<ParserRegionInput>,
    #[serde(default)]
    pub unsupported_regions: Vec<ParserUnsupportedInput>,
}

// —— 覆盖与状态视图(《M04》§8)——

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct RegionStatusView {
    pub region_key: String,
    pub region_kind: RegionKind,
    pub status: RegionStatus,
    #[ts(type = "number")]
    #[ts(type = "number")]
    pub block_count: i64,
    pub order_confidence: OrderConfidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct UnsupportedRegionView {
    pub region_key: String,
    pub object_type: String,
    pub reason_code: String,
    pub locator: SourceLocator,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct CoverageView {
    pub completed_regions: Vec<String>,
    pub partial_regions: Vec<String>,
    pub failed_regions: Vec<RegionStatusView>,
    pub unsupported_regions: Vec<UnsupportedRegionView>,
    /// PDF 是否有可用文本层;非 PDF 为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub has_text_layer: Option<bool>,
    /// 顺序或版式不确定区域(order_confidence=low)。
    pub order_warnings: Vec<String>,
}

// —— 2D 页级 OCR 状态 ——

pub const PAGE_OCR_SCHEMA_VERSION: &str = "page_ocr.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum PageOcrStatus {
    Pending,
    Rendering,
    OcrRunning,
    Completed,
    LowConfidence,
    Failed,
    Skipped,
}

impl PageOcrStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Rendering => "rendering",
            Self::OcrRunning => "ocr_running",
            Self::Completed => "completed",
            Self::LowConfidence => "low_confidence",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse_db(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "rendering" => Some(Self::Rendering),
            "ocr_running" => Some(Self::OcrRunning),
            "completed" => Some(Self::Completed),
            "low_confidence" => Some(Self::LowConfidence),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::LowConfidence | Self::Failed | Self::Skipped
        )
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct PageOcrStatusView {
    #[ts(type = "number")]
    pub page_number: i64,
    pub status: PageOcrStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confidence: Option<f64>,
    #[ts(type = "number")]
    pub block_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error_code: Option<String>,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub engine_version: Option<String>,
    #[ts(type = "number")]
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    #[ts(optional)]
    pub completed_at: Option<i64>,
}

/// 页级 OCR 暂停状态:按文档版本持久化,如实反映运行中的暂停信号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct PageOcrControlView {
    pub document_id: String,
    pub version_id: String,
    pub paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub paused_at: Option<i64>,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// 单页可编辑的 OCR 文本块(给 UI 纠错用;只含本页 OCR 原文)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct PageOcrBlockText {
    pub block_id: String,
    #[ts(type = "number")]
    pub ordinal: i64,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct PageOcrText {
    #[ts(type = "number")]
    pub page_number: i64,
    pub blocks: Vec<PageOcrBlockText>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrBlockCorrection {
    /// 已有块 ID;空 = 新增一段纠错文本(扫描页原本无可编辑块)。
    #[serde(default)]
    pub block_id: Option<String>,
    pub text: String,
}

/// 编辑器中明确删除的块(按 ID;后端删除块与 FTS,不伪造“已删除”)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrRecognitionBlock {
    pub text: String,
    /// 0..1;非 OCR 引擎结果可为空。
    #[serde(default)]
    pub confidence: Option<f64>,
    /// paragraph / heading 等;缺省 paragraph。
    #[serde(default)]
    pub kind: Option<String>,
    /// 与文本同页的真实定位(含 bbox,供引用回跳高亮)。
    pub locator: SourceLocator,
}

/// 指定页 OCR 的单页识别结果:前端 tesseract 识别后提交,后端只落库。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrRecognitionPage {
    pub page_number: i64,
    /// completed / low_confidence / failed
    pub status: String,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub engine_version: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub blocks: Vec<PageOcrRecognitionBlock>,
}

/// Worker 页状态上报(仅解析桥令牌可达;不落页图)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOcrReportEntry {
    pub page_number: i64,
    pub status: PageOcrStatus,
    #[serde(default)]
    pub quality: Option<serde_json::Value>,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub render_hash: Option<String>,
    #[serde(default)]
    pub engine_version: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub block_count: i64,
    #[serde(default)]
    pub error_code: Option<String>,
}

pub const MAX_PAGE_OCR_REPORTS: usize = 512;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ParseStatusView {
    pub document_id: String,
    pub version_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub generation_id: Option<String>,
    pub parse_state: ParseState,
    pub coverage: CoverageView,
    pub fts_state: FtsState,
    pub vector_state: VectorState,
    pub capability: Capability,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_task_id: Option<String>,
}

/// 解析桥令牌票据:随 `parser.request` 事件发给主窗口 Worker。
/// 不含文件路径;路径只存在于服务端令牌绑定中。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ParseJobTicket {
    pub token: String,
    #[ts(type = "number")]
    pub expires_at: i64,
    pub task_id: String,
    pub attempt_id: String,
    pub document_id: String,
    pub version_id: String,
    pub generation_id: String,
    pub format: crate::document::models::DocumentFormat,
    #[ts(type = "number")]
    pub input_size: i64,
    /// parser_read_input 单次读取上限(字节)。
    #[ts(type = "number")]
    pub chunk_size: i64,
    /// 输入读取方式:markdown/docx 全量字节流分块;PDF 用字节范围。
    pub read_mode: ParseReadMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ParseReadMode {
    /// 分块顺序读取(parser_read_input offset/length)。
    Chunked,
}

// —— 块检索结果(《M04》§10)——

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ContentBlockView {
    pub id: String,
    pub document_version_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parse_generation_id: Option<String>,
    pub origin: BlockOrigin,
    pub block_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub ordinal: Option<i64>,
    pub kind: BlockKind,
    pub text: String,
    pub heading_path: Vec<String>,
    pub locator: SourceLocator,
    pub completeness: BlockCompleteness,
    pub indexable: bool,
    /// OCR 置信度(0..1);非 OCR 块为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ocr_confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct FtsHit {
    pub block_id: String,
    /// 命中摘要(截断的规范化文本附近窗口,非正文全量)。
    pub snippet: String,
    pub rank: f64,
}

/// 按需选区块请求:来自 M03 RawSelection 的最小投影。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SelectionBlockRequest {
    pub document_id: String,
    pub version_id: String,
    pub text: String,
    pub locator: SourceLocator,
    #[serde(default)]
    pub heading_path: Vec<String>,
}

/// 数据库行(内部)。
#[derive(Debug, Clone)]
pub struct GenerationRow {
    pub id: String,
    pub document_version_id: String,
    pub parser_config_hash: String,
    pub parser_version: String,
    pub status: GenerationStatus,
    pub block_set_hash: Option<String>,
    pub is_current: bool,
    pub task_id: Option<String>,
    pub current_attempt_id: Option<String>,
    pub last_batch_seq: i64,
    pub error_code: Option<String>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationStatus {
    Queued,
    Parsing,
    Indexing,
    Complete,
    Partial,
    Failed,
    Canceled,
}

impl GenerationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Parsing => "parsing",
            Self::Indexing => "indexing",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "parsing" => Self::Parsing,
            "indexing" => Self::Indexing,
            "complete" => Self::Complete,
            "partial" => Self::Partial,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            _ => return None,
        })
    }
}

pub const GENERATION_COLS: &str = "id, document_version_id, parser_config_hash, parser_version, \
     status, block_set_hash, is_current, task_id, current_attempt_id, last_batch_seq, error_code, \
     started_at, completed_at";

impl<'r> TryFrom<&rusqlite::Row<'r>> for GenerationRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_version_id: r.get("document_version_id")?,
            parser_config_hash: r.get("parser_config_hash")?,
            parser_version: r.get("parser_version")?,
            status: GenerationStatus::parse_db(&r.get::<_, String>("status")?)
                .unwrap_or(GenerationStatus::Failed),
            block_set_hash: r.get("block_set_hash")?,
            is_current: r.get::<_, i64>("is_current")? != 0,
            task_id: r.get("task_id")?,
            current_attempt_id: r.get("current_attempt_id")?,
            last_batch_seq: r.get("last_batch_seq")?,
            error_code: r.get("error_code")?,
            started_at: r.get("started_at")?,
            completed_at: r.get("completed_at")?,
        })
    }
}

pub const BLOCK_COLS: &str = "id, document_version_id, parse_generation_id, origin, block_key, \
     ordinal, parent_block_id, kind, text, normalized_text, text_hash, normalized_text_hash, \
     heading_path_json, locator_json, completeness, indexable, normalization_version, \
     ocr_confidence, created_at";

#[derive(Debug, Clone)]
pub struct BlockRow {
    pub id: String,
    pub document_version_id: String,
    pub parse_generation_id: Option<String>,
    pub origin: BlockOrigin,
    pub block_key: String,
    pub ordinal: Option<i64>,
    pub parent_block_id: Option<String>,
    pub kind: BlockKind,
    pub text: String,
    pub completeness: BlockCompleteness,
    pub indexable: bool,
    pub ocr_confidence: Option<f64>,
    pub heading_path_json: String,
    pub locator_json: String,
}

impl BlockRow {
    pub fn to_view(&self) -> Option<ContentBlockView> {
        Some(ContentBlockView {
            id: self.id.clone(),
            document_version_id: self.document_version_id.clone(),
            parse_generation_id: self.parse_generation_id.clone(),
            origin: self.origin,
            block_key: self.block_key.clone(),
            ordinal: self.ordinal,
            kind: self.kind,
            text: self.text.clone(),
            heading_path: serde_json::from_str(&self.heading_path_json).ok()?,
            locator: serde_json::from_str(&self.locator_json).ok()?,
            completeness: self.completeness,
            indexable: self.indexable,
            ocr_confidence: self.ocr_confidence,
        })
    }
}

impl<'r> TryFrom<&rusqlite::Row<'r>> for BlockRow {
    type Error = rusqlite::Error;
    fn try_from(r: &rusqlite::Row<'r>) -> Result<Self, Self::Error> {
        Ok(Self {
            id: r.get("id")?,
            document_version_id: r.get("document_version_id")?,
            parse_generation_id: r.get("parse_generation_id")?,
            origin: BlockOrigin::parse_db(&r.get::<_, String>("origin")?)
                .unwrap_or(BlockOrigin::Parser),
            block_key: r.get("block_key")?,
            ordinal: r.get("ordinal")?,
            parent_block_id: r.get("parent_block_id")?,
            kind: BlockKind::parse_db(&r.get::<_, String>("kind")?).unwrap_or(BlockKind::Paragraph),
            text: r.get("text")?,
            completeness: match r.get::<_, String>("completeness")?.as_str() {
                "partial" => BlockCompleteness::Partial,
                "restricted" => BlockCompleteness::Restricted,
                _ => BlockCompleteness::Complete,
            },
            indexable: r.get::<_, i64>("indexable")? != 0,
            ocr_confidence: r.get("ocr_confidence")?,
            heading_path_json: r.get("heading_path_json")?,
            locator_json: r.get("locator_json")?,
        })
    }
}
