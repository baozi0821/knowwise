//! M12 流程论证图领域对象与 DTO(《M12》§2 graph.v1 Schema)。
//!
//! 契约:所有图形数据先规范化成本 Schema;React Flow 内部对象不持久化,
//! 布局结果与用户位置以 `layout` 字段保存,不改变图语义与证据。
//! 图内容本身保存在 M10 的 `artifact_version.content_json`(《M12》§1),
//! M12 不拥有业务内容表。
//!
//! 证据真实性:`evidenceStatus=supported` 必须有至少一条 Citation;
//! 手工删除证据后状态降级为未核验,不得保留"已支持"视觉状态(《M12》§8)。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::AppError;

pub const GRAPH_SCHEMA_VERSION: &str = "graph.v1";

// —— 枚举(封闭联合,snake_case,《M12》§2)——

/// 图类型:MVP 支持 process(流程)与 argument(论证)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GraphType {
    Process,
    Argument,
}

impl GraphType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Argument => "argument",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "process" => Self::Process,
            "argument" => Self::Argument,
            _ => return None,
        })
    }
}

/// 节点类型(《M12》§2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GraphNodeKind {
    Step,
    Decision,
    Claim,
    Premise,
    Conclusion,
    Group,
}

impl GraphNodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Step => "step",
            Self::Decision => "decision",
            Self::Claim => "claim",
            Self::Premise => "premise",
            Self::Conclusion => "conclusion",
            Self::Group => "group",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "step" => Self::Step,
            "decision" => Self::Decision,
            "claim" => Self::Claim,
            "premise" => Self::Premise,
            "conclusion" => Self::Conclusion,
            "group" => Self::Group,
            _ => return None,
        })
    }
}

/// 边类型(《M12》§2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GraphEdgeKind {
    Sequence,
    Branch,
    Supports,
    Contradicts,
    Depends,
}

impl GraphEdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sequence => "sequence",
            Self::Branch => "branch",
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Depends => "depends",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "sequence" => Self::Sequence,
            "branch" => Self::Branch,
            "supports" => Self::Supports,
            "contradicts" => Self::Contradicts,
            "depends" => Self::Depends,
            _ => return None,
        })
    }
}

/// 连接确定性:不明确的连接必须以 uncertain 与虚线/文本标识呈现,
/// 不能因为布局需要虚构(《M12》§3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GraphCertainty {
    Stated,
    Inferred,
    Uncertain,
}

impl GraphCertainty {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stated => "stated",
            Self::Inferred => "inferred",
            Self::Uncertain => "uncertain",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "stated" => Self::Stated,
            "inferred" => Self::Inferred,
            "uncertain" => Self::Uncertain,
            _ => return None,
        })
    }
}

/// 证据状态(《M12》§2)。语义支持判断来源仍通过 M05 记录,
/// 本状态只表达"该节点/边的证据呈现档位"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GraphEvidenceStatus {
    Supported,
    Unverified,
    Insufficient,
    Conflict,
}

impl GraphEvidenceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unverified => "unverified",
            Self::Insufficient => "insufficient",
            Self::Conflict => "conflict",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "supported" => Self::Supported,
            "unverified" => Self::Unverified,
            "insufficient" => Self::Insufficient,
            "conflict" => Self::Conflict,
            _ => return None,
        })
    }
}

/// 布局方向(《M12》§4)。JSON/TS 契约为大写 "TB"/"LR"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "UPPERCASE")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GraphDirection {
    Tb,
    Lr,
}

impl GraphDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tb => "TB",
            Self::Lr => "LR",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "TB" => Self::Tb,
            "LR" => Self::Lr,
            _ => return None,
        })
    }
}

/// 节点/边来源:用户手工观点标记 user_view 或在说明中标明非原文,
/// 不自动生成 Citation(《M12》§5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GraphItemOrigin {
    #[default]
    Model,
    User,
}

impl GraphItemOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::User => "user",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "model" => Self::Model,
            "user" => Self::User,
            _ => return None,
        })
    }
}

/// 呈现格式:布局失败/超限时降级为文本大纲,降级状态写入元数据,
/// 不伪装为完整图形渲染成功(《M12》§6)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GraphFormatMode {
    Graph,
    Outline,
}

