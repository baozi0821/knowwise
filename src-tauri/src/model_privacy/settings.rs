//! M08 模型设置(非敏感,原子 JSON 文件;《M08》§6)。
//!
//! 与 `shared::settings::SettingsV1`(导入大小等)互不隶属:本文件只存放
//! 模型接入相关配置。密钥永不进入此文件(仅系统凭据存储,见 `secrets.rs`);
//! baseUrl 是用户输入的非秘密配置,可回显。损坏副本由 `AtomicJsonFile`
//! 保留为 `.corrupt` 并报错,不静默清空。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::paths::{AtomicFileError, AtomicJsonFile};
use crate::shared::{AppError, CoreErrorCode};

use super::models::{ModelPurpose, ProviderKind};

pub const MODEL_SETTINGS_SCHEMA_VERSION: &str = "model_settings.v1";
pub const MODEL_SETTINGS_FILE: &str = "model_settings.json";

/// 单个提供方配置(设置文件持久化形态)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ProviderConfigRecord {
    pub id: String,
    pub kind: ProviderKind,
    pub display_name: String,
    /// 远程提供方必填;Ollama 缺省用 `ollama_base_url`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub enabled: bool,
    /// 嵌入期望维度(与 EmbeddingGeneration 一致;缺省以探测为准)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    pub embed_dimensions: Option<i64>,
}

impl ProviderConfigRecord {
    pub fn new(id: &str, kind: ProviderKind, display_name: &str) -> Self {
        Self {
            id: id.to_string(),
            kind,
            display_name: display_name.to_string(),
            base_url: None,
            enabled: true,
            embed_dimensions: None,
        }
    }
}

/// serde(default) 需要:空对象反序列化时的占位(随后 validate 拒绝空 id)。
impl Default for ProviderConfigRecord {
    fn default() -> Self {
        Self::new("", ProviderKind::Ollama, "")
    }
}

/// 密钥元数据(非敏感:只记 configured 与更新时间)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SecretMetadata {
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    pub updated_at: Option<i64>,
}

/// 模型设置持久化形态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelSettingsV1 {
    pub schema_version: String,
    /// 乐观并发修订号:每次成功写入递增。
    #[ts(type = "number")]
    pub revision: i64,
    /// 默认本地链路地址(仅回环地址合法)。
    pub ollama_base_url: String,
    pub providers: Vec<ProviderConfigRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate_reference: Option<ModelReferenceRecord>,
    /// 角色化参考模型;未配置时按回退链使用 generate_reference。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fast_reference: Option<ModelReferenceRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub quality_reference: Option<ModelReferenceRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub translation_reference: Option<ModelReferenceRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_reference: Option<ModelReferenceRecord>,
    /// 远程请求并发上限(>0)。
    #[ts(type = "number")]
    pub max_concurrent_requests: i64,
    /// 单次模型请求超时(ms)。
    #[ts(type = "number")]
    pub request_timeout_ms: i64,
    /// 模型返回数据上限(字节;"模型返回大量数据要有上限")。
    #[ts(type = "number")]
    pub max_response_bytes: i64,
    /// 减少动态(可访问性偏好)。
    pub reduce_motion: bool,
    /// 独立开关:允许把图片发送到远程模型(默认关闭;本地链路不受影响)。
    pub allow_remote_vision: bool,
    /// 诊断默认勾选状态(导出前仍需逐次确认)。
    pub diagnostics_include_paths: bool,
    pub diagnostics_include_model_details: bool,
    /// 密钥元数据(仅 configured/updatedAt,永不含秘密本体)。
    pub secret_metadata: std::collections::BTreeMap<String, SecretMetadata>,
    // —— P0 1E 体验设置(写入同一设置文件,密钥除外)——
    /// 默认问答模式:fast / quality。
    pub default_mode: String,
    /// 默认页范围(±0/±1/±2)。
    #[ts(type = "number")]
    pub default_page_window: i64,
    /// 是否显示 provider 原始推理(关闭后仍保留真实工具/阶段轨迹)。
    pub show_raw_reasoning: bool,
    /// 是否保存思考记录(默认 false;开启后可在隐私清除中删除)。
    pub save_reasoning: bool,
    /// OCR 语言(chi_sim+eng 等;本地离线)。
    pub ocr_languages: String,
    /// 翻译目标语言(默认 zh-CN)。
    pub translation_target_language: String,
    /// 是否显示成本/时间预估。
    pub show_cost_estimate: bool,
    /// 外部 PDF 翻译引擎开关(默认关闭;AGPL 工具只作为外部进程)。
    pub allow_external_translation_engine: bool,
    /// 翻译术语表(强制命中;可导入/编辑/导出)。
    pub translation_glossary: Vec<TranslationGlossaryRecord>,
    /// 流式正文呈现速度:slow / standard / fast。只影响前端逐字呈现,
    /// 不改变生成过程与顺序。
    pub stream_speed: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct TranslationGlossaryRecord {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelReferenceRecord {
    pub provider_id: String,
    pub model_id: String,
}

impl Default for ModelSettingsV1 {
    fn default() -> Self {
        Self {
            schema_version: MODEL_SETTINGS_SCHEMA_VERSION.into(),
            revision: 1,
            ollama_base_url: "http://127.0.0.1:11434".into(),
            providers: Vec::new(),
            generate_reference: None,
            fast_reference: None,
            quality_reference: None,
            translation_reference: None,
            embed_reference: None,
            max_concurrent_requests: 2,
            request_timeout_ms: 120_000,
            max_response_bytes: 2 * 1024 * 1024,
            reduce_motion: false,
            allow_remote_vision: false,
            diagnostics_include_paths: false,
            diagnostics_include_model_details: false,
            secret_metadata: std::collections::BTreeMap::new(),
            default_mode: "fast".into(),
            default_page_window: 1,
            show_raw_reasoning: true,
            save_reasoning: false,
            ocr_languages: "chi_sim+eng".into(),
            translation_target_language: "zh-CN".into(),
            show_cost_estimate: true,
            allow_external_translation_engine: false,
            translation_glossary: Vec::new(),
            stream_speed: "standard".into(),
        }
    }
}

/// `settings_get` 返回(设置文件内容 + 修订号)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelSettingsView {
    pub schema_version: &'static str,
    #[ts(type = "number")]
    pub revision: i64,
    pub settings: ModelSettingsV1,
}

