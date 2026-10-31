//! M12 流程论证图(《M12》分册)。
//!
//! 模块所有权:`graph.v1` Schema、生成校验、布局、渲染映射辅助与导出;
//! Artifact 身份、版本保护与保存由 M10 执行,生成编排由 M09 执行。
//! 图内容不建业务表,保存在 M10 `artifact_version.content_json`。
//!
//! 布局采用与 Dagre 同族的确定性分层算法在 Rust 侧实现(§4),
//! React Flow 只消费 LayoutResult,内部对象不持久化。

pub mod layout;
pub mod models;
pub mod outline;
pub mod registration;
pub mod service;
pub mod validate;

#[cfg(feature = "desktop")]
pub mod commands;

pub use models::*;
