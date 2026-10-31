//! M09 检索与问答编排(《M09》)。
//!
//! 模型只看到已授权、已冻结、可追溯的范围与候选。本模块负责检索与
//! 生成编排,不解析文档、不写索引、不保存回答正文(归 M07 消息)、
//! 不创建 Artifact。M10–M12 通过同一 StructuredGenerationPort 生成,
//! 持久化归产物模块。
//!
//! M07(快照)与 M08(模型出口)尚未实现时,通过 `ports` 中冻结的
//! Port 契约与测试替身开发(《00》§1);M13 组装根注入真实实现。

pub mod budget;
pub mod document_map;
pub mod error;
pub mod fusion;
pub mod history;
pub mod models;
pub mod ports;
pub mod prompt;
pub mod query;
pub mod registration;
pub mod service;
pub mod summary;

#[cfg(feature = "desktop")]
pub mod commands;

pub use fusion::{ExclusionReason, RetrievalConfig, DEFAULT_CONFIG};
pub use models::{
    AnswerContent, AnswerStatus, GenerationAttachment, SegmentKind, StructuredGenerationRequest,
    StructuredGenerationResult, ANSWER_SCHEMA_VERSION, PROMPT_INSTRUCTION_VERSION,
};
pub use ports::{
    resolve_capability_mode, CapabilityMode, ContextSnapshotPort, ModelCapabilityView,
    SnapshotContext, SnapshotScopeKind, StructuredGenerationPort,
};
pub use service::{answer_json_schema, AskRequest, RetrievalAiService};
