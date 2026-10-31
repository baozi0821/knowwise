//! P2 Agent 只读循环的领域对象与端口契约(WBS-2/WBS-3)。
//!
//! 默认路径仍是 answer.v1 引用式问答;Agent 只是显式“深度研究”开关。
//! 本模块只做只读工具,不执行任何写工具;所有模型调用经 EgressGateway,
//! 每一步单独签发许可。

use serde::Serialize;
use ts_rs::TS;

use crate::context::models::{AgentAnswerV1, AgentStepView, ContextSnapshotView};
use crate::model_privacy::provider::{ProviderMessage, ToolSpec};
use crate::persistence::task::TaskRow;
use crate::retrieval_ai::models::{ChatTurn, GenerationAttachment};
use crate::shared::stream::SharedObserver;
use crate::shared::AppError;

/// 单轮 Agent 最多发起的模型请求数(含最后强制收敛请求)。
pub const MAX_STEPS: usize = 8;
/// 质量模式墙钟预算:超过后不再发起新步骤,用已获证据收敛。
/// 与 `MAX_STEPS` 共同构成“步数 + 时间”双预算(Dify/LangGraph 同款)。
pub const RUN_WALL_CLOCK_BUDGET_MS: i64 = 150_000;
/// 触发“将收敛”提示的预算比例(80%)。
pub const BUDGET_WARN_PERCENT: i64 = 80;
/// 单次工具返回的字符预算;超出必须 truncated=true 并给出剩余匹配数。
pub const TOOL_RESULT_MAX_CHARS: usize = 8000;
/// 单个工具条目文本上限(避免一个块吃掉整个预算)。
pub const TOOL_ITEM_MAX_CHARS: usize = 1200;

/// 模型/提供方能力(Agent 预制检查)。
#[derive(Debug, Clone)]
pub struct AgentModelCapability {
    pub available: bool,
    pub authorized: bool,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
}

/// 单步模型请求(含完整消息与工具清单;许可由端口实现按内容重新签发)。
#[derive(Debug, Clone)]
pub struct AgentModelRequest {
    pub task_id: String,
    pub attempt_id: String,
    pub document_id: String,
    pub snapshot_id: String,
    /// 启动时选定的提供方/模型:每步复核,设置变化必须显式失败而非静默换模型。
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub messages: Vec<ProviderMessage>,
    pub tools: Vec<ToolSpec>,
    pub json_schema: Option<serde_json::Value>,
}

/// 单步模型响应。
#[derive(Debug, Clone)]
pub struct AgentModelResponse {
    pub content: String,
    pub tool_calls: Vec<crate::model_privacy::provider::ToolCall>,
    pub provider_id: String,
    pub model_id: String,
}

/// 只在只读工具执行期内使用的冻结快照投影。
#[derive(Debug, Clone)]
pub struct AgentToolContext {
    pub document_id: String,
    pub snapshot_id: String,
    pub document_version_id: String,
    pub parse_generation_id: Option<String>,
    pub scope_kind: crate::context::models::ScopeKind,
    /// 快照为固定页范围时的 (start,end);用于工具区分“范围外未读取”和“页面为空”。
    pub page_scope: Option<(i64, i64)>,
    /// 快照允许的块(按 ordinal 排序;全文快照为代次内全部块)。
    pub blocks: Vec<crate::parse_index::models::ContentBlockView>,
}

/// 工具返回条目(只供模型;UI 步骤时间线只显示计数与状态)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultItem {
    pub source_id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    pub text: String,
}

/// 统一工具返回形状 `{ items, truncated, total_matches }` + 可解释原因。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub tool: String,
    pub items: Vec<ToolResultItem>,
    pub truncated: bool,
    pub total_matches: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 工具执行摘要(只进 agent_step,不进正文)。
#[derive(Debug, Clone)]
pub struct ToolResultSummary {
    pub items: usize,
    pub truncated: bool,
    pub total_matches: usize,
    pub reason: Option<String>,
}

/// 运行输入(命令层组装:M07 会话/消息 + M09 历史 + M15 附件)。
pub struct AgentRunInput {
    pub document_id: String,
    pub snapshot: ContextSnapshotView,
    pub conversation_id: String,
    pub assistant_message_id: String,
    pub question: String,
    pub locale: String,
    pub idempotency_key: String,
    pub history: Vec<ChatTurn>,
    pub history_summary: Option<String>,
    pub attachments: Vec<GenerationAttachment>,
    /// 真实流事件观察者(仅临时展示;非流式入口使用 noop)。
    pub observer: SharedObserver,
}

/// 启动结果:已完成(幂等重放)或已创建任务/运行并等待后台执行。
pub enum AgentStart {
    Ready {
        task: TaskRow,
        answer: Box<AgentAnswerV1>,
    },
    Started {
        task: TaskRow,
        job: Box<AgentJob>,
    },
}

/// 后台执行所需的全部输入(不借用 Tauri State)。
pub struct AgentJob {
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub document_id: String,
    pub snapshot: ContextSnapshotView,
    pub conversation_id: String,
    pub assistant_message_id: String,
    pub question: String,
    pub locale: String,
    pub history: Vec<ChatTurn>,
    pub history_summary: Option<String>,
    pub attachments: Vec<GenerationAttachment>,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub observer: SharedObserver,
}

/// 执行结果(成功):任务终态 + 可直接写入助手消息的 agent.v1。
#[derive(Debug)]
pub struct AgentRunOutcome {
    pub task: TaskRow,
    pub answer: AgentAnswerV1,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
}

/// `agent_get_steps` 的读模型(按 taskId 轮询/回放)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AgentRunStepsView {
    pub task_id: String,
    pub run_id: String,
    pub status: String,
    pub steps: Vec<AgentStepView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stopped_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error_code: Option<String>,
}

/// 端口:Agent 单步生成。生产实现走 EgressGateway;测试注入替身。
pub trait AgentGenerationPort: Send + Sync {
    fn capability(&self, document_id: &str) -> Result<AgentModelCapability, AppError>;

    fn generate(
        &self,
        request: &AgentModelRequest,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<AgentModelResponse, AppError>;

    /// 带真实 reasoning 增量的单步生成;默认退回同步路径(测试替身)。
    fn generate_streamed(
        &self,
        request: &AgentModelRequest,
        cancel: &tokio_util::sync::CancellationToken,
        observer: &SharedObserver,
    ) -> Result<AgentModelResponse, AppError> {
        let _ = observer;
        self.generate(request, cancel)
    }
}

/// 稳定的 Agent 内部错误码(不进入 shared::CoreErrorCode)。
pub fn agent_invalid(reason: &str) -> AppError {
    crate::retrieval_ai::error::model_output_invalid(reason)
}

pub fn tool_args_invalid(reason: &str) -> AppError {
    AppError::new(
        crate::shared::CoreErrorCode::InvalidRequest,
        crate::shared::new_trace_id(),
    )
    .with_detail("reason", reason.into())
}
