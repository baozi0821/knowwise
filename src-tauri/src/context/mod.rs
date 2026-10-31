//! M07 上下文快照与会话(《M07》)。
//!
//! "用户看到的范围"与"实际冻结的上下文"分离的唯一领域所有者:
//! 建议只预填、发送前可见可改;请求发出后快照不可变,滚动、标注或切换
//! 章节只影响下一次建议。不解析文档、不执行检索、不调用模型、不保存 Artifact。
//!
//! 范围真相由 M04 块与覆盖提供;笔记与 AI 草稿由 M06/消息快照提供,
//! 冻结时复制当时文本,后续编辑不反写历史任务。

pub mod export;
pub mod models;
pub mod registration;
pub mod service;
pub mod summary;

#[cfg(feature = "desktop")]
pub mod commands;

pub use models::*;
pub use service::{
    complete_assistant_message_tx, create_assistant_placeholder_tx, create_user_message_tx,
    estimate_tokens, fail_assistant_message_tx, message_owner_exists_tx, validate_answer_content,
    ContextService, NewAssistantMessage, NewUserMessage, SnapshotProtectFn, SnapshotProtectors,
    DEFAULT_CONTEXT_TOKEN_BUDGET, MAX_SNAPSHOT_BLOCKS, MAX_SOURCES_PER_SNAPSHOT,
};
