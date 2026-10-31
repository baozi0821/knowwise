//! M05 锚点与证据(《M05》)。
//!
//! "引用真实、定位真实"的唯一领域所有者:把阅读器选区转为 Anchor,
//! 把模型或用户产物的结论关联为 Citation,校验版本、块、引文和定位,
//! 并向 UI 提供证据透镜。不渲染文档、不保存回答正文;渲染与导航经 M03。
//!
//! 结构校验与语义支持分离;孤立不等于删除;旧引用默认快照,
//! 用户确认重定位前不自动绑定新版本。

pub mod models;
pub mod registration;
pub mod service;

#[cfg(feature = "desktop")]
pub mod commands;

pub use models::*;
pub use service::{
    anchor_summary_tx, delete_citations_for_owner_tx, effective_anchor_status, save_citations_tx,
    CitationValidation, EvidenceService, OwnerCheckFn, OwnerValidators, MAX_ANCHOR_BLOCKS,
    MAX_CITATIONS_PER_BATCH, MAX_RELOCATION_CANDIDATES,
};
