//! M09 查询处理(《M09》§4)。
//!
//! 用户问题 → 受控关键词集合。NFKC + 空白规范化后提取:
//! 中文连续双字/三字候选、英文词元、数字、比较符与否定词。
//! 停用词仅限明确无检索价值词;“不/未/无/冲突/数字”绝不删除。

use crate::shared::normalize_v1;

/// 停用词白名单:仅收录明确无检索价值的中文疑问助词与英文功能词。
/// 疑问词是否进入查询由 `searchConfigVersion` 冻结,本版本不含疑问词。
const STOPWORDS_ZH: &[&str] = &[
    "的",
    "了",
    "是",
    "在",
    "我",
    "你",
    "他",
    "她",
    "它",
    "们",
    "这",
    "那",
    "什么",
    "哪些",
    "怎么",
    "怎样",
    "如何",
    "为什么",
    "请问",
    "告诉",
    "帮我",
    "一下",
    "请",
];

const STOPWORDS_EN: &[&str] = &[
    "the", "a", "an", "of", "to", "and", "or", "is", "are", "was", "were", "be", "been", "do",
    "does", "did", "what", "which", "how", "why", "who", "whom", "when", "where", "please", "me",
    "my", "your", "tell", "about",
];

/// 否定词与冲突词:高信息量,必须保留(《M09》§4)。
const NEGATIONS: &[&str] = &[
    "不", "未", "无", "没有", "不是", "not", "no", "never", "without",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedQuery {
    /// 去重后的受控关键词,按出现顺序。
    pub keywords: Vec<String>,
    /// 关键词中是否含否定词(检索与答案状态判断都要感知)。
    pub has_negation: bool,
    /// 是否存在数字/年份类词(数字完全匹配可加权)。
    pub has_number: bool,
    /// 规范化后的完整查询文本(仅内存使用,不落普通日志)。
    pub normalized_question: String,
    /// 向量检索文本:发生多轮改写时包含上一轮问题,否则与当前问题一致。
    pub embedding_text: String,
    /// 是否因指代/过短而结合了上一轮问题扩展检索词。
    pub rewritten_from_previous: bool,
}

fn is_chinese(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

fn is_digit_word(s: &str) -> bool {
    !s.is_empty() && s.chars().any(|c| c.is_ascii_digit())
}

fn is_negation(s: &str) -> bool {
    NEGATIONS.contains(&s)
}

fn is_stopword(s: &str) -> bool {
    if s.chars().all(is_chinese) {
        STOPWORDS_ZH.contains(&s)
    } else {
        let lower = s.to_ascii_lowercase();
        STOPWORDS_EN.contains(&lower.as_str())
    }
}

/// 英文词元切分:字母数字与比较符/百分号连续段。
/// 用户输入的 FTS 操作符、括号、星号永远按普通字符处理,不解析语法。
fn tokenize_ascii(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_has_word = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '%' | '.' | '+' | '-' | '/' | ':') {
            cur.push(c);
            if c.is_ascii_alphanumeric() {
                cur_has_word = true;
            }
        } else {
            if cur_has_word && !cur.is_empty() {
                out.push(cur.clone());
            }
            cur.clear();
            cur_has_word = false;
        }
    }
    if cur_has_word {
        out.push(cur);
    }
    out
}

/// 中文连续片段 → 双字候选 + 独立否定词(trigram 语料由 FTS 索引兜底)。
/// 否定字必须作为独立词保留,不得被并入相邻 bigram 后丢失(《M09》§4)。
fn chinese_bigrams(seg: &str) -> Vec<String> {
    let chars: Vec<char> = seg.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    if chars.len() == 1 {
        // 单字:trigram 不支持,交由 LIKE 降级路径。
        out.push(chars[0].to_string());
        return out;
    }
    for i in 0..chars.len() - 1 {
        out.push(chars[i..i + 2].iter().collect::<String>());
    }
    // 否定字独立成词:保证 has_negation 与否定检索可用。
    for c in &chars {
        let one = c.to_string();
        if is_negation(&one) {
            out.push(one);
        }
    }
    out
}

