//! M09 端口定义(《M09》§2、§8)。
//!
//! M07(快照)与 M08(模型出口)由其他窗口实现;M09 在此处冻结
//! 自己需要消费的 Port 契约。生产组装根(M13)注入真实实现,
//! 测试注入替身。任何实现都不允许 M09 直接拿到 HTTP 客户端。

use crate::parse_index::models::{Capability, FtsState, VectorState};
use crate::shared::stream::SharedObserver;
use crate::shared::AppError;

use super::models::{ChatTurnRole, StructuredGenerationRequest, StructuredGenerationResult};

/// 快照范围类型(《01》§4.4)。M09 只消费冻结结果,不创建快照。
#[derive(Debug, Clone, PartialEq)]
pub enum SnapshotScopeKind {
    /// 明确选区/按需块:显式块集合,可直接装填。
    ExplicitBlocks,
    /// 局部页面/章节/多来源:范围内检索。
    Local,
    /// 全文(要求解析代次完整)。
    FullText,
}

/// M07 快照读取 Port(M09 视角的最小契约)。
#[derive(Debug, Clone)]
pub struct SnapshotContext {
    pub snapshot_id: String,
    pub document_id: String,
    pub document_version_id: String,
    pub parse_generation_id: Option<String>,
    pub scope_kind: SnapshotScopeKind,
    /// 显式块集合(ExplicitBlocks / Local 范围);全文快照为空 =
    /// 按代次 + blockSetHash 全文检索。
    pub explicit_block_ids: Vec<String>,
    /// 用户笔记 / AI 草稿来源(文本已冻结)。
    pub note_sources: Vec<super::models::GenerationInput>,
    /// 覆盖是否满足该快照声明的任务(《M09》§3)。
    pub coverage_complete: bool,
}

impl SnapshotScopeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExplicitBlocks => "explicit_blocks",
            Self::Local => "local",
            Self::FullText => "full_text",
        }
    }
}

/// M07 ContextSnapshotPort:读取并校验快照。
pub trait ContextSnapshotPort: Send + Sync {
    /// 加载快照;不存在返回 NotFound。
    fn load(&self, snapshot_id: &str) -> Result<SnapshotContext, AppError>;
}

/// M08 模型能力视图(《M09》§3 能力判断输入)。
#[derive(Debug, Clone)]
pub struct ModelCapabilityView {
    /// 本地 Ollama 或远程生成模型是否可用。
    pub generation_available: bool,
    /// 参考生成模型标识(不可用时空)。
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    /// 文档是否已授权当前用途的远程外发(本地模型恒为 true)。
    pub generation_authorized: bool,
}

/// M08 StructuredGenerationPort:唯一模型出口。产物模块不能自己创建
/// HTTP 请求;Port 内部经 EgressGateway 二次授权(《00》§3.4)。
pub trait StructuredGenerationPort: Send + Sync {
    /// 当前能力(编排前检查;实际发送前 M08 仍会再授权)。
    fn capability(&self, document_id: &str) -> Result<ModelCapabilityView, AppError>;

