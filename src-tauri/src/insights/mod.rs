//! 2A 文档画像与动态卡片(缓存可重建;不写用户资产)。

#[cfg(feature = "desktop")]
pub mod commands;
pub mod models;
pub mod registration;
pub mod service;

pub use models::{DocumentProfileView, InsightCardV1, InsightCardsRequest};
pub use service::CardService;
