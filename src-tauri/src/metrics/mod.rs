//! M13 运行指标(P0 1C):只保存耗时/比例,不含正文、推理、密钥或路径。

#[cfg(feature = "desktop")]
pub mod commands;
pub mod models;
pub mod registration;
pub mod service;

pub use models::{RunMetricsInput, RunMetricsView};
pub use service::RunMetricsService;