impl GraphFormatMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::Outline => "outline",
        }
    }
}

// —— Schema 主体(《M12》§2)——

/// 节点/边内联引用草稿(保存为产物版本时由 M10 经 M05 落成 Citation)。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphCitation {
    pub id: String,
    /// M04 ContentBlock ID(原文证据目标)。
    pub block_id: String,
    pub quoted_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub node_type: GraphNodeKind,
    #[serde(default)]
    #[ts(optional)]
    pub description: Option<String>,
    #[serde(default)]
    pub citations: Vec<GraphCitation>,
    pub evidence_status: GraphEvidenceStatus,
    #[serde(default)]
    pub origin: GraphItemOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default)]
    #[ts(optional)]
    pub label: Option<String>,
    pub edge_type: GraphEdgeKind,
    pub certainty: GraphCertainty,
    #[serde(default)]
    pub citations: Vec<GraphCitation>,
    pub evidence_status: GraphEvidenceStatus,
    #[serde(default)]
    pub origin: GraphItemOrigin,
}

/// 平面坐标(用户拖动位置;不影响图语义与证据,《M12》§4)。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphPoint {
    pub x: f64,
    pub y: f64,
}

/// 布局元数据:算法、方向与用户手工位置(独立布局字段,《M12》§4)。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphLayoutMeta {
    pub algorithm: String,
    pub direction: GraphDirection,
    /// 用户拖动后的节点位置(节点 ID → 坐标);自动重排前需确认覆盖。
    #[serde(default)]
    pub user_positions: HashMap<String, GraphPoint>,
}

impl Default for GraphLayoutMeta {
    fn default() -> Self {
        Self {
            algorithm: "layered".to_string(),
            direction: GraphDirection::Tb,
            user_positions: HashMap::new(),
        }
    }
}

/// 呈现格式元数据:降级状态与原因(《M12》§6)。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphFormatInfo {
    pub mode: GraphFormatMode,
    /// outline 模式的原因(budget_exceeded / cycle_dominated / layout_failed / user_choice)。
    #[serde(default)]
    #[ts(optional)]
    pub degraded_reason: Option<String>,
}

impl Default for GraphFormatInfo {
    fn default() -> Self {
        Self {
            mode: GraphFormatMode::Graph,
            degraded_reason: None,
        }
    }
}

/// graph.v1 内容根(《M12》§2)。持久化为 M10 artifact_version.content_json。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphContent {
    pub schema_version: String,
    pub graph_type: GraphType,
    pub title: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// 语义顺序标签大纲(校验时重新生成,保证与节点/边一致)。
    #[serde(default)]
    pub outline: Vec<String>,
    #[serde(default)]
    pub layout: GraphLayoutMeta,
    #[serde(default)]
    pub format: GraphFormatInfo,
}

impl GraphContent {
    pub fn node(&self, id: &str) -> Option<&GraphNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

// —— 校验报告 ——

/// 单条校验问题。`pointer` 为 JSON 风格定位(如 /nodes/3/id)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphIssue {
    pub code: String,
    pub pointer: String,
    pub message: String,
}

/// 校验输出:错误(拒绝保存)、警告(可见但不拒绝)与规范化后的内容。
/// 证据校验失败但图结构有效时可保存为"未核验图",只是不满足发布质量
/// (《M12》§3);结构失败则 normalized=None。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphValidationReport {
    pub valid: bool,
    pub errors: Vec<GraphIssue>,
    pub warnings: Vec<GraphIssue>,
    /// 结构有效时的规范化内容(证据降级 + 大纲重生成)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub normalized: Option<GraphContent>,
}

