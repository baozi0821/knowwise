//! M08 远程密钥存储(《M08》§6 / SEC-05)。
//!
//! 密钥只进操作系统凭据存储(Windows Credential Manager / macOS Keychain)。
//! 命令层只返回 `configured` 与更新时间;不存在"读取秘密"的命令路径。
//! 凭据服务不可用 → `SECRET_STORE_UNAVAILABLE`,禁止退回明文文件。
//! 日志与 AppError 永不携带秘密本体。

use crate::shared::AppError;

use super::errors::{model_err, ModelErrorCode};

/// 凭据存储端口:生产为系统凭据库;测试注入内存替身。
/// 实现永不允许把秘密写入任何文件。
pub trait SecretStore: Send + Sync {
    /// 写入(覆盖)指定提供方的密钥。
    fn set_secret(&self, provider_id: &str, secret: &str) -> Result<(), AppError>;
    /// 读取密钥(仅 EgressGateway 发送前调用;命令层禁止使用)。
    fn get_secret(&self, provider_id: &str) -> Result<Option<String>, AppError>;
    /// 删除;返回是否确实存在过。
    fn delete_secret(&self, provider_id: &str) -> Result<bool, AppError>;
}

/// 系统凭据存储实现(keyring crate)。
/// 服务条目名固定,用户名为 providerId;不支持平台不编译 keyring,
/// 所有操作返回 SECRET_STORE_UNAVAILABLE。
pub struct SystemSecretStore {
    service: &'static str,
}

impl SystemSecretStore {
    pub fn new() -> Self {
        Self {
            service: "Knowwise.ModelProvider",
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn entry(&self, provider_id: &str) -> Result<keyring::Entry, AppError> {
        keyring::Entry::new(self.service, provider_id).map_err(|_| secret_unavailable())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn entry(&self, _provider_id: &str) -> Result<std::convert::Infallible, AppError> {
        Err(secret_unavailable())
    }
}

impl Default for SystemSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

fn secret_unavailable() -> AppError {
    model_err(ModelErrorCode::SecretStoreUnavailable)
}

impl SecretStore for SystemSecretStore {
    #[cfg(any(windows, target_os = "macos"))]
    fn set_secret(&self, provider_id: &str, secret: &str) -> Result<(), AppError> {
        if secret.is_empty() {
            return Err(crate::shared::AppError::new(
                crate::shared::CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "empty_secret".into()));
        }
        let entry = self.entry(provider_id)?;
        entry.set_password(secret).map_err(|_| secret_unavailable())
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn get_secret(&self, provider_id: &str) -> Result<Option<String>, AppError> {
        let entry = self.entry(provider_id)?;
        match entry.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(secret_unavailable()),
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn delete_secret(&self, provider_id: &str) -> Result<bool, AppError> {
        let entry = self.entry(provider_id)?;
        match entry.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(_) => Err(secret_unavailable()),
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn set_secret(&self, _: &str, _: &str) -> Result<(), AppError> {
        Err(secret_unavailable())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn get_secret(&self, _: &str) -> Result<Option<String>, AppError> {
        Err(secret_unavailable())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn delete_secret(&self, _: &str) -> Result<bool, AppError> {
        Err(secret_unavailable())
    }
}

/// 内存替身:仅测试与开发装配(dev_fixtures)使用,不得进入生产组装根。
#[derive(Default)]
pub struct InMemorySecretStore {
    secrets: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// 注入失败以测试凭据不可用路径(A4)。
    pub fail_all: std::sync::atomic::AtomicBool,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_fail_all(&self, fail: bool) {
        self.fail_all
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    fn unavailable(&self) -> bool {
        self.fail_all.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl SecretStore for InMemorySecretStore {
    fn set_secret(&self, provider_id: &str, secret: &str) -> Result<(), AppError> {
        if self.unavailable() {
            return Err(secret_unavailable());
        }
        if secret.is_empty() {
            return Err(crate::shared::AppError::new(
                crate::shared::CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "empty_secret".into()));
        }
        self.secrets
            .lock()
            .unwrap()
            .insert(provider_id.to_string(), secret.to_string());
        Ok(())
    }

    fn get_secret(&self, provider_id: &str) -> Result<Option<String>, AppError> {
        if self.unavailable() {
            return Err(secret_unavailable());
        }
        Ok(self.secrets.lock().unwrap().get(provider_id).cloned())
    }

    fn delete_secret(&self, provider_id: &str) -> Result<bool, AppError> {
        if self.unavailable() {
            return Err(secret_unavailable());
        }
        Ok(self.secrets.lock().unwrap().remove(provider_id).is_some())
    }
}

/// 恒失败替身:验证凭据库不可用时永不落盘明文(A4)。
#[derive(Default)]
pub struct UnavailableSecretStore;

impl SecretStore for UnavailableSecretStore {
    fn set_secret(&self, _: &str, _: &str) -> Result<(), AppError> {
        Err(secret_unavailable())
    }
    fn get_secret(&self, _: &str) -> Result<Option<String>, AppError> {
        Err(secret_unavailable())
    }
    fn delete_secret(&self, _: &str) -> Result<bool, AppError> {
        Err(secret_unavailable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_roundtrip_and_delete() {
        let s = InMemorySecretStore::new();
        assert_eq!(s.get_secret("p1").unwrap(), None);
        s.set_secret("p1", "k-1").unwrap();
        assert_eq!(s.get_secret("p1").unwrap().as_deref(), Some("k-1"));
        assert!(s.delete_secret("p1").unwrap());
        assert!(!s.delete_secret("p1").unwrap());
        assert_eq!(s.get_secret("p1").unwrap(), None);
    }

    #[test]
    fn empty_secret_rejected() {
        let s = InMemorySecretStore::new();
        let err = s.set_secret("p1", "").unwrap_err();
        assert_eq!(err.code, "INVALID_REQUEST");
    }

    #[test]
    fn unavailable_store_maps_to_secret_store_unavailable() {
        let s = InMemorySecretStore::new();
        s.set_fail_all(true);
        let err = s.set_secret("p1", "k").unwrap_err();
        assert_eq!(err.code, "SECRET_STORE_UNAVAILABLE");
        assert_eq!(err.user_action, crate::shared::UserAction::None);
        // 替身不可用时也不得留下任何明文。
        assert!(s.secrets.lock().unwrap().is_empty());

        let u = UnavailableSecretStore;
        assert_eq!(
            u.get_secret("x").unwrap_err().code,
            "SECRET_STORE_UNAVAILABLE"
        );
    }
}
