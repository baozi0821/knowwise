//! 短期令牌注册表(M00 工具,M02 文档访问令牌与 M04 解析桥令牌共用)。
//! 令牌只存在于内存,过期/吊销后立即失效;不落库、不进日志。

use std::collections::HashMap;
use std::sync::Mutex;

struct Entry<T> {
    binding: T,
    expires_at_ms: i64,
}

/// 校验结果:None 表示令牌未知或已过期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    UnknownOrExpired,
}

pub struct TokenRegistry<T: Clone> {
    entries: Mutex<HashMap<String, Entry<T>>>,
}

impl<T: Clone> Default for TokenRegistry<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl<T: Clone> TokenRegistry<T> {
    pub fn issue(&self, token: &str, binding: T, now_ms: i64, ttl_ms: i64) {
        self.entries.lock().unwrap().insert(
            token.to_string(),
            Entry {
                binding,
                expires_at_ms: now_ms + ttl_ms,
            },
        );
    }

    /// 校验并克隆绑定数据;顺带清理已过期条目。
    pub fn validate(&self, token: &str, now_ms: i64) -> Result<T, TokenError> {
        let mut map = self.entries.lock().unwrap();
        map.retain(|_, e| e.expires_at_ms > now_ms);
        match map.get(token) {
            Some(e) if e.expires_at_ms > now_ms => Ok(e.binding.clone()),
            _ => Err(TokenError::UnknownOrExpired),
        }
    }

    pub fn revoke(&self, token: &str) {
        self.entries.lock().unwrap().remove(token);
    }

    /// 吊销全部满足谓词的令牌(例如某文档切换版本后全部重签)。
    pub fn revoke_where(&self, pred: impl Fn(&T) -> bool) {
        let mut map = self.entries.lock().unwrap();
        map.retain(|_, e| !pred(&e.binding));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_lifecycle() {
        let reg: TokenRegistry<&'static str> = TokenRegistry::default();
        reg.issue("t1", "doc-v1", 1000, 500);
        assert_eq!(reg.validate("t1", 1200).unwrap(), "doc-v1");
        assert!(matches!(
            reg.validate("t1", 2000),
            Err(TokenError::UnknownOrExpired)
        ));
    }

    #[test]
    fn revoked_token_is_invalid_even_before_expiry() {
        let reg: TokenRegistry<&'static str> = TokenRegistry::default();
        reg.issue("t1", "b", 0, 10_000);
        reg.revoke("t1");
        assert!(matches!(
            reg.validate("t1", 1),
            Err(TokenError::UnknownOrExpired)
        ));
    }

    #[test]
    fn revoke_where_targets_matching_bindings() {
        let reg: TokenRegistry<(u32, u32)> = TokenRegistry::default();
        reg.issue("a", (1, 2), 0, 10_000);
        reg.issue("b", (1, 3), 0, 10_000);
        reg.issue("c", (2, 9), 0, 10_000);
        reg.revoke_where(|(doc, _)| *doc == 1);
        assert!(matches!(
            reg.validate("a", 1),
            Err(TokenError::UnknownOrExpired)
        ));
        assert!(matches!(
            reg.validate("b", 1),
            Err(TokenError::UnknownOrExpired)
        ));
        assert_eq!(reg.validate("c", 1).unwrap(), (2, 9));
    }
}
