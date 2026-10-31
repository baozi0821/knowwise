//! 应用路径与原子配置文件(M00,见《M00》§6)。
//! 业务模块不拼接用户主目录;测试用 from_base 注入。

use std::path::{Path, PathBuf};

/// 应用目录布局。`base` 在生产环境为 Tauri `app_data_dir`。
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub base: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl AppPaths {
    pub fn from_base(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        Self {
            config_dir: base.join("config"),
            cache_dir: base.join("cache"),
            log_dir: base.join("logs"),
            base,
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.base.join("knowwise.sqlite3")
    }

    /// 迁移备份目录(Online Backup 产物,保留最近若干份)。
    pub fn backups_dir(&self) -> PathBuf {
        self.base.join("backups")
    }

    pub fn ensure_created(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.base)?;
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(&self.log_dir)?;
        Ok(())
    }
}

/// 原子 JSON 文件写:临时文件 → 刷盘 → rename。
/// 读失败/损坏不静默重置:损坏副本保留为 `<name>.corrupt` 并返回错误。
pub struct AtomicJsonFile {
    path: PathBuf,
}

#[derive(Debug)]
pub enum AtomicFileError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    Corrupted {
        preserved_as: PathBuf,
        source: serde_json::Error,
    },
}

impl std::fmt::Display for AtomicFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Serde(e) => write!(f, "serialize error: {e}"),
            Self::Corrupted { preserved_as, .. } => {
                write!(
                    f,
                    "corrupted config preserved at {}",
                    preserved_as.display()
                )
            }
        }
    }
}

impl AtomicJsonFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_json(&self, value: &impl serde::Serialize) -> Result<(), AtomicFileError> {
        let tmp = self.path.with_extension("tmp");
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(AtomicFileError::Io)?;
        }
        let body = serde_json::to_vec_pretty(value).map_err(AtomicFileError::Serde)?;
        // 写入 + 刷盘 + 原子 rename。
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp).map_err(AtomicFileError::Io)?;
            f.write_all(&body).map_err(AtomicFileError::Io)?;
            f.sync_all().map_err(AtomicFileError::Io)?;
        }
        // Windows rename 语义:目标存在时先移除(rename 覆盖在 Windows 上不可靠)。
        if self.path.exists() {
            let old = self.path.with_extension("old");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&self.path, &old).map_err(AtomicFileError::Io)?;
            std::fs::rename(&tmp, &self.path).map_err(AtomicFileError::Io)?;
            let _ = std::fs::remove_file(&old);
        } else {
            std::fs::rename(&tmp, &self.path).map_err(AtomicFileError::Io)?;
        }
        Ok(())
    }

    pub fn read_json<T: for<'de> serde::Deserialize<'de>>(
        &self,
    ) -> Result<Option<T>, AtomicFileError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let body = std::fs::read(&self.path).map_err(AtomicFileError::Io)?;
        match serde_json::from_slice(&body) {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                // 保留损坏副本供恢复,不静默重置。
                let preserved = self.path.with_extension("corrupt");
                let _ = std::fs::copy(&self.path, &preserved);
                Err(AtomicFileError::Corrupted {
                    preserved_as: preserved,
                    source: e,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_json_roundtrip_and_corruption_preserved() {
        let dir = std::env::temp_dir().join(format!("kw-test-{}", std::process::id()));
        let file = AtomicJsonFile::new(dir.join("settings.json"));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(file.read_json::<serde_json::Value>().unwrap().is_none());

        file.write_json(&serde_json::json!({"a": 1})).unwrap();
        let v: serde_json::Value = file.read_json().unwrap().unwrap();
        assert_eq!(v["a"], 1);

        // 覆盖写仍是原子的。
        file.write_json(&serde_json::json!({"a": 2})).unwrap();
        let v: serde_json::Value = file.read_json().unwrap().unwrap();
        assert_eq!(v["a"], 2);

        // 损坏文件被保留并报错,不静默重置。
        std::fs::write(file.path(), b"{not-json").unwrap();
        match file.read_json::<serde_json::Value>() {
            Err(AtomicFileError::Corrupted { preserved_as, .. }) => {
                assert!(preserved_as.exists(), "corrupt 副本必须保留");
            }
            other => panic!("expected Corrupted, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paths_layout_is_predictable() {
        let p = AppPaths::from_base("C:/data/kw");
        assert_eq!(p.db_path(), PathBuf::from("C:/data/kw/knowwise.sqlite3"));
        assert_eq!(p.backups_dir(), PathBuf::from("C:/data/kw/backups"));
    }
}