/// `settings_update` 的补丁(全部可选;None=不变)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ModelSettingsPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ollama_base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<ProviderConfigRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate_reference: Option<Option<ModelReferenceRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_reference: Option<Option<ModelReferenceRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_reference: Option<Option<ModelReferenceRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_reference: Option<Option<ModelReferenceRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_reference: Option<Option<ModelReferenceRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub max_concurrent_requests: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub request_timeout_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub max_response_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reduce_motion: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_remote_vision: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics_include_paths: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics_include_model_details: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub default_page_window: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_raw_reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_languages: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_target_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_cost_estimate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_external_translation_engine: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_glossary: Option<Vec<TranslationGlossaryRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_speed: Option<String>,
}

#[derive(Debug)]
pub enum ModelSettingsError {
    File(AtomicFileError),
    UnknownSchemaVersion(String),
    Invalid(String),
}

impl std::fmt::Display for ModelSettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(e) => write!(f, "model settings file error: {e}"),
            Self::UnknownSchemaVersion(v) => write!(f, "unknown model settings schemaVersion: {v}"),
            Self::Invalid(m) => write!(f, "invalid model settings: {m}"),
        }
    }
}

/// 回环主机判定:本地链路只允许 127.0.0.1 / [::1] / localhost。
pub fn is_loopback_base_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    match parsed.host_str() {
        Some(h) => h == "127.0.0.1" || h == "localhost" || h == "[::1]" || h == "::1",
        None => false,
    }
}

/// 远程 baseUrl 校验:http(s) 且有主机;文件/data 等协议拒绝。
pub fn valid_remote_base_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
}

