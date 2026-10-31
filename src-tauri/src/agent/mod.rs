//! Agent 循环:M13 编排(P2 只读 + P3A 写工具人在环)。
//!
//! 只在用户显式开启“深度研究”时使用;默认问答仍走 M09 answer.v1。
//! 只读工具受冻结快照约束;写工具必须先进入 awaiting_confirmation,
//! 经用户批准后才调用 M10/M11/导出器的既有校验与写入路径。

#[cfg(feature = "desktop")]
pub mod commands;
pub mod generation;
pub mod models;
pub mod prompt;
pub mod reasoning;
pub mod registration;
pub mod service;
pub mod tools;
pub mod write_tools;
