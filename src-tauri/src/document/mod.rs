//! M02 文档工作区与版本:导入、拖放、最近文档、文件访问、指纹、
//! 移动重连和内容版本;内含 M03 拥有的 `reader_*` 阅读器命令与服务。
//!
//! 路径或文件名不是身份;内容哈希不是每次启动阻断阅读的门槛。
//! 原文件只读;文件 I/O 不进事务。

pub mod detect;
pub mod models;
pub mod reader_models;
pub mod reader_registration;
pub mod reader_service;
pub mod registration;
pub mod service;

#[cfg(feature = "desktop")]
pub mod commands;

#[cfg(feature = "desktop")]
pub mod reader_commands;

pub use reader_service::ReaderService;
pub use service::{DocumentService, FingerprintOutcome, TokenBinding};
