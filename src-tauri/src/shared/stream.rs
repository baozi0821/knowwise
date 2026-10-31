//! 临时流事件抽象(M00):只用于 UI 展示,不是事实来源。
//!
//! 设计约束:
//! - 流片段永不落库;最终答案仍以 `answer.v1` / `agent.v1` / `assistant_notice.v1`
//!   的校验后消息为准。
//! - `reasoning` 与 `answer` 分通道;没有 provider reasoning 时不得伪造。
//! - 阶段、工具轨迹来自真实执行点,不按秒轮播固定文案。

use std::sync::Arc;

/// 增量通道:模型原始推理 / 可见答案正文 / 结构化 JSON 原文。
///
/// `StructuredJson` 只在进程内部流转:生成端口用它做增量解析,提取
/// `segments[].text` 后改发 `Answer`。它绝不能被映射到前端事件,
/// 以免把原始 JSON 渲染给用户。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamChannel {
    Reasoning,
    Answer,
    StructuredJson,
}

impl StreamChannel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reasoning => "reasoning",
            Self::Answer => "answer",
            Self::StructuredJson => "structured_json",
        }
    }
}

/// 单条增量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDelta {
    pub channel: StreamChannel,
    pub text: String,
}

impl StreamDelta {
    pub fn reasoning(text: impl Into<String>) -> Self {
        Self {
            channel: StreamChannel::Reasoning,
            text: text.into(),
        }
    }

    pub fn answer(text: impl Into<String>) -> Self {
        Self {
            channel: StreamChannel::Answer,
            text: text.into(),
        }
    }

    pub fn structured_json(text: impl Into<String>) -> Self {
        Self {
            channel: StreamChannel::StructuredJson,
            text: text.into(),
        }
    }
}

/// 真实执行阶段(不是“思考内容”)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStage {
    ResolvingScope,
    Preparing,
    Retrieving,
    Planning,
    Generating,
    Tool,
    Validating,
    Finalizing,
}

impl StreamStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ResolvingScope => "resolving_scope",
            Self::Preparing => "preparing",
            Self::Retrieving => "retrieving",
            Self::Planning => "planning",
            Self::Generating => "generating",
            Self::Tool => "tool",
            Self::Validating => "validating",
            Self::Finalizing => "finalizing",
        }
    }
}

/// 接收真实流事件的观察者。默认方法为空,调用方只覆写需要的通道。
pub trait StreamObserver: Send + Sync {
    fn on_stage(&self, _stage: StreamStage) {}
    fn on_reasoning(&self, _text: &str) {}
    fn on_answer(&self, _text: &str) {}
    /// 结构化解析失败时通知 UI 丢弃临时草稿(最终答案仍以数据库为准)。
    fn on_answer_reset(&self, _reason: &str) {}
    /// 看门狗的真实通知(不是模型推理,也不是固定文案)。
    fn on_notice(&self, _visible_message: &str) {}
    fn on_tool_started(&self, _step_id: &str, _tool: &str, _summary: &str) {}
    fn on_tool_finished(&self, _step_id: &str, _tool: &str, _status: &str, _summary: &str) {}
    fn on_heartbeat(&self, _stage: StreamStage, _elapsed_ms: i64) {}
}

pub type SharedObserver = Arc<dyn StreamObserver>;

struct NoopObserver;

impl StreamObserver for NoopObserver {}

/// 无观察者场景(评测、同步旧命令、测试)。
pub fn noop_observer() -> SharedObserver {
    Arc::new(NoopObserver)
}
