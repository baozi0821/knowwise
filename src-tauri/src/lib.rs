//! Knowwise Rust 核心:单 crate 多模块(《00_总览》§3.1)。
//!
//! `clippy::result_large_err` 全局豁免:AppError 是跨模块统一错误信封,
//! 按值返回换取调用点简洁;其体积(约 150B)在桌面应用场景无实际代价。
#![allow(clippy::result_large_err)]
// Tests are deliberately colocated next to the module they exercise.
#![allow(clippy::items_after_test_module)]
//!
//! 模块所有权:
//! - `shared`     — M00 共享内核(稳定 ID、时间、错误、事件、分页、工具)
//! - `persistence`— M01 持久化、任务与恢复
//! - `document`   — M02 文档工作区与版本;内含 M03 拥有的 `reader_*` 命令
//! - `parse_index`— M04 语义解析与索引
//! - `evidence`   — M05 锚点与证据(Anchor/Citation/校验/回跳/重定位)
//! - `notes`      — M06 标注、书签、笔记与待澄清
//! - `context`    — M07 上下文快照与会话(范围建议/不可变快照/消息)
//! - `attachments` — M15 图片附件(落盘/配额/摘要校验/孤儿扫描)
//! - `model_privacy` — M08 模型接入、隐私授权、密钥与清除协调(EgressGateway)
//! - `retrieval_ai` — M09 检索与问答编排(RRF 融合、结构化生成、引用编排)
//! - `artifacts`  — M10 产物中心与可编辑摘要(版本保护、Codec 注册)
//! - `exporters`  — M10 Markdown 导出器;M12 拥有 `exporters/graph`
//! - `graph`      — M12 流程论证图(graph.v1 Schema/校验/布局/大纲)
//! - `quiz`       — M11 理解校验题(quiz.v1 Codec、生成校验、作答与反馈)
//! - `app_shell` — M13 唯一组装根;契约部分(命令清单/安全配置/壳层状态/
//!   数据库装配)默认特性下也编译,Tauri Builder 位于
//!   `desktop` feature 之后
//!
//! 默认特性不包含 Tauri:核心模块与测试不依赖 WebView 运行时(见 Cargo.toml
//! 的已验证决策)。`#[tauri::command]` 命令层是薄封装,业务逻辑在服务层。

pub mod agent;
pub mod app_shell;
pub mod artifacts;
pub mod attachments;
#[cfg(feature = "desktop")]
pub mod brief;
pub mod context;
pub mod document;
pub mod evidence;
pub mod exporters;
pub mod graph;
pub mod insights;
pub mod metrics;
pub mod model_privacy;
pub mod notes;
pub mod parse_index;
pub mod persistence;
pub mod quiz;
pub mod retrieval_ai;
pub mod shared;
pub mod translation;

use shared::EventSink;

/// 应用核心运行时:各模块共享的端口装配。
/// M13 在 Tauri setup 中构建并注入真实实现;测试注入替身。
#[derive(Clone)]
pub struct CoreState {
    pub clock: std::sync::Arc<dyn shared::Clock>,
    pub ids: std::sync::Arc<dyn shared::IdGenerator>,
    pub events: std::sync::Arc<dyn EventSink>,
}

impl Default for CoreState {
    fn default() -> Self {
        Self {
            clock: std::sync::Arc::new(shared::SystemClock),
            ids: std::sync::Arc::new(shared::SystemIdGenerator),
            events: std::sync::Arc::new(shared::NoopEventSink),
        }
    }
}
