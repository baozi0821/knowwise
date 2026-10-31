//! 非敏感设置(M00 机制,M02/M01 的默认值)。
//! 密钥永远不进入此文件(仅系统凭据存储,M08)。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::paths::{AtomicFileError, AtomicJsonFile};

pub const SETTINGS_SCHEMA_VERSION: &str = "settings.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SettingsV1 {
    pub schema_version: String,
    /// 各格式导入大小上限(MiB)。超限给出原因而不是读入内存后崩溃。
    pub max_pdf_mib: u64,
    pub max_docx_mib: u64,
    pub max_markdown_mib: u64,
    /// 启动恢复时本地解析任务是否自动创建新 Attempt(远程生成永不自动恢复)。
    pub auto_retry_parse_on_startup: bool,
}

impl Default for SettingsV1 {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION.into(),
            max_pdf_mib: 500,
            max_docx_mib: 100,
            max_markdown_mib: 50,
            auto_retry_parse_on_startup: false,
        }
    }
}

pub struct SettingsStore {
    file: AtomicJsonFile,
    cache: std::sync::Mutex<Option<SettingsV1>>,
}

#[derive(Debug)]
pub enum SettingsError {
    File(AtomicFileError),
    UnknownSchemaVersion(String),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(e) => write!(f, "settings file error: {e}"),
            Self::UnknownSchemaVersion(v) => write!(f, "unknown settings schemaVersion: {v}"),
        }
    }
}

impl SettingsStore {
    pub fn open(config_dir: &std::path::Path) -> Self {
        Self {
            file: AtomicJsonFile::new(config_dir.join("settings.json")),
            cache: std::sync::Mutex::new(None),
        }
    }

    /// 读取(带缓存)。未知 schemaVersion 拒绝返回默认值,防止降级覆盖。
    pub fn load(&self) -> Result<SettingsV1, SettingsError> {
        if let Some(cached) = self.cache.lock().unwrap().clone() {
            return Ok(cached);
        }
        match self
            .file
            .read_json::<SettingsV1>()
            .map_err(SettingsError::File)?
        {
            Some(s) if s.schema_version == SETTINGS_SCHEMA_VERSION => {
                *self.cache.lock().unwrap() = Some(s.clone());
                Ok(s)
            }
            Some(s) => Err(SettingsError::UnknownSchemaVersion(s.schema_version)),
            None => {
                let s = SettingsV1::default();
                *self.cache.lock().unwrap() = Some(s.clone());
                Ok(s)
            }
        }
    }

    pub fn save(&self, settings: &SettingsV1) -> Result<(), SettingsError> {
        let mut next = settings.clone();
        next.schema_version = SETTINGS_SCHEMA_VERSION.into();
        self.file.write_json(&next).map_err(SettingsError::File)?;
        *self.cache.lock().unwrap() = Some(next);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_roundtrip() {
        let dir = std::env::temp_dir().join(format!("kw-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = SettingsStore::open(&dir);
        let s = store.load().unwrap();
        assert_eq!(s.max_pdf_mib, 500);
        let mut s2 = s.clone();
        s2.max_pdf_mib = 42;
        store.save(&s2).unwrap();
        assert_eq!(store.load().unwrap().max_pdf_mib, 42);
        assert_eq!(store.load().unwrap().schema_version, "settings.v1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