    /// 结构化生成。实现负责:授权、流式(仅进度)、超时分类、
    /// 最终 JSON 的 Schema 校验。取消由实现通过任务取消令牌感知。
    fn generate(
        &self,
        request: &StructuredGenerationRequest,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<StructuredGenerationResult, AppError>;

    /// 带真实流事件的生成。默认实现退回同步路径(测试替身无需改动);
    /// 生产实现把 provider reasoning/answer 增量转发给观察者。
    fn generate_streamed(
        &self,
        request: &StructuredGenerationRequest,
        cancel: &tokio_util::sync::CancellationToken,
        observer: &SharedObserver,
    ) -> Result<StructuredGenerationResult, AppError> {
        let _ = observer;
        self.generate(request, cancel)
    }

    /// 2C 翻译:使用 translation 角色(未配置时按 quality→fast 回退)。
    fn generate_translation(
        &self,
        request: &StructuredGenerationRequest,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<StructuredGenerationResult, AppError> {
        self.generate(request, cancel)
    }

    /// 当前生成模型的上下文窗口(2F:按真实窗口计算预算)。
    /// 未接线/未知返回 None,调用方回退保守默认值。
    fn context_window(&self) -> Option<usize> {
        None
    }

    /// 查询向量(M08 嵌入出口)。FTS-only/未配置嵌入返回 None;
    /// 返回向量必须与 `parse_generation_id` 的嵌入代次维度一致。
    ///
    /// `document_id` 用于文档级授权:向量查询是检索的子步骤,天然没有
    /// 独立 Task,但必须证明“这份文档已被授权嵌入”,不能匿名调用。
    fn query_embedding(
        &self,
        _query_text: &str,
        _parse_generation_id: &Option<String>,
        _document_id: &str,
    ) -> Option<QueryEmbedding> {
        None
    }
}

/// 会话摘要的一条历史消息(带稳定消息 ID,供摘要覆盖范围校验)。
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryTurn {
    pub message_id: String,
    pub role: ChatTurnRole,
    pub text: String,
}

/// M07 滚动摘要计划:M09 只消费,不决定"哪些消息该被压缩"。
#[derive(Debug, Clone, Default)]
pub struct ConversationSummaryPlan {
    /// 已保存的摘要(可能为空;失败时保留上一次成功摘要)。
    pub previous_summary: Option<String>,
    pub previous_up_to_message_id: Option<String>,
    /// ok / failed(最近一次摘要生成状态;失败不阻塞主问答)。
    pub previous_status: Option<String>,
    pub previous_error_code: Option<String>,
    /// 本次应压缩的消息(时间升序,已排除最近原文窗口)。
    pub messages: Vec<SummaryTurn>,
    /// 最近原文窗口之外、尚未进入本次摘要的消息总数。
    /// 大于 `messages.len()` 时说明本轮只压缩了其中一部分,必须向用户说明。
    pub older_unsummarized_turns: usize,
}

/// M07 提供:滚动摘要的读取与写入。生成由 M09 复用同一模型出口完成,
/// 避免 M07 直接持有生成能力或绕过 EgressGateway。
pub trait ConversationSummaryPort: Send + Sync {
    /// 读取当前摘要与下一批待压缩消息(不存在摘要时返回空计划)。
    fn plan(&self, conversation_id: &str) -> Result<ConversationSummaryPlan, AppError>;

    /// 保存成功摘要(推进覆盖范围;失败不会覆盖旧摘要)。
    fn save_summary(
        &self,
        conversation_id: &str,
        up_to_message_id: &str,
        summary_text: &str,
        token_count: usize,
    ) -> Result<(), AppError>;

    /// 记录失败状态(保留旧摘要文本与覆盖范围,供 UI/诊断可见)。
    fn save_failure(&self, conversation_id: &str, error_code: &str) -> Result<(), AppError>;
}

/// 检索到的候选(供编排层使用;来源 M04 检索服务)。
#[derive(Debug, Clone)]
pub struct RetrievedCandidate {
    pub block_id: String,
    pub rank: usize,
}

/// 查询向量来源(M08 嵌入;FTS-only 场景为 None)。
pub type QueryEmbedding = Vec<f32>;

pub fn port_unavailable() -> AppError {
    super::error::model_unavailable().with_detail("reason", "port_not_wired".into())
}

/// 由 parse 状态推导检索能力模式(《01》§4.6 capabilityMode)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityMode {
    ExplicitBlocks,
    FtsOnly,
    Hybrid,
    Unavailable,
}

impl CapabilityMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExplicitBlocks => "explicit_blocks",
            Self::FtsOnly => "fts_only",
            Self::Hybrid => "hybrid",
            Self::Unavailable => "unavailable",
        }
    }
}

/// 能力裁决(《M09》§3):无文本层不开放文档事实问答;
/// FTS-only 只服务关键词型任务并标记降级。
pub fn resolve_capability_mode(
    scope_kind: &SnapshotScopeKind,
    coverage_complete: bool,
    capability: Capability,
    fts_state: FtsState,
    vector_state: VectorState,
) -> CapabilityMode {
    match scope_kind {
        SnapshotScopeKind::ExplicitBlocks => CapabilityMode::ExplicitBlocks,
        SnapshotScopeKind::Local => {
            if matches!(fts_state, FtsState::Complete | FtsState::Partial) {
                CapabilityMode::FtsOnly
            } else {
                CapabilityMode::Unavailable
            }
        }
        SnapshotScopeKind::FullText => {
            if !coverage_complete {
                CapabilityMode::Unavailable
            } else if matches!(fts_state, FtsState::Complete)
                && matches!(vector_state, VectorState::Complete | VectorState::Partial)
                && matches!(capability, Capability::HybridFull)
            {
                CapabilityMode::Hybrid
            } else if matches!(fts_state, FtsState::Complete | FtsState::Partial) {
                CapabilityMode::FtsOnly
            } else {
                CapabilityMode::Unavailable
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_text_hybrid_requires_complete_coverage_and_both_indices() {
        let m = resolve_capability_mode(
            &SnapshotScopeKind::FullText,
            false,
            Capability::HybridFull,
            FtsState::Complete,
            VectorState::Complete,
        );
        assert_eq!(m, CapabilityMode::Unavailable);

        let m = resolve_capability_mode(
            &SnapshotScopeKind::FullText,
            true,
            Capability::HybridFull,
            FtsState::Complete,
            VectorState::Complete,
        );
        assert_eq!(m, CapabilityMode::Hybrid);

        // FTS-only:向量缺失 → 降级可用但界面须标注。
        let m = resolve_capability_mode(
            &SnapshotScopeKind::FullText,
            true,
            Capability::FtsOnly,
            FtsState::Complete,
            VectorState::NotConfigured,
        );
        assert_eq!(m, CapabilityMode::FtsOnly);
    }

    #[test]
    fn explicit_blocks_works_without_full_index() {
        let m = resolve_capability_mode(
            &SnapshotScopeKind::ExplicitBlocks,
            false,
            Capability::ViewOnly,
            FtsState::NotBuilt,
            VectorState::NotConfigured,
        );
        assert_eq!(m, CapabilityMode::ExplicitBlocks);
    }

    #[test]
    fn no_text_layer_is_unavailable_for_local() {
        let m = resolve_capability_mode(
            &SnapshotScopeKind::Local,
            true,
            Capability::Failed,
            FtsState::NotBuilt,
            VectorState::NotConfigured,
        );
        assert_eq!(m, CapabilityMode::Unavailable);
    }
}
