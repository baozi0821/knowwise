//! 统一文本规范化 normalize_v1(M00,契约见《01》§2)。
//! 依次:NFKC → 去软连字符 → 合并跨行断词 `-\n` → 折叠连续空白。
//! 保留规范化字符到原文 UTF-16 code unit 偏移的映射。
//! 任何情况下保存与展示的原文快照都使用未修改的原文 `text`。

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// 规范化结果:normalized 中的每个 UTF-16 码元对应 map 里的原文偏移。
/// map.len() == normalized.len() (按 UTF-16 码元)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedText {
    pub normalized: String,
    /// 规范化文本第 i 个 UTF-16 码元 → 原文 UTF-16 偏移。
    pub utf16_map: Vec<usize>,
}

impl NormalizedText {
    /// 在规范化文本中查找 needle(先对 needle 做 normalize_v1),
    /// 返回匹配对应的原文 [start, end) UTF-16 偏移区间。
    pub fn find_original_range(&self, needle_norm: &NormalizedText) -> Option<(usize, usize)> {
        let hay: Vec<u16> = self.normalized.encode_utf16().collect();
        let needle: Vec<u16> = needle_norm.normalized.encode_utf16().collect();
        if needle.is_empty() || needle.len() > hay.len() {
            return None;
        }
        let n = needle.len();
        (0..=hay.len() - n)
            .find(|&i| hay[i..i + n] == needle[..])
            .map(|i| {
                let start = self.utf16_map[i];
                // 区间终点 = 匹配之后第一个保留字符的原文偏移;末尾则退化为末字符偏移 +1。
                let end = if i + n < self.utf16_map.len() {
                    self.utf16_map[i + n]
                } else {
                    self.utf16_map[i + n - 1] + 1
                };
                let end = end.max(start + 1);
                (start, end)
            })
    }
}

/// SHA-256 十六进制小写(内容指纹的统一实现)。
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

pub fn sha256_str(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

fn is_ws(c: char) -> bool {
    c.is_whitespace()
}

/// normalize_v1 主入口。逐字符 NFKC 并保留原文偏移映射;
/// 跨字符组合(如预分解的重音序列)不参与组合,此为 v1 的已知边界,
/// 精确子串匹配仍是权威路径,规范化匹配仅处理换行/软连字符/兼容差异。
pub fn normalize_v1(text: &str) -> NormalizedText {
    // 第一步:展开为 (char, 原文 UTF-16 码元偏移, 原文 UTF-16 长度)。
    // 注意:char_indices 给出的是字节偏移,契约要求 UTF-16 偏移(《01》§5)。
    let mut expanded: Vec<(char, usize, usize)> = Vec::with_capacity(text.len());
    let mut utf16_pos: usize = 0;
    for ch in text.chars() {
        let utf16_len = ch.len_utf16();
        let at = utf16_pos;
        let nfkc: Vec<char> = ch.nfkc().collect();
        if nfkc.is_empty() {
            // NFKC 不产生空序列,防御分支。
            expanded.push((ch, at, utf16_len));
        } else {
            for c in nfkc {
                expanded.push((c, at, utf16_len));
            }
        }
        utf16_pos += utf16_len;
    }

    // 第二步:去软连字符、合并 `-\n` 断词、折叠空白,产出规范化码元与映射。
    let mut norm_chars: Vec<(char, usize)> = Vec::with_capacity(expanded.len());
    let mut i = 0usize;
    while i < expanded.len() {
        let (c, orig_off, _) = expanded[i];
        // 软连字符直接丢弃。
        if c == '\u{00AD}' {
            i += 1;
            continue;
        }
        // 行尾连字符断词:`-` + 可选 `\r` + `\n` ⇒ 全部删除。
        if c == '-' {
            let mut j = i + 1;
            if j < expanded.len() && expanded[j].0 == '\r' {
                j += 1;
            }
            if j < expanded.len() && expanded[j].0 == '\n' {
                i = j + 1;
                continue;
            }
        }
        // 折叠连续空白为单个空格(映射到首个空白字符的原文偏移)。
        if is_ws(c) {
            let mut j = i;
            while j < expanded.len() && is_ws(expanded[j].0) {
                j += 1;
            }
            norm_chars.push((' ', orig_off));
            i = j;
            continue;
        }
        norm_chars.push((c, orig_off));
        i += 1;
    }

    // 第三步:构建 String 与 UTF-16 映射(每个 UTF-16 码元都带原文偏移)。
    let mut normalized = String::with_capacity(norm_chars.len());
    let mut utf16_map: Vec<usize> = Vec::with_capacity(norm_chars.len());
    for (c, orig_off) in norm_chars {
        let mut buf = [0u16; 2];
        for u in c.encode_utf16(&mut buf).iter() {
            normalized.push_str(&String::from_utf16_lossy(&[*u]));
            utf16_map.push(orig_off);
        }
    }
    NormalizedText {
        normalized,
        utf16_map,
    }
}

/// 便捷:规范化文本的 SHA-256(用于按需块 ID 与匹配辅助值)。
pub fn normalized_hash(text: &str) -> String {
    sha256_str(&normalize_v1(text).normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_whitespace_and_maps_offsets() {
        let n = normalize_v1("Hello   world");
        assert_eq!(n.normalized, "Hello world");
        // "world" 首字符在原文 UTF-16 偏移 8 处(三个空格中的第一个被保留)。
        let w = n.normalized.find("world").unwrap();
        assert_eq!(n.utf16_map[w], 8);
    }

    #[test]
    fn merges_hyphenated_linebreak() {
        let n = normalize_v1("gebo-\r\nterd");
        assert_eq!(n.normalized, "geboterd");
    }

    #[test]
    fn strips_soft_hyphen() {
        let n = normalize_v1("Sy\u{00AD}stem");
        assert_eq!(n.normalized, "System");
    }

    #[test]
    fn nfkc_normalizes_fullwidth() {
        // 全角字母 NFKC → 半角。
        let n = normalize_v1("ＡＢC");
        assert_eq!(n.normalized, "ABC");
    }

    #[test]
    fn finds_original_range_across_normalization() {
        let original = "第一章  内容。\n第二段";
        let hay = normalize_v1(original);
        let needle = normalize_v1("内容。 第二段"); // 规范化后与原文换行版本对齐
        let (start, end) = hay.find_original_range(&needle).expect("normalized match");
        let units: Vec<u16> = original.encode_utf16().collect();
        assert!(
            start < end && end <= units.len(),
            "range ({start},{end}) within {0}",
            units.len()
        );
        // 映射回去的起点必须指向原文中的字符边界且内容一致。
        let restored: String = String::from_utf16_lossy(&units[start..end]);
        assert_eq!(normalize_v1(&restored).normalized, needle.normalized);
    }

    #[test]
    fn find_original_range_misses_cleanly() {
        let hay = normalize_v1("完全不同的文本");
        let needle = normalize_v1("不存在");
        assert!(hay.find_original_range(&needle).is_none());
    }

    #[test]
    fn normalized_hash_is_stable() {
        assert_eq!(normalized_hash("A  B"), normalized_hash("A B"));
        assert_ne!(normalized_hash("A B"), normalized_hash("AB"));
    }

    #[test]
    fn map_len_matches_utf16_len() {
        for s in ["中文测试", "emoji 🎉 mixed", "", "a\u{00AD}b c"] {
            let n = normalize_v1(s);
            assert_eq!(
                n.utf16_map.len(),
                n.normalized.encode_utf16().count(),
                "input: {s:?}"
            );
        }
    }
}