/// 查询文本解析。输入应为用户问题原文;长度上限防御性截断。
pub fn parse_query(question: &str) -> ParsedQuery {
    let norm = normalize_v1(question);
    let text = norm.normalized.trim();
    if text.is_empty() {
        return ParsedQuery {
            keywords: Vec::new(),
            has_negation: false,
            has_number: false,
            normalized_question: String::new(),
            embedding_text: String::new(),
            rewritten_from_previous: false,
        };
    }

    // 中英混合切分:连续中文段与连续 ASCII 段分别处理。
    let mut raw_terms: Vec<String> = Vec::new();
    let mut zh_run = String::new();
    let mut ascii_run = String::new();
    let flush_zh = |zh: &mut String, out: &mut Vec<String>| {
        if !zh.is_empty() {
            out.extend(chinese_bigrams(zh));
            zh.clear();
        }
    };
    for c in text.chars() {
        if is_chinese(c) || matches!(c, '。' | '，' | '、' | '！' | '？') {
            if !ascii_run.is_empty() {
                raw_terms.extend(tokenize_ascii(&ascii_run));
                ascii_run.clear();
            }
            if is_chinese(c) {
                zh_run.push(c);
            } else {
                flush_zh(&mut zh_run, &mut raw_terms);
            }
        } else {
            flush_zh(&mut zh_run, &mut raw_terms);
            ascii_run.push(c);
        }
    }
    flush_zh(&mut zh_run, &mut raw_terms);
    if !ascii_run.is_empty() {
        raw_terms.extend(tokenize_ascii(&ascii_run));
    }

    // 停用词过滤(否定词/数字词优先豁免),去重保序。
    let mut keywords: Vec<String> = Vec::new();
    let mut has_negation = false;
    let mut has_number = false;
    for term in raw_terms {
        let term = term.trim();
        if term.is_empty() {
            continue;
        }
        if is_negation(term) {
            has_negation = true;
        }
        if is_digit_word(term) {
            has_number = true;
        }
        if is_stopword(term) && !is_negation(term) && !is_digit_word(term) {
            continue;
        }
        if !keywords.iter().any(|k| k == term) {
            keywords.push(term.to_string());
        }
    }
    // 关键词数量上限:防止超长问题产生无界 FTS 表达式。
    keywords.truncate(16);

    ParsedQuery {
        keywords,
        has_negation,
        has_number,
        normalized_question: text.to_string(),
        embedding_text: text.to_string(),
        rewritten_from_previous: false,
    }
}

/// 指代/承接标记:出现时允许用上一轮问题补足检索词(仅扩检索,不扩授权)。
const CONTINUATION_ZH: &[&str] = &[
    "它们", "这个", "那个", "这些", "那些", "上述", "前面", "刚才", "接着", "继续", "再说", "还有",
];
const CONTINUATION_EN: &[&str] = &[
    "it", "its", "they", "them", "this", "that", "these", "those", "above", "previous", "continue",
    "more",
];

fn has_continuation_marker(question: &str) -> bool {
    let lowered = normalize_v1(question).normalized.to_lowercase();
    if CONTINUATION_ZH.iter().any(|m| lowered.contains(m)) {
        return true;
    }
    // 单字指代只在短问题里视为承接,避免"其他/因此"等普通词误触发。
    if lowered.chars().count() <= 12
        && (lowered.contains('它') || lowered.contains('该') || lowered.contains('此'))
    {
        return true;
    }
    tokenize_ascii(&lowered)
        .iter()
        .any(|token| CONTINUATION_EN.contains(&token.as_str()))
}

