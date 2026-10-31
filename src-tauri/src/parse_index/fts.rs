//! FTS5 安全查询构造(《01》§8.1、《M04》§9)。
//!
//! 用户输入永远不进入查询语法:关键词统一转义为 FTS 字符串字面量,
//! `AND`/`OR`/`NEAR`/引号/星号不具备注入能力(M04-A6)。
//! trigram 基线要求每词 ≥3 字符;更短的词走代次内 LIKE 降级。

/// 单个关键词 → FTS5 字符串字面量(双引号包裹,内部引号加倍)。
fn quote_phrase(keyword: &str) -> String {
    format!("\"{}\"", keyword.replace('"', "\"\""))
}

/// 关键词集合 → MATCH 表达式(隐式 AND 的短语拼接)。
/// 任一关键词含通配/逻辑符号都被字面量化;空集合返回 None。
pub fn build_match_expression(keywords: &[String]) -> Option<String> {
    let parts: Vec<String> = keywords
        .iter()
        .filter(|k| !k.trim().is_empty())
        .map(|k| quote_phrase(k.trim()))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// 放宽表达式(OR):长自然语言问题往往没有任何单块包含全部关键词。
/// 仅在 AND 零命中时使用,仍对每个词做字面量化。
pub fn build_match_expression_relaxed(keywords: &[String]) -> Option<String> {
    let parts: Vec<String> = keywords
        .iter()
        .filter(|k| !k.trim().is_empty())
        .map(|k| quote_phrase(k.trim()))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" OR "))
    }
}

/// trigram 最短长度:低于该值 FTS 无法命中,需 LIKE 降级。
pub const TRIGRAM_MIN_CHARS: usize = 3;

/// 是否需要 LIKE 降级(存在 <3 字符的关键词)。
pub fn needs_like_fallback(keywords: &[String]) -> bool {
    keywords
        .iter()
        .any(|k| k.trim().chars().count() < TRIGRAM_MIN_CHARS)
}

/// LIKE 模式:%kw1% OR 语义由调用方逐词执行;此处生成单个词的转义模式。
/// `%`、`_`、`\` 全部字面量化。
pub fn like_pattern(keyword: &str) -> String {
    let mut out = String::with_capacity(keyword.len() + 2);
    out.push('%');
    for c in keyword.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_operators_are_neutralized() {
        // M04-A6:用户输入中的语法符号必须失效,不抛 FTS 语法错误。
        let expr = build_match_expression(&["AND\"OR".to_string(), "a*b".to_string()]).unwrap();
        assert_eq!(expr, "\"AND\"\"OR\" \"a*b\"");
        // 表达式本身是两个字符串字面量,可被 FTS5 接受。
        assert!(expr.starts_with('"') && expr.ends_with('"'));
    }

    #[test]
    fn empty_keywords_produce_no_expression() {
        assert!(build_match_expression(&[]).is_none());
        assert!(build_match_expression(&["  ".to_string()]).is_none());
    }

    #[test]
    fn multiple_keywords_join_with_implicit_and() {
        let expr = build_match_expression(&["知识".to_string(), "图谱".to_string()]).unwrap();
        assert_eq!(expr, "\"知识\" \"图谱\"");
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("50%_"), "%50\\%\\_%");
        assert_eq!(like_pattern("a\\b"), "%a\\\\b%");
    }

    #[test]
    fn short_keywords_flag_fallback() {
        // trigram 按字符计:<3 字符无法命中,必须降级。
        assert!(needs_like_fallback(&["中".to_string(), "文档".to_string()]));
        assert!(needs_like_fallback(&[
            "文档".to_string(),
            "abc".to_string()
        ]));
        assert!(!needs_like_fallback(&[
            "文档问答".to_string(),
            "abc".to_string()
        ]));
    }
}