impl ModelSettingsV1 {
    /// 业务校验(写入前):引用完整性、数值界限、本地回环限制。
    pub fn validate(&self) -> Result<(), ModelSettingsError> {
        if self.max_concurrent_requests < 1 || self.max_concurrent_requests > 16 {
            return Err(ModelSettingsError::Invalid(
                "max_concurrent_requests out of range".into(),
            ));
        }
        if self.request_timeout_ms < 1_000 || self.request_timeout_ms > 600_000 {
            return Err(ModelSettingsError::Invalid(
                "request_timeout_ms out of range".into(),
            ));
        }
        if self.max_response_bytes < 1024 || self.max_response_bytes > 64 * 1024 * 1024 {
            return Err(ModelSettingsError::Invalid(
                "max_response_bytes out of range".into(),
            ));
        }
        if !is_loopback_base_url(&self.ollama_base_url) {
            return Err(ModelSettingsError::Invalid(
                "ollama_base_url must be loopback".into(),
            ));
        }
        if self.default_mode != "fast" && self.default_mode != "quality" {
            return Err(ModelSettingsError::Invalid("default_mode invalid".into()));
        }
        if !(0..=2).contains(&self.default_page_window) {
            return Err(ModelSettingsError::Invalid(
                "default_page_window out of range".into(),
            ));
        }
        if self.ocr_languages.trim().is_empty() {
            return Err(ModelSettingsError::Invalid("ocr_languages empty".into()));
        }
        if self.translation_target_language.trim().is_empty() {
            return Err(ModelSettingsError::Invalid(
                "translation_target_language empty".into(),
            ));
        }
        if !matches!(self.stream_speed.as_str(), "slow" | "standard" | "fast") {
            return Err(ModelSettingsError::Invalid("stream_speed invalid".into()));
        }
        for entry in &self.translation_glossary {
            if entry.source.trim().is_empty() || entry.target.trim().is_empty() {
                return Err(ModelSettingsError::Invalid(
                    "translation_glossary entry empty".into(),
                ));
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        for p in &self.providers {
            if p.id.trim().is_empty() {
                return Err(ModelSettingsError::Invalid("provider id empty".into()));
            }
            if !ids.insert(p.id.as_str()) {
                return Err(ModelSettingsError::Invalid("duplicate provider id".into()));
            }
            match p.kind {
                ProviderKind::OpenaiCompatible => {
                    let Some(url) = &p.base_url else {
                        return Err(ModelSettingsError::Invalid(
                            "remote provider missing base_url".into(),
                        ));
                    };
                    if !valid_remote_base_url(url) {
                        return Err(ModelSettingsError::Invalid(
                            "remote provider base_url invalid".into(),
                        ));
                    }
                }
                ProviderKind::Ollama => {
                    if let Some(url) = &p.base_url {
                        // Ollama 提供方也只允许回环目的地(《M08》§2)。
                        if !is_loopback_base_url(url) {
                            return Err(ModelSettingsError::Invalid(
                                "ollama provider base_url must be loopback".into(),
                            ));
                        }
                    }
                }
            }
            if let Some(d) = p.embed_dimensions {
                if !(1..=8192).contains(&d) {
                    return Err(ModelSettingsError::Invalid(
                        "embed_dimensions out of range".into(),
                    ));
                }
            }
        }
        for (role, reference) in [
            (ModelPurpose::Generate, &self.generate_reference),
            (ModelPurpose::Generate, &self.fast_reference),
            (ModelPurpose::Generate, &self.quality_reference),
            (ModelPurpose::Generate, &self.translation_reference),
            (ModelPurpose::Embed, &self.embed_reference),
        ] {
            if let Some(r) = reference {
                let provider = self
                    .providers
                    .iter()
                    .find(|p| p.id == r.provider_id)
                    .ok_or_else(|| {
                        ModelSettingsError::Invalid(format!("{role:?} reference provider missing"))
                    })?;
                if !provider.enabled {
                    return Err(ModelSettingsError::Invalid(format!(
                        "{role:?} reference provider disabled"
                    )));
                }
                if r.model_id.trim().is_empty() {
                    return Err(ModelSettingsError::Invalid(format!(
                        "{role:?} reference model empty"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn apply_patch(&mut self, patch: &ModelSettingsPatch) {
        if let Some(v) = &patch.ollama_base_url {
            self.ollama_base_url = v.clone();
        }
        if let Some(v) = &patch.providers {
            self.providers = v.clone();
        }
        if let Some(v) = &patch.generate_reference {
            self.generate_reference = v.clone();
        }
        if let Some(v) = &patch.fast_reference {
            self.fast_reference = v.clone();
        }
        if let Some(v) = &patch.quality_reference {
            self.quality_reference = v.clone();
        }
        if let Some(v) = &patch.translation_reference {
            self.translation_reference = v.clone();
        }
        if let Some(v) = &patch.embed_reference {
            self.embed_reference = v.clone();
        }
        if let Some(v) = patch.max_concurrent_requests {
            self.max_concurrent_requests = v;
        }
        if let Some(v) = patch.request_timeout_ms {
            self.request_timeout_ms = v;
        }
        if let Some(v) = patch.max_response_bytes {
            self.max_response_bytes = v;
        }
        if let Some(v) = patch.reduce_motion {
            self.reduce_motion = v;
        }
        if let Some(v) = patch.allow_remote_vision {
            self.allow_remote_vision = v;
        }
        if let Some(v) = patch.diagnostics_include_paths {
            self.diagnostics_include_paths = v;
        }
        if let Some(v) = patch.diagnostics_include_model_details {
            self.diagnostics_include_model_details = v;
        }
        if let Some(v) = &patch.default_mode {
            self.default_mode = v.clone();
        }
        if let Some(v) = patch.default_page_window {
            self.default_page_window = v;
        }
        if let Some(v) = patch.show_raw_reasoning {
            self.show_raw_reasoning = v;
        }
        if let Some(v) = patch.save_reasoning {
            self.save_reasoning = v;
        }
        if let Some(v) = &patch.ocr_languages {
            self.ocr_languages = v.clone();
        }
        if let Some(v) = &patch.translation_target_language {
            self.translation_target_language = v.clone();
        }
        if let Some(v) = patch.show_cost_estimate {
            self.show_cost_estimate = v;
        }
        if let Some(v) = patch.allow_external_translation_engine {
            self.allow_external_translation_engine = v;
        }
        if let Some(v) = &patch.translation_glossary {
            self.translation_glossary = v.clone();
        }
        if let Some(v) = &patch.stream_speed {
            self.stream_speed = v.clone();
        }
    }
}

pub struct ModelSettingsStore {
    file: AtomicJsonFile,
    cache: std::sync::Mutex<Option<ModelSettingsV1>>,
}

impl ModelSettingsStore {
    pub fn open(config_dir: &std::path::Path) -> Self {
        Self {
            file: AtomicJsonFile::new(config_dir.join(MODEL_SETTINGS_FILE)),
            cache: std::sync::Mutex::new(None),
        }
    }

    pub fn file_path(&self) -> &std::path::Path {
        self.file.path()
    }

    /// 读取(带缓存)。未知 schemaVersion 或损坏文件报错,不静默重置。
    pub fn load(&self) -> Result<ModelSettingsV1, ModelSettingsError> {
        if let Some(cached) = self.cache.lock().unwrap().clone() {
            return Ok(cached);
        }
        match self
            .file
            .read_json::<ModelSettingsV1>()
            .map_err(ModelSettingsError::File)?
        {
            Some(s) if s.schema_version == MODEL_SETTINGS_SCHEMA_VERSION => {
                *self.cache.lock().unwrap() = Some(s.clone());
                Ok(s)
            }
            Some(s) => Err(ModelSettingsError::UnknownSchemaVersion(s.schema_version)),
            None => {
                let s = ModelSettingsV1::default();
                *self.cache.lock().unwrap() = Some(s.clone());
                Ok(s)
            }
        }
    }

    /// 写入(原子):先校验再落盘,成功后修订号已在调用方递增。
    pub fn save(&self, settings: &ModelSettingsV1) -> Result<(), ModelSettingsError> {
        let mut next = settings.clone();
        next.schema_version = MODEL_SETTINGS_SCHEMA_VERSION.into();
        next.validate()?;
        self.file
            .write_json(&next)
            .map_err(ModelSettingsError::File)?;
        *self.cache.lock().unwrap() = Some(next);
        Ok(())
    }
}

pub fn settings_io_err(e: ModelSettingsError) -> AppError {
    AppError::new(CoreErrorCode::Internal, crate::shared::new_trace_id())
        .with_detail("reason", "model_settings_io".into())
        .with_detail("message", e.to_string().into())
}

pub fn settings_invalid_err(e: ModelSettingsError) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
        .with_detail("reason", "model_settings_invalid".into())
        .with_detail("message", e.to_string().into())
}

/// 修订号冲突 → CONFLICT_REVISION_MISMATCH(不强行覆盖,《02》§1)。
pub fn settings_revision_conflict() -> AppError {
    AppError::new(
        CoreErrorCode::ConflictRevisionMismatch,
        crate::shared::new_trace_id(),
    )
    .with_detail("reason", "model_settings_revision_mismatch".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("kw-m08-settings-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn defaults_validate_and_roundtrip() {
        let d = dir("roundtrip");
        let store = ModelSettingsStore::open(&d);
        let s = store.load().unwrap();
        s.validate().unwrap();
        assert_eq!(s.revision, 1);
        let mut next = s.clone();
        next.revision += 1;
        next.providers.push(ProviderConfigRecord::new(
            "remote-main",
            ProviderKind::OpenaiCompatible,
            "Remote",
        ));
        next.providers[0].base_url = Some("https://api.example.com/v1".into());
        store.save(&next).unwrap();
        assert_eq!(store.load().unwrap().providers.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn remote_provider_requires_valid_base_url() {
        let mut s = ModelSettingsV1::default();
        let mut p = ProviderConfigRecord::new("r", ProviderKind::OpenaiCompatible, "R");
        p.base_url = Some("ftp://x".into());
        s.providers.push(p);
        assert!(s.validate().is_err());
        s.providers[0].base_url = Some("https://api.example.com".into());
        s.validate().unwrap();
    }

    #[test]
    fn ollama_destinations_must_be_loopback() {
        let mut s = ModelSettingsV1 {
            ollama_base_url: "http://0.0.0.0:11434".into(),
            ..ModelSettingsV1::default()
        };
        assert!(s.validate().is_err());
        s.ollama_base_url = "http://127.0.0.1:11434".into();
        assert!(s.validate().is_ok());
        assert!(is_loopback_base_url("http://localhost:11434"));
        assert!(is_loopback_base_url("http://[::1]:11434"));
        assert!(!is_loopback_base_url("http://lan-host:11434"));
        assert!(!is_loopback_base_url("not a url"));
    }

    #[test]
    fn reference_must_point_to_enabled_provider() {
        let mut s = ModelSettingsV1::default();
        let mut p = ProviderConfigRecord::new("r", ProviderKind::OpenaiCompatible, "R");
        p.base_url = Some("https://api.example.com".into());
        p.enabled = false;
        s.providers.push(p);
        s.generate_reference = Some(ModelReferenceRecord {
            provider_id: "r".into(),
            model_id: "m".into(),
        });
        assert!(s.validate().is_err());
        s.providers[0].enabled = true;
        s.validate().unwrap();
        // 指向不存在的提供方同样拒绝。
        s.embed_reference = Some(ModelReferenceRecord {
            provider_id: "ghost".into(),
            model_id: "m".into(),
        });
        assert!(s.validate().is_err());
    }

    #[test]
    fn old_settings_without_stream_speed_defaults_standard() {
        // 旧配置没有 streamSpeed 字段:仍可读,默认标准速度。
        let parsed: ModelSettingsV1 =
            serde_json::from_str(r#"{"schemaVersion":"model_settings.v1"}"#).unwrap();
        assert_eq!(parsed.stream_speed, "standard");
        parsed.validate().unwrap();
    }

    #[test]
    fn patch_applies_only_provided_fields() {
        let mut s = ModelSettingsV1::default();
        let before = s.ollama_base_url.clone();
        let patch = ModelSettingsPatch {
            reduce_motion: Some(true),
            ..Default::default()
        };
        s.apply_patch(&patch);
        assert!(s.reduce_motion);
        assert_eq!(s.ollama_base_url, before);
    }

    #[test]
    fn corrupted_file_is_preserved_not_reset() {
        let d = dir("corrupt");
        let store = ModelSettingsStore::open(&d);
        let mut s = store.load().unwrap();
        s.revision += 1;
        store.save(&s).unwrap();
        drop(store);
        std::fs::write(d.join(MODEL_SETTINGS_FILE), b"{broken").unwrap();
        let store = ModelSettingsStore::open(&d);
        assert!(matches!(
            store.load(),
            Err(ModelSettingsError::File(AtomicFileError::Corrupted { .. }))
        ));
        assert!(
            d.join("model_settings.corrupt").exists(),
            "损坏副本必须保留"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