/// 多轮查询改写:当前问题带有指代/承接,或关键词过少时,把上一轮问题的
/// 检索词补进来。改写只影响检索关键词与向量文本,**不改变答案问题文本**,
/// 也不改变快照/授权范围。
pub fn parse_query_with_previous(question: &str, previous_question: Option<&str>) -> ParsedQuery {
    let mut current = parse_query(question);
    let Some(previous) = previous_question.map(str::trim).filter(|p| !p.is_empty()) else {
        return current;
    };
    let previous_query = parse_query(previous);
    if previous_query.keywords.is_empty() {
        return current;
    }
    if !current.keywords.is_empty()
        && current.keywords.len() > 2
        && !has_continuation_marker(question)
    {
        return current;
    }
    let mut inherited = 0usize;
    for keyword in previous_query.keywords {
        if current.keywords.len() >= 16 {
            break;
        }
        if !current.keywords.iter().any(|k| k == &keyword) {
            current.keywords.push(keyword);
            inherited += 1;
        }
    }
    if inherited == 0 {
        return current;
    }
    current.has_negation |= previous_query.has_negation;
    current.has_number |= previous_query.has_number;
    current.rewritten_from_previous = true;
    current.embedding_text = format!("{} {}", previous, current.normalized_question);
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_question_yields_bigram_keywords() {
        let q = parse_query("文件中的预算阈值是多少?");
        assert!(!q.keywords.is_empty());
        // 中文全部转为双字词,无整句进入查询。
        assert!(q
            .keywords
            .iter()
            .all(|k| k.chars().count() <= 2 || !k.chars().all(is_chinese)));
        assert!(q.has_number || q.keywords.iter().any(|k| k.contains('阈')));
    }

    #[test]
    fn negation_and_numbers_survive_stopwords() {
        // “不/未/无/数字”不得作为停用词删除(M09-A3 语义前提)。
        let q = parse_query("2024 年未披露的收入是多少");
        assert!(q.has_negation);
        assert!(q.has_number);
        assert!(q.keywords.iter().any(|k| k.contains("未")));
        assert!(q.keywords.iter().any(|k| k.contains("2024")));
    }

    #[test]
    fn english_tokens_and_operators_are_plain_text() {
        let q = parse_query("What is the revenue AND \"OR\" (a*b)?");
        assert!(q.keywords.iter().any(|k| k.eq_ignore_ascii_case("revenue")));
        // 操作符/括号/星号全部消失为普通词元:关键词中不存在任何
        // FTS 语法字符(注入被字面量化)。
        assert!(q
            .keywords
            .iter()
            .all(|k| !k.chars().any(|c| "\"()*".contains(c))));
    }

    #[test]
    fn stopwords_and_question_words_are_removed() {
        let q = parse_query("这是什么 the of 是");
        assert_eq!(q.keywords, ["这是", "是什"]);
    }

    #[test]
    fn empty_and_huge_queries_are_bounded() {
        assert!(parse_query("   ").keywords.is_empty());
        let long = "词".repeat(500);
        let q = parse_query(&long);
        assert!(q.keywords.len() <= 16);
    }

    #[test]
    fn short_followup_inherits_previous_terms_for_retrieval_only() {
        let q = parse_query_with_previous("那它呢?", Some("预算阈值是多少?"));
        assert!(q.rewritten_from_previous);
        assert!(q.keywords.iter().any(|k| k.contains("预算")));
        // 答案问题仍是当前问题;上一轮只扩检索/向量文本。
        assert_eq!(q.normalized_question, "那它呢?");
        assert!(q.embedding_text.contains("预算阈值是多少?"));
        assert!(q.embedding_text.contains("那它呢?"));
    }

    #[test]
    fn standalone_question_does_not_inherit_previous_terms() {
        let q = parse_query_with_previous("风险准备金怎么计提?", Some("预算阈值是多少?"));
        assert!(!q.rewritten_from_previous);
        assert_eq!(q.embedding_text, q.normalized_question);
        assert!(!q.keywords.iter().any(|k| k.contains("预算")));
    }

    #[test]
    fn english_followup_uses_previous_question_without_changing_scope() {
        let q = parse_query_with_previous("What about it?", Some("What is the revenue?"));
        assert!(q.rewritten_from_previous);
        assert!(q.keywords.iter().any(|k| k.eq_ignore_ascii_case("revenue")));
        assert_eq!(q.normalized_question, "What about it?");
    }
}