// —— 布局结果(内部 LayoutResult,《M12》§4)——

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct LayoutNode {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct LayoutEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    /// 折线路径(含端点);环形回边绕侧路由。
    pub points: Vec<GraphPoint>,
    /// certainty != stated ⇒ 虚线(《M12》§3/§5)。
    pub dashed: bool,
    /// 回边(参与环)标记:前端可叠加"回路"提示。
    pub back_edge: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct LayoutResult {
    pub algorithm: String,
    pub direction: GraphDirection,
    pub nodes: Vec<LayoutNode>,
    pub edges: Vec<LayoutEdge>,
    /// 参与环并被排除出分层的回边 ID(仍渲染,绕行路由)。
    pub back_edge_ids: Vec<String>,
    /// 画布尺寸(含边距;SVG/React Flow fitView 使用)。
    pub width: f64,
    pub height: f64,
}

/// 布局结果或降级信号:超限/环占优/异常 ⇒ 大纲降级,不产出空白产物
/// (《M12》§4/§6)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphLayoutOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub layout: Option<LayoutResult>,
    /// 降级原因(layout 为 None 时必有)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fallback_reason: Option<String>,
}

// —— 文本大纲(《M12》§6)——

/// 引用目标的原文位置(大纲"证据:第 X 页,第 Y 段"来源)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct EvidencePosition {
    pub block_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ordinal: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub heading_path: Option<Vec<String>>,
}

/// 大纲条目:节点 + 证据行 + 后继行(带确定性/证据标记)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphOutlineEntry {
    pub index: i64,
    pub node_id: String,
    pub label: String,
    pub evidence_lines: Vec<String>,
    pub successor_lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphOutline {
    pub title: String,
    pub graph_type: GraphType,
    pub entries: Vec<GraphOutlineEntry>,
    /// 规范化文本(《M12》§6 示例格式;作为降级产物正文保存)。
    pub text: String,
}

// —— 导出(《M12》§7)——

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum GraphExportFormat {
    Svg,
    Outline,
}

impl GraphExportFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Svg => "svg",
            Self::Outline => "outline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "svg" => Self::Svg,
            "outline" => Self::Outline,
            _ => return None,
        })
    }
}

/// 导出结果:只记录格式与字节数,不含完整目标路径(《M12》§7、《01》ExportRecord)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct GraphExportResult {
    pub format: GraphExportFormat,
    #[ts(type = "number")]
    pub bytes_written: i64,
}

// —— M12 模块错误码(《00》§7:M07+ 模块错误码由自有模块定义)——

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphErrorCode {
    GraphSchemaInvalid,
    GraphTooLarge,
    GraphLayoutFailed,
    GraphVersionSourceUnavailable,
    GraphExportTargetExists,
    GraphExportFailed,
}

impl GraphErrorCode {
    pub fn code(&self) -> &'static str {
        match self {
            Self::GraphSchemaInvalid => "GRAPH_SCHEMA_INVALID",
            Self::GraphTooLarge => "GRAPH_TOO_LARGE",
            Self::GraphLayoutFailed => "GRAPH_LAYOUT_FAILED",
            Self::GraphVersionSourceUnavailable => "GRAPH_VERSION_SOURCE_UNAVAILABLE",
            Self::GraphExportTargetExists => "GRAPH_EXPORT_TARGET_EXISTS",
            Self::GraphExportFailed => "GRAPH_EXPORT_FAILED",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::GraphLayoutFailed | Self::GraphExportFailed)
    }

    pub fn user_action(&self) -> crate::shared::UserAction {
        match self {
            Self::GraphSchemaInvalid | Self::GraphTooLarge => {
                crate::shared::UserAction::ChooseScope
            }
            Self::GraphLayoutFailed => crate::shared::UserAction::Retry,
            Self::GraphVersionSourceUnavailable => crate::shared::UserAction::None,
            Self::GraphExportTargetExists => crate::shared::UserAction::None,
            Self::GraphExportFailed => crate::shared::UserAction::Retry,
        }
    }
}

/// 构造带 M12 错误码的 AppError 信封(与 CoreErrorCode 同构,不复用枚举)。
pub fn graph_error(code: GraphErrorCode, trace_id: &str) -> AppError {
    AppError {
        schema_version: "error.v1",
        code: code.code().to_string(),
        message_key: format!("error.{}", code.code().to_lowercase()),
        retryable: code.retryable(),
        user_action: code.user_action(),
        details: None,
        trace_id: trace_id.to_string(),
    }
}
