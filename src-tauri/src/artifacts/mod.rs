//! M10 产物中心与可编辑摘要(《M10》)。
//!
//! 拥有 Artifact 外壳、版本写入保护规则、Codec 注册契约与 Markdown 导出。
//! M09 只生成内容不建产物;M11/M12 的生成器/渲染器经注册接入,本模块
//! 不 import M11/M12。摘要生成编排(Map/Reduce/覆盖)归 M09;本模块
//! 落库 ai_draft、提供编辑版本链与导出。

pub mod models;
pub mod registration;
pub mod service;

#[cfg(feature = "desktop")]
pub mod commands;

pub use models::*;
pub use service::{
    AiDraftCommitInput, ArtifactService, MAX_ARTIFACT_TITLE_CHARS, MAX_VERSIONS_PER_ARTIFACT,
};
