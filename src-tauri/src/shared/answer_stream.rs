//! 结构化答案增量提取(M00):从流式 `answer.v1` / `agent.v1` JSON 中
//! 提取 `segments[].text`,产出可显示的正文增量。
//!
//! 约束:
//! - 只提取 `segments` 数组内 `"text"` 键的字符串值;原始 JSON 永不展示。
//! - 未闭合的字符串/转义序列不会被提前输出;解析失败由调用方发
//!   `answer.reset`,最终以数据库中的校验答案为准。
//! - 该提取器只做“临时展示”,不做语义校验。

/// 提取器在流结束时的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerStreamFinish {
    /// 增量展示与最终答案一致,无需重置。
    Settled,
    /// 增量展示与最终答案不一致/最终 JSON 不可解析:调用方应发重置事件。
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    SeekSegments,
    InArray { depth: usize },
    Done,
}

#[derive(Debug, Clone, Copy)]
struct ActiveText {
    /// 字符串左引号在 buffer 中的位置。
    value_start: usize,
    /// 已输出的解码后字符数。
    emitted_chars: usize,
}

/// 增量提取器:可跨任意大小的 chunk 调用。
pub struct IncrementalAnswerExtractor {
    buffer: Vec<u8>,
    pos: usize,
    phase: Phase,
    active: Option<ActiveText>,
    emitted: String,
}

impl Default for IncrementalAnswerExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalAnswerExtractor {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            pos: 0,
            phase: Phase::SeekSegments,
            active: None,
            emitted: String::new(),
        }
    }

    /// 追加一段模型原文,返回本次新可显示的文本片段(可能为空)。
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        if chunk.is_empty() {
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk.as_bytes());
        self.advance()
    }

    /// 已累计输出的正文(用于与最终答案比较)。
    pub fn emitted_text(&self) -> &str {
        &self.emitted
    }

    pub fn has_emitted(&self) -> bool {
        !self.emitted.is_empty()
    }

    /// 流结束:用最终 JSON 校验增量展示是否完整一致。
    ///
    /// 最终答案仍以调用方解析/校验后的内容为准;这里只决定是否需要
    /// 通知 UI 清空临时草稿。
    pub fn finish(&self, final_json: &str) -> AnswerStreamFinish {
        let Some(final_text) = final_segment_text(final_json) else {
            return if self.has_emitted() {
                AnswerStreamFinish::Reset
            } else {
                AnswerStreamFinish::Settled
            };
        };
        if final_text == self.emitted {
            AnswerStreamFinish::Settled
        } else {
            AnswerStreamFinish::Reset
        }
    }

    fn advance(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            match self.phase {
                Phase::Done => return out,
                Phase::SeekSegments => {
                    let Some(start) = find_segments_array_start(&self.buffer) else {
                        // 防止异常流无限增长;保留尾部以覆盖跨 chunk 的键。
                        if self.buffer.len() > 1_048_576 {
                            let keep = 64;
                            let len = self.buffer.len();
                            self.buffer.drain(..len - keep);
                            self.pos = 0;
                        }
                        return out;
                    };
                    self.pos = start;
                    self.phase = Phase::InArray { depth: 1 };
                }
                Phase::InArray { mut depth } => {
                    if let Some(active) = self.active {
                        let Some((suffix, consumed, complete)) = decode_available(
                            &self.buffer,
                            active.value_start,
                            active.emitted_chars,
                        ) else {
                            return out;
                        };
                        let suffix_chars = suffix.chars().count();
                        if !suffix.is_empty() {
                            self.emitted.push_str(&suffix);
                            out.push(suffix);
                        }
                        let emitted_chars = active.emitted_chars + suffix_chars;
                        if complete {
                            self.active = None;
                            self.pos = active.value_start + consumed;
                        } else {
                            self.active = Some(ActiveText {
                                value_start: active.value_start,
                                emitted_chars,
                            });
                            return out;
                        }
                        continue;
                    }

                    let mut pending = false;
                    while self.pos < self.buffer.len() {
                        match self.buffer[self.pos] {
                            b'"' => {
                                let Some(end) = find_string_end(&self.buffer, self.pos) else {
                                    pending = true;
                                    break;
                                };
                                // 字符串结束后紧跟 `:` 才是键;否则是值,跳过。
                                // 关节点(都是实测踩到的):
                                // - 切块常把下一位截断,`next_non_ws` 返回 None 时必须等
                                //   下一个 chunk,不能把 `"text"` 当值跳过,否则整段永不输出;
                                // - 跳过值后 pos 只能推进到 `end`(右引号之后),多推一位会错位。
                                let Some(after) = next_non_ws(&self.buffer, end) else {
                                    pending = true;
                                    break;
                                };
                                if self.buffer[after] != b':' {
                                    self.pos = end;
                                    continue;
                                }
                                if json_string_is(&self.buffer[self.pos..end], "text") {
                                    let Some(value_start) = next_non_ws(&self.buffer, after + 1)
                                        .filter(|index| {
                                            *index < self.buffer.len()
                                                && self.buffer[*index] == b'"'
                                        })
                                    else {
                                        // 值引号还没到:回到这个键的开引号重新扫描,
                                        // 下一个 chunk 到来时会再次识别到 `"text"`。
                                        // (停在 end 会丢掉键名,整段文本永远不输出。)
                                        pending = true;
                                        break;
                                    };
                                    self.active = Some(ActiveText {
                                        value_start,
                                        emitted_chars: 0,
                                    });
                                    self.pos = value_start;
                                    break;
                                }
                                // 其它键(`"kind"`、`"id"` 等):跳过键名,循环继续扫过
                                // `:` 与其值。
                                self.pos = end;
                            }
                            b'[' => {
                                depth += 1;
                                self.pos += 1;
                            }
                            b']' => {
                                depth = depth.saturating_sub(1);
                                self.pos += 1;
                                if depth == 0 {
                                    self.phase = Phase::Done;
                                    break;
                                }
                            }
                            _ => self.pos += 1,
                        }
                    }
                    if matches!(self.phase, Phase::Done) {
                        continue;
                    }
                    if pending || self.pos >= self.buffer.len() {
                        // 数据不足:等待下一个 chunk(不退出外层 loop 的其它分支,
                        // 但本阶段确实无更多可做)。
                        self.phase = Phase::InArray { depth };
                        return out;
                    }
                    self.phase = Phase::InArray { depth };
                }
            }
        }
    }
}

