//! 通用分页(M00)。游标为不透明字符串;不提供 offset 语义。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct PageRequest {
    pub limit: i64,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl PageRequest {
    /// 规范化:limit 夹在 1..=200,超界不报错但结果可预期。
    pub fn normalized(&self) -> PageRequest {
        PageRequest {
            limit: self.limit.clamp(1, 200),
            cursor: self.cursor.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub total: Option<i64>,
}

impl<T> Page<T> {
    /// 从“多取一个”的探测结果构造分页:items 长度超过 limit 时截断并给出游标。
    pub fn from_probe(items: Vec<T>, limit: i64, cursor_of: impl Fn(&T) -> String) -> Self {
        let mut items = items;
        let next = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(cursor_of)
        } else {
            None
        };
        Self {
            items,
            next_cursor: next,
            total: None,
        }
    }
}

/// 游标编码:URL-safe base64 的 JSON,不透明于调用方。
pub fn encode_cursor<T: Serialize>(value: &T) -> String {
    use base64::Engine;
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn decode_cursor<T: for<'de> Deserialize<'de>>(cursor: &str) -> Option<T> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip() {
        let c = encode_cursor(&serde_json::json!({"k": "v", "n": 7}));
        let v: serde_json::Value = decode_cursor(&c).unwrap();
        assert_eq!(v["k"], "v");
        assert_eq!(v["n"], 7);
    }

    #[test]
    fn page_from_probe_truncates_and_sets_cursor() {
        let items = vec!["a", "b", "c", "d"];
        let page = Page::from_probe(items, 3, |s| s.to_string());
        assert_eq!(page.items, vec!["a", "b", "c"]);
        assert_eq!(page.next_cursor.as_deref(), Some("c"));
    }

    #[test]
    fn page_from_probe_exact_has_no_cursor() {
        let items = vec!["a", "b"];
        let page = Page::from_probe(items, 2, |s| s.to_string());
        assert_eq!(page.items.len(), 2);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn limit_is_clamped() {
        let req = PageRequest {
            limit: 5000,
            cursor: None,
        }
        .normalized();
        assert_eq!(req.limit, 200);
        let req = PageRequest {
            limit: 0,
            cursor: None,
        }
        .normalized();
        assert_eq!(req.limit, 1);
    }
}
