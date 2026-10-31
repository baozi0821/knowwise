//! 2C 翻译(translation.v1):段落双语、术语、翻译记忆与部分失败。

#[cfg(feature = "desktop")]
pub mod commands;
pub mod models;
pub mod offline;
#[cfg(feature = "local-mt")]
pub mod onnx_mt;
pub mod registration;
pub mod service;

pub use models::{TranslationGenerateRequest, TranslationV1};
pub use service::TranslationService;