/// 定位 `"segments"` 键后的数组左括号之后的位置。
fn find_segments_array_start(buffer: &[u8]) -> Option<usize> {
    let key = b"\"segments\"";
    let mut from = 0;
    while let Some(relative) = find_subslice(&buffer[from..], key) {
        let key_start = from + relative;
        let after_key = key_start + key.len();
        let Some(colon) = next_non_ws(buffer, after_key).filter(|i| buffer[*i] == b':') else {
            from = after_key;
            continue;
        };
        let Some(bracket) = next_non_ws(buffer, colon + 1).filter(|i| buffer[*i] == b'[') else {
            from = after_key;
            continue;
        };
        return Some(bracket + 1);
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn next_non_ws(buffer: &[u8], from: usize) -> Option<usize> {
    let mut index = from;
    while index < buffer.len() {
        if !buffer[index].is_ascii_whitespace() {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// 返回字符串结束后的位置(含右引号)。
fn find_string_end(buffer: &[u8], start: usize) -> Option<usize> {
    debug_assert_eq!(buffer.get(start), Some(&b'"'));
    let mut index = start + 1;
    while index < buffer.len() {
        match buffer[index] {
            b'\\' => {
                index += 2;
            }
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

fn json_string_is(slice_including_quotes: &[u8], expected: &str) -> bool {
    // `find_string_end` 返回的是**右引号之后**的位置;直接整段做 `from_slice` 会
    // 多出一个字节的解释失败,于是所有键都被判成"不是 text"。
    // 这里按"含首引号、不含尾引号的位置"取内容比较,不依赖 serde 的尾部严格性。
    let Some(rest) = slice_including_quotes.strip_prefix(b"\"") else {
        return false;
    };
    let body = rest
        .strip_suffix(b"\"")
        .or_else(|| rest.get(..rest.len().saturating_sub(1)))
        .unwrap_or(rest);
    match serde_json::from_slice::<String>(&{
        let mut quoted = Vec::with_capacity(body.len() + 2);
        quoted.push(b'"');
        quoted.extend_from_slice(body);
        quoted.push(b'"');
        quoted
    }) {
        Ok(value) => value == expected,
        Err(_) => false,
    }
}

/// 解码从 `value_start` 开始的字符串中“当前已安全可用”的部分。
/// 返回 (新增解码文本, 消耗到的字节位置(相对 value_start), 是否已闭合)。
fn decode_available(
    buffer: &[u8],
    value_start: usize,
    already_emitted_chars: usize,
) -> Option<(String, usize, bool)> {
    if buffer.get(value_start) != Some(&b'"') {
        return None;
    }
    let mut index = value_start + 1;
    let mut safe_end: Option<usize> = None;
    let mut complete = false;
    while index < buffer.len() {
        match buffer[index] {
            b'"' => {
                safe_end = Some(index);
                complete = true;
                index += 1;
                break;
            }
            b'\\' => {
                let Some(next) = buffer.get(index + 1).copied() else {
                    break;
                };
                if next == b'u' {
                    if index + 5 >= buffer.len() {
                        break;
                    }
                    index += 6;
                } else {
                    index += 2;
                }
            }
            _ => index += 1,
        }
    }
    let safe_end = safe_end.unwrap_or(index.min(buffer.len()));
    // 缓冲尾部可能落在一个多字节 UTF-8 字符中间(中文 3 字节/字,而 provider
    // 常按 3–8 字符切块):此时整段解码会失败,若直接放弃,中文流会一直
    // 等到字符串闭合才输出——实测整轮 384 个增量、0 段可见文本。
    // 回退到最后一个完整字符边界,已完整的部分照常增量输出。
    let safe_end = utf8_safe_end(buffer, value_start + 1, safe_end);
    if safe_end <= value_start + 1 {
        return Some((String::new(), index - value_start, complete));
    }
    // 构造带引号的 JSON 字符串以复用 serde 的转义解码。
    let mut quoted = Vec::with_capacity(safe_end - value_start + 2);
    quoted.push(b'"');
    quoted.extend_from_slice(&buffer[value_start + 1..safe_end]);
    quoted.push(b'"');
    let decoded = serde_json::from_slice::<String>(&quoted).ok()?;
    let suffix: String = decoded.chars().skip(already_emitted_chars).collect();
    Some((suffix, index - value_start, complete))
}

/// 把 `[from, end)` 截到"能被 UTF-8 正确解码"的最长前缀,返回其结束位置。
/// 缓冲尾部常落在多字节字符中间(中文 3 字节/字,provider 按 3–8 字符切块),
/// 直接解码会失败;回退到最后一个完整字符边界,已完整部分照常增量输出。
fn utf8_safe_end(buffer: &[u8], from: usize, end: usize) -> usize {
    let mut cut = end.min(buffer.len());
    let mut back = 0;
    while cut > from && back < 4 {
        match std::str::from_utf8(&buffer[from..cut]) {
            Ok(_) => return cut,
            Err(err) => {
                // 只在"尾部不完整"时回退;中途非法说明不是切块问题,原样返回。
                if err.valid_up_to() + err.error_len().unwrap_or(1) < buffer[from..cut].len() {
                    return cut;
                }
                cut -= 1;
                back += 1;
            }
        }
    }
    cut
}

/// 从最终 JSON 读取 `segments[].text`(兼容 `answer.v1` 与 `agent.v1`)。
fn final_segment_text(final_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(final_json).ok()?;
    let segments = value.get("segments").or_else(|| {
        value
            .get("answer")
            .and_then(|answer| answer.get("segments"))
    })?;
    let array = segments.as_array()?;
    let mut text = String::new();
    for segment in array {
        if let Some(part) = segment.get("text").and_then(|v| v.as_str()) {
            text.push_str(part);
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(chunks: &[&str]) -> (IncrementalAnswerExtractor, String) {
        let mut extractor = IncrementalAnswerExtractor::new();
        let mut emitted = String::new();
        for chunk in chunks {
            for part in extractor.push(chunk) {
                emitted.push_str(&part);
            }
        }
        (extractor, emitted)
    }

    /// 回归:provider 按 3 字符切块时(中文 3 字节/字,切点常落在多字节字符与
    /// JSON 结构中间),提取器必须持续增量输出,而不是等整段闭合后一次性吐出。
    /// 旧实现在此用例下整轮 0 输出(实测 384 个增量、0 段可见文本)。
    #[test]
    fn emits_incrementally_with_tiny_multibyte_chunks() {
        let json = r#"{"schemaVersion":"answer.v1","status":"answered","segments":[{"id":"s1","kind":"document_fact","text":"本地与远程模型的分工是这样安排的","verificationStatus":"unverified","citationIds":[]}],"citations":[]}"#;
        let chars: Vec<char> = json.chars().collect();
        let mut extractor = IncrementalAnswerExtractor::new();
        let mut emitted = String::new();
        let mut emissions = 0usize;
        let mut i = 0;
        while i < chars.len() {
            let end = (i + 3).min(chars.len());
            let piece: String = chars[i..end].iter().collect();
            for part in extractor.push(&piece) {
                emissions += 1;
                emitted.push_str(&part);
            }
            i = end;
        }
        assert_eq!(emitted, "本地与远程模型的分工是这样安排的");
        assert!(emissions > 5, "增量输出次数过少: {emissions}(整段一次吐出)");
        assert!(!emitted.contains("schemaVersion"));
    }

    #[test]
    fn extracts_segments_incrementally_without_raw_json() {
        let chunks = [
            r#"{"schemaVersion":"answer.v1","status":"answered","segments":[{"id":"s1","kind":"#,
            r#""document_fact","text":"第一段"#,
            r#"，继续","verificationStatus":"unverified","citationIds":[]},{"id":"s2","text":"第二段"#,
            r#""}]}"#,
        ];
        let (extractor, emitted) = feed(&chunks);
        assert_eq!(emitted, "第一段，继续第二段");
        assert!(!emitted.contains("schemaVersion"));
        assert_eq!(
            extractor.finish(r#"{"schemaVersion":"answer.v1","segments":[{"id":"s1","text":"第一段，继续"},{"id":"s2","text":"第二段"}]}"#),
            AnswerStreamFinish::Settled
        );
    }

    #[test]
    fn handles_escapes_and_unicode_split_across_chunks() {
        let chunks = [
            r#"{"segments":[{"text":"line1\nline2 \"quote\" \u4e2d"#,
            r#"文"}]}"#,
        ];
        let (_, emitted) = feed(&chunks);
        assert_eq!(emitted, "line1\nline2 \"quote\" 中文");
    }

    #[test]
    fn does_not_emit_unclosed_escape() {
        let (mut extractor, first) = feed(&[r#"{"segments":[{"text":"abc\u4e"#]);
        assert_eq!(first, "abc");
        let mut emitted = first;
        for part in extractor.push(r#"2d"}]}"#) {
            emitted.push_str(&part);
        }
        assert_eq!(emitted, "abc中");
    }

    #[test]
    fn nested_agent_answer_segments_are_supported() {
        let (extractor, emitted) = feed(&[
            r#"{"schemaVersion":"agent.v1","answer":{"segments":[{"text":"代理回答"}]},"steps":[]}"#,
        ]);
        assert_eq!(emitted, "代理回答");
        assert_eq!(
            extractor.finish(r#"{"schemaVersion":"agent.v1","answer":{"segments":[{"text":"代理回答"}]},"steps":[]}"#),
            AnswerStreamFinish::Settled
        );
    }

    #[test]
    fn final_mismatch_or_invalid_json_requests_reset() {
        let (extractor, _) = feed(&[r#"{"segments":[{"text":"半截"}]}"#]);
        assert_eq!(
            extractor.finish(r#"{"segments":[{"text":"完整答案"}]}"#),
            AnswerStreamFinish::Reset
        );
        assert_eq!(extractor.finish("not-json"), AnswerStreamFinish::Reset);
    }

    #[test]
    fn no_segments_key_stays_silent() {
        let (extractor, emitted) = feed(&[r#"{"foo":"bar"}"#]);
        assert!(emitted.is_empty());
        assert_eq!(
            extractor.finish(r#"{"foo":"bar"}"#),
            AnswerStreamFinish::Settled
        );
    }

    #[test]
    fn never_emits_the_text_key_name_or_other_fields() {
        let (_, emitted) = feed(&[
            r#"{"schemaVersion":"answer.v1","status":"answered","suggestions":["text"],"segments":[{"#,
            r#""text":"正文","citationIds":["text"]}]}"#,
        ]);
        assert_eq!(emitted, "正文");
    }
}
