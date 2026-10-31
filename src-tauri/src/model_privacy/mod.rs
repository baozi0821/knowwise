//! M08 模型接入、隐私与设置(《M08》)。
//!
//! 唯一模型网络出口与唯一隐私授权裁决者:业务模块只能提出"要生成/嵌入"
//! 的请求,经 EgressGateway 的许可签发/消费/复核后才可能发出;密钥只进
//! 系统凭据存储,命令永不回显;本地与远程、生成与嵌入授权独立,按文档
//! 粒度管理。模型不可用只降级依赖任务,不是启动失败原因。
//!
//! 模块结构:
//! - `errors`     — M08 自有错误码(《02》§7 基线,不进 shared)
//! - `models`    — IPC DTO(ts-rs 导出)
//! - `settings`  — 非敏感模型设置(原子 JSON)
//! - `secrets`   — 凭据存储端口(keyring 实现 + 测试替身)
//! - `provider`  — ModelProvider 抽象与传输句柄
//! - `openai_compatible` / `ollama` — 协议适配器(用户自有远程 API 优先)
//! - `permit`    — EgressPermit(内存、短期、单次消费)
//! - `gateway`   — EgressGateway(唯一网络出口)
//! - `grants`    — PrivacyGrant 存取
//! - `clear`     — 派生数据清除协调(Contributor 注册 + M05/M06 接线)
//! - `diagnostics`— 脱敏诊断预览/导出
//! - `service`   — 应用服务门面

pub mod capability;
pub mod clear;
pub mod diagnostics;
pub mod errors;
pub mod gateway;
pub mod grants;
pub mod models;
pub mod ollama;
pub mod openai_compatible;
pub mod permit;
pub mod provider;
pub mod registration;
pub mod secrets;
pub mod service;
pub mod settings;

#[cfg(feature = "desktop")]
pub mod commands;

pub use capability::{
    CapabilityOverride, CapabilitySource, ModelCapability, ModelCapabilityService,
};
pub use errors::{model_err, ModelErrorCode, ProviderError};
pub use gateway::{
    messages_from_parts, EgressGateway, EgressRequestSpec, EgressStats, EgressStatsSnapshot,
    EgressTaskGate, GrantSource,
};
pub use grants::{document_exists_tx, GrantInput, GrantRow, GrantStore};
pub use models::*;
pub use permit::{EgressPayloadPart, EgressPermit};
pub use provider::{
    noop_delta, DeltaSink, EmbedRequest, GenerateRequest, GenerateSuccess, ModelProvider,
    ProviderAttachment, ProviderMessage, ProviderMessageRole, ProviderTransport,
};
pub use secrets::{InMemorySecretStore, SecretStore, SystemSecretStore, UnavailableSecretStore};
pub use service::{DefaultProviderFactory, ModelPrivacyService, ProviderFactory};
pub use settings::{
    ModelSettingsPatch, ModelSettingsStore, ModelSettingsV1, ModelSettingsView,
    ProviderConfigRecord,
};
