//! 生成物统一清洗(用户可见正文的唯一出口)。
//!
//! 模型经常把内部标记写进正文:`[SOURCE <uuid>]`、`[SOURCE id | role]`、
//! 字面 `\n`、HTML 实体(`&amp;`/`&quot;`)、JSON 信封字段。这些都属于
//! “内部协议”,不得展示给用户。本模块把它们统一收敛为**可渲染的 Markdown**:
//!
//! - 保留正常方括号内容(`[1]`、`[注意]`),只剥离 `SOURCE` 标记;
//! - 代码围栏内不做转义还原(代码里的 `\n` 是真实字符,不能动);
//! - 不猜测语义:不删除普通 UUID(文档本身可能含 UUID),只删除紧邻
//!   `SOURCE` 或成对方括号包裹的内部 ID;
//! - 幂等:对已清洗文本再调用一次结果不变(单测保证)。

/// 清洗一条用户可见正文(答案段/摘要段/题干/卡片/译文)。
///
/// 先确认协议结构再解包;普通 JSON 示例不能因含 text 字段而丢失上下文。
/// 正文解码后剥内部标记,代码、数学、路径与普通 JSON 示例保持原样。
pub fn sanitize_generated_markdown(text: &str) -> String {
    let mut current = text.to_string();
    let mut protocol = false;
    loop {
        let salvaged = salvage_json_like(&current);
        protocol |= salvaged.is_some();
        let source = salvaged.as_deref().unwrap_or(&current);
        // 每一步重新定位保护区:实体/转义还原后可能出现代码、公式或路径。
        let decoded = map_outside_protected(source, decode_html_entities);
        let restored = map_outside_protected(&decoded, unescape_literal_escapes);
        let stripped = map_outside_protected(&restored, |prose| strip_markers(prose, protocol));
        let normalized = normalize_blank_lines(&stripped);
        if normalized == current {
            return normalized;
        }
        // 转换只消去协议包装/编码/多余空白,直至稳定,避免二次调用继续解码。
        current = normalized;
    }
}

/// 模型输出不可恢复时的可读兜底:优先解包完整 JSON;截断/损坏的
/// 结构化 JSON 则提取其中可读文本字段,绝不把原始 JSON 当正文返回。
///
/// 返回空字符串表示没有可读内容(调用方应显示可见错误/通知,而不是 JSON)。
pub fn salvage_readable_markdown(text: &str) -> String {
    // 与正常出口共用协议判定;已识别但没有正文的协议必须返回空,不能回落到原文。
    sanitize_generated_markdown(text)
}

/// 只剥离内部来源标记(保留其它内容原样)。
///
/// 支持:`[SOURCE <id>]`、`[SOURCE <id> | role]`、`[SOURCE: <id>]`、
/// `[[SOURCE <id>]]`、`(SOURCE <id>)`,大小写不敏感;以及紧随其后的
/// 连续标记(模型常一次写多个)。
pub fn strip_internal_markers(text: &str) -> String {
    map_outside_protected(text, strip_markers_in_prose)
        .trim()
        .to_string()
}

fn strip_markers_in_prose(text: &str) -> String {
    strip_markers(text, false)
}

fn strip_markers(text: &str, protocol: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = find_marker_start(rest) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(mut consumed) = find_marker_close(tail) else {
            // 仅已确认的信封正文允许丢弃截断在末尾的来源 token。
            // 普通正文中的未闭合括号仍原样保留。
            if protocol
                && tail.get(7..).is_some_and(|body| {
                    !body.contains(['\n', '\r', '[', ']', '(', ')']) && marker_id_is_valid(body)
                })
            {
                if out.ends_with('[') {
                    out.pop();
                }
                return out;
            }
            // 一个坏标记不能阻止后面的合法标记被清除。
            out.push_str(&tail[..1]);
            rest = &tail[1..];
            continue;
        };
        if out.ends_with('[') && tail[consumed..].starts_with(']') {
            out.pop();
            consumed += 1;
        }
        rest = &tail[consumed..];
    }
    out.push_str(rest);
    out
}

/// SOURCE 后必须是分隔符,不能把 a[source]、f(source)、[source_map] 当协议。
fn find_marker_start(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    ["[source", "(source"]
        .iter()
        .flat_map(|needle| lower.match_indices(needle))
        .filter(|(pos, needle)| {
            matches!(
                lower.as_bytes().get(pos + needle.len()),
                Some(b' ' | b'\t' | b':')
            )
        })
        .map(|(pos, _)| pos)
        .min()
}

fn marker_id_is_valid(body: &str) -> bool {
    let body = body.trim_start_matches([' ', '\t', ':']);
    let id = body.split('|').next().unwrap_or("").trim();
    let id = id
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(id);
    !id.is_empty()
        && id
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b"_-.:/".contains(&ch))
}

fn find_marker_close(tail: &str) -> Option<usize> {
    let close = match tail.as_bytes().first()? {
        b'[' => ']',
        b'(' => ')',
        _ => return None,
    };
    let end = tail.find(close)?;
    let body = tail.get(7..end)?;
    if body.contains(['\n', '\r', '[', '(', ']']) || !marker_id_is_valid(body) {
        return None;
    }
    Some(end + 1)
}

/// 所有正文变换共用保护范围,不再仅对转义处理保护数学/路径。
fn map_outside_protected(text: &str, transform: impl Fn(&str) -> String) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = protected_span_end(&chars, i) {
            out.push_str(&transform(&chars[from..i].iter().collect::<String>()));
            out.extend(chars[i..end].iter());
            i = end;
            from = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&transform(&chars[from..].iter().collect::<String>()));
    out
}

/// (正文起点,正文终点,围栏终点,是否允许协议 JSON)。闭合符必须同种且不短于开头。
fn fence_span(chars: &[char], start: usize) -> Option<(usize, usize, usize, bool)> {
    let marker = *chars.get(start)?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let line_start = chars[..start]
        .iter()
        .rposition(|ch| *ch == '\n')
        .map_or(0, |i| i + 1);
    if start - line_start > 3 || chars[line_start..start].iter().any(|ch| *ch != ' ') {
        return None;
    }
    let width = chars[start..]
        .iter()
        .take_while(|ch| **ch == marker)
        .count();
    if width < 3 {
        return None;
    }
    let opening_end = chars[start..]
        .iter()
        .position(|ch| *ch == '\n')
        .map_or(chars.len(), |i| start + i);
    let info: String = chars[start + width..opening_end].iter().collect();
    let json = info.trim().is_empty() || info.trim().eq_ignore_ascii_case("json");
    let content_start = (opening_end + 1).min(chars.len());
    let mut line = content_start;
    while line < chars.len() {
        let end = chars[line..]
            .iter()
            .position(|ch| *ch == '\n')
            .map_or(chars.len(), |i| line + i);
        let indent = chars[line..end].iter().take_while(|ch| **ch == ' ').count();
        let count = chars[line + indent..end]
            .iter()
            .take_while(|ch| **ch == marker)
            .count();
        if indent <= 3
            && count >= width
            && chars[line + indent + count..end]
                .iter()
                .all(|ch| ch.is_whitespace())
        {
            return Some((content_start, line, (end + 1).min(chars.len()), json));
        }
        line = (end + 1).min(chars.len());
    }
    Some((content_start, chars.len(), chars.len(), json))
}

/// 还原被双重转义的文本:字面 `\n`、`\t`、`\"`、`\\`。
///
/// 只处理“反斜杠 + 已知转义字符”的组合,其余反斜杠原样保留。
/// 关键约束:数学公式(`$...$`、`$$...$$`、`\(...\)`、`\[...\]`)与
/// Windows 路径整体跳过;正文中的常见 LaTeX 命令也保留,
/// 但不能因转义后是英文正文就拒绝恢复 `\nNext`。
fn unescape_literal_escapes(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        if let Some(end) = protected_span_end(&chars, i) {
            out.extend(chars[i..end].iter());
            i = end;
            continue;
        }
        let ch = chars[i];
        if ch != '\\' {
            out.push(ch);
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        match next {
            Some('n') if !is_latex_command(&chars, i) => {
                out.push('\n');
                i += 2;
            }
            Some('t') if !is_latex_command(&chars, i) => {
                out.push('\t');
                i += 2;
            }
            Some('r') if !is_latex_command(&chars, i) => {
                out.push('\r');
                i += 2;
            }
            Some('"') => {
                out.push('"');
                i += 2;
            }
            Some('\\') => {
                // `\\n` / `\\t` / `\\r` 是 JSON 字符串里的二次转义;
                // 后接字母时按 LaTeX 保留,避免破坏公式。
                match chars.get(i + 2).copied() {
                    Some('n') if !is_latex_command(&chars, i + 1) => {
                        out.push('\n');
                        i += 3;
                    }
                    Some('t') if !is_latex_command(&chars, i + 1) => {
                        out.push('\t');
                        i += 3;
                    }
                    Some('r') if !is_latex_command(&chars, i + 1) => {
                        out.push('\r');
                        i += 3;
                    }
                    Some('"') => {
                        out.push('"');
                        i += 3;
                    }
                    Some('\\') => {
                        out.push('\\');
                        i += 3;
                    }
                    _ => {
                        out.push('\\');
                        i += 2;
                    }
                }
            }
            _ => {
                out.push('\\');
                i += 1;
            }
        }
    }
    out
}

/// 只有 n/t/r 开头的命令与 JSON 空白转义冲突;数学保护区内无需此启发式。
fn is_latex_command(chars: &[char], slash: usize) -> bool {
    let command: String = chars[slash + 1..]
        .iter()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .collect();
    matches!(
        command.as_str(),
        "nabla"
            | "neq"
            | "ne"
            | "neg"
            | "not"
            | "notin"
            | "ni"
            | "nu"
            | "newcommand"
            | "newenvironment"
            | "newline"
            | "nobreak"
            | "nonumber"
            | "times"
            | "theta"
            | "tau"
            | "tan"
            | "tanh"
            | "to"
            | "top"
            | "triangle"
            | "text"
            | "textbf"
            | "textit"
            | "textrm"
            | "textsf"
            | "texttt"
            | "textcolor"
            | "tfrac"
            | "tilde"
            | "tiny"
            | "tag"
            | "right"
            | "rightarrow"
            | "rightleftharpoons"
            | "rangle"
            | "rbrace"
            | "rceil"
            | "rfloor"
            | "rho"
            | "ref"
            | "renewcommand"
            | "rm"
            | "rule"
    )
}

/// 返回代码、数学、路径或普通 JSON 示例的结束位置(不含)。
fn protected_span_end(chars: &[char], start: usize) -> Option<usize> {
    let rest = &chars[start..];
    let starts_with =
        |needle: &[char]| rest.len() >= needle.len() && &rest[..needle.len()] == needle;
    if let Some((_, _, end, _)) = fence_span(chars, start) {
        return Some(end);
    }
    if chars[start] == '`' {
        let width = rest.iter().take_while(|ch| **ch == '`').count();
        let mut i = start + width;
        while i < chars.len() {
            if chars[i] == '`' {
                let count = chars[i..].iter().take_while(|ch| **ch == '`').count();
                if count == width {
                    return Some(i + count);
                }
                i += count;
            } else {
                i += 1;
            }
        }
    }
    if let Some(end) = json_container_end(chars, start) {
        return Some(end);
    }
    // 显示数学 $$...$$ / \[...\] 与行内数学 $...$ / \(...\)。
    if starts_with(&['$', '$']) {
        return find_closing(chars, start + 2, &['$', '$']);
    }
    if starts_with(&['\\', '[']) {
        return find_closing(chars, start + 2, &['\\', ']']);
    }
    if starts_with(&['\\', '(']) {
        return find_closing(chars, start + 2, &['\\', ')']);
    }
    if chars.get(start).copied() == Some('$') {
        return find_closing(chars, start + 1, &['$']);
    }
    // Windows 盘符路径:C:\... ;UNC:\\server\share。
    if start + 2 < chars.len()
        && chars[start].is_ascii_alphabetic()
        && chars[start + 1] == ':'
        && (chars[start + 2] == '\\' || chars[start + 2] == '/')
    {
        return Some(path_span_end(chars, start));
    }
    if starts_with(&['\\', '\\']) {
        let end = path_span_end(chars, start);
        let path = &chars[start + 2..end];
        // UNC 至少要有 server\share,不能把二次转义的 \\n 当成路径。
        if path
            .iter()
            .position(|ch| matches!(ch, '\\' | '/'))
            .is_some_and(|separator| {
                separator > 0
                    && separator + 1 < path.len()
                    && path[..separator]
                        .iter()
                        .all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_'))
                    && !matches!(path[separator + 1], '\\' | '/')
            })
        {
            return Some(end);
        }
    }
    None
}

/// 只定位容器边界,不因普通示例含 text/schemaVersion 字样而认定其为协议。
fn json_container_end(chars: &[char], start: usize) -> Option<usize> {
    if !matches!(chars.get(start), Some('{' | '[')) {
        return None;
    }
    let next = chars[start + 1..]
        .iter()
        .find(|ch| !ch.is_whitespace())
        .copied();
    if chars[start] == '{' && !matches!(next, Some('"' | '}')) {
        return None;
    }
    if chars[start] == '[' && !matches!(next, Some('{' | '"' | ']')) {
        return None;
    }
    let mut depth = 0usize;
    let mut quoted = false;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\\' if quoted => {
                i += 2;
                continue;
            }
            '"' => quoted = !quoted,
            '{' | '[' if !quoted => depth += 1,
            '}' | ']' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    Some(chars.len())
}

fn find_closing(chars: &[char], from: usize, closing: &[char]) -> Option<usize> {
    let mut i = from;
    while i + closing.len() <= chars.len() {
        if &chars[i..i + closing.len()] == closing {
            return Some(i + closing.len());
        }
        i += 1;
    }
    // 未闭合:不做保护,避免吞掉后文。
    None
}

fn path_span_end(chars: &[char], start: usize) -> usize {
    let mut i = start;
    while i < chars.len() {
        if chars[i].is_whitespace() || matches!(chars[i], ')' | ']' | '"' | '\'' | '`') {
            break;
        }
        i += 1;
    }
    i
}

/// 解码常见 HTML 实体(命名实体 + 数字实体)。未知实体原样保留。
fn decode_html_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let Some(end) = tail.find(';').filter(|end| *end <= 10) else {
            // 没有合理的实体结尾:保留 `&` 继续扫描。
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        match decode_entity(entity) {
            Some(decoded) => {
                out.push_str(&decoded);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<String> {
    let named = match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "hellip" => Some('…'),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        "middot" => Some('·'),
        _ => None,
    };
    if let Some(ch) = named {
        return Some(ch.to_string());
    }
    // 数字实体:`#39` / `#x27`
    let number = entity.strip_prefix('#')?;
    let code = if let Some(hex) = number
        .strip_prefix('x')
        .or_else(|| number.strip_prefix('X'))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        number.parse::<u32>().ok()?
    };
    char::from_u32(code).map(|ch| ch.to_string())
}

/// 只规整正文空白;围栏内空行、行尾空格也属于代码,必须保留。
fn normalize_blank_lines(text: &str) -> String {
    map_outside_protected(text, |prose| {
        let mut out = String::with_capacity(prose.len());
        let mut blank_run = 0usize;
        for line in prose.split_inclusive('\n') {
            if !line.ends_with('\n') {
                out.push_str(line);
                continue;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                blank_run += 1;
                if blank_run > 2 {
                    continue;
                }
            } else {
                blank_run = 0;
            }
            out.push_str(trimmed);
            out.push('\n');
        }
        out
    })
    .trim()
    .to_string()
}

/// None = 普通文本/示例;Some("") = 确认是协议,但没有可恢复正文。
/// 容器逐个扫描,跳过 inline code/数学/非 JSON 围栏,不搜索字符串值内部的协议字样。
fn salvage_json_like(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut parts = Vec::new();
    let mut recognized = false;
    let mut i = 0;
    while i < chars.len() {
        if let Some((from, to, end, json)) = fence_span(&chars, i) {
            if json {
                if let Some(readable) =
                    salvage_json_like(&chars[from..to].iter().collect::<String>())
                {
                    recognized = true;
                    if !readable.is_empty() {
                        parts.push(readable);
                    }
                }
            }
            i = end;
            continue;
        }
        if let Some(end) = json_container_end(&chars, i) {
            let mut cursor = i;
            if let Some(value) = read_partial_json_value(&chars[..end], &mut cursor, 0) {
                if is_protocol_value(&value) {
                    recognized = true;
                    if let Some(readable) = readable_text_from_value(&value) {
                        parts.push(readable);
                    }
                }
            }
            i = end;
            continue;
        }
        i = protected_span_end(&chars, i).unwrap_or(i + 1);
    }
    recognized.then(|| parts.join("\n\n"))
}

/// 容错解析:完整 JSON → 代码围栏内 JSON → 首个 `{` 到最后一个 `}` 的片段。
pub(crate) fn parse_json_like(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(value);
    }
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|rest| rest.strip_suffix("```"))
        .map(str::trim);
    if let Some(unfenced) = unfenced {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(unfenced) {
            return Some(value);
        }
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&trimmed[start..=end]).ok()
}

/// 从 JSON 值提取用户可读 Markdown;没有可读字段时返回 None,
/// 以免把任意 JSON(例如代码示例)当成正文展示。
fn readable_text_from_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Some(readable) = salvage_json_like(trimmed) {
                return (!readable.is_empty()).then_some(readable);
            }
            Some(trimmed.to_string())
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(readable_text_from_value).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n\n"))
            }
        }
        serde_json::Value::Object(object) => {
            // 单值字段优先(模型常写 {"answer": "..."} / {"text": "..."})。
            for key in [
                "text",
                "answer",
                "summary",
                "markdown",
                "content",
                "body",
                "conclusion",
                "message",
                "draft",
            ] {
                if let Some(item) = object.get(key) {
                    if let Some(text) = readable_text_from_value(item) {
                        return Some(text);
                    }
                }
            }
            // 摘要/结构信封:sections → 标题 + 段落。
            if let Some(sections) = object.get("sections").and_then(|v| v.as_array()) {
                let mut parts = Vec::new();
                for section in sections {
                    if let Some(title) = section.get("title").and_then(|v| v.as_str()) {
                        let title = title.trim();
                        if !title.is_empty() {
                            parts.push(format!("## {title}"));
                        }
                    }
                    if let Some(segments) = section.get("segments").and_then(|v| v.as_array()) {
                        for segment in segments {
                            if let Some(text) = segment
                                .get("text")
                                .or_else(|| segment.get("content"))
                                .and_then(|v| v.as_str())
                            {
                                let text = text.trim();
                                if !text.is_empty() {
                                    parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
                if !parts.is_empty() {
                    return Some(parts.join("\n\n"));
                }
            }
            // 答案信封:segments[].text。
            if let Some(segments) = object.get("segments").and_then(|v| v.as_array()) {
                let parts: Vec<String> = segments
                    .iter()
                    .filter_map(|segment| {
                        segment
                            .get("text")
                            .or_else(|| segment.get("content"))
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                            .map(str::to_string)
                    })
                    .collect();
                if !parts.is_empty() {
                    return Some(parts.join("\n\n"));
                }
            }
            // 题目信封:题干 + 选项 + 参考答案 + 解析。
            if let Some(questions) = object.get("questions").and_then(|v| v.as_array()) {
                let mut parts = Vec::new();
                for (index, question) in questions.iter().enumerate() {
                    if let Some(prompt) = question.get("prompt").and_then(|v| v.as_str()) {
                        let prompt = prompt.trim();
                        if !prompt.is_empty() {
                            parts.push(format!("{}. {prompt}", index + 1));
                        }
                    }
                    if let Some(options) = question.get("options").and_then(|v| v.as_array()) {
                        for option in options {
                            if let Some(text) = option.get("text").and_then(|v| v.as_str()) {
                                let text = text.trim();
                                if !text.is_empty() {
                                    parts.push(format!("- {text}"));
                                }
                            }
                        }
                    }
                    if let Some(answer) = question.get("answer") {
                        if let Some(text) = answer
                            .get("referenceAnswer")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                        {
                            parts.push(format!("参考答案:{text}"));
                        }
                    }
                }
                if !parts.is_empty() {
                    return Some(parts.join("\n"));
                }
            }
            // 列表字段:按 Markdown 列表拼接,不丢条目。
            for key in [
                "facts",
                "key_points",
                "keyPoints",
                "points",
                "items",
                "bullets",
            ] {
                if let Some(items) = object.get(key).and_then(|v| v.as_array()) {
                    let lines: Vec<String> = items
                        .iter()
                        .filter_map(|item| {
                            item.as_str().map(str::to_string).or_else(|| {
                                item.get("text")
                                    .or_else(|| item.get("content"))
                                    .or_else(|| item.get("claim"))
                                    .and_then(|text| text.as_str())
                                    .map(str::to_string)
                            })
                        })
                        .map(|text| text.trim().to_string())
                        .filter(|text| !text.is_empty())
                        .collect();
                    if !lines.is_empty() {
                        return Some(
                            lines
                                .into_iter()
                                .map(|line| format!("- {line}"))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// 显式版本或协议字段组合才是信封;单独 text/answer/sections/schemaVersion 不是证据。
/// 不要求 ID,也不拒绝扩展字段:这里是可读化出口,不是严格 schema 校验器。
fn is_protocol_value(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if matches!(
        object.get("schemaVersion").and_then(|v| v.as_str()),
        Some(
            "answer.v1"
                | "summary.v1"
                | "summary.draft.v1"
                | "quiz.v1"
                | "quiz.v2"
                | "translation.v1"
                | "quick_answer.v1"
                | "conversation_summary.v1"
                | "agent.v1"
                | "insight_card.v1"
        )
    ) {
        return true;
    }
    let segment_kind = matches!(
        object.get("kind").and_then(|v| v.as_str()),
        Some(
            "document_fact"
                | "ai_inference"
                | "inference"
                | "example"
                | "user_view"
                | "general_note"
        )
    );
    if segment_kind && (object.contains_key("text") || object.contains_key("citationIds")) {
        return true;
    }
    if let Some(segments) = object.get("segments").and_then(|v| v.as_array()) {
        if object.contains_key("answer")
            || matches!(
                object.get("status").and_then(|v| v.as_str()),
                Some("answered" | "insufficient_evidence" | "needs_clarification")
            )
            || segments.iter().any(is_protocol_value)
        {
            return true;
        }
    }
    if let Some(sections) = object.get("sections").and_then(|v| v.as_array()) {
        // sections/segments/text 也是普通业务字段,仍需段落协议种类等证据。
        if sections.iter().any(is_protocol_value) {
            return true;
        }
    }
    // 兼容已有无版本要点信封,不能扩大为任意含 text/items 字段的 JSON。
    object.len() == 1 && object.get("key_points").is_some_and(|v| v.is_array())
}

/// 容错读取 JSON 前缀,保留对象/数组层级,只丢弃未完成的值。
/// 因此 citations/未知字段中的 text 不会被全局搜键误当正文。
fn read_partial_json_value(
    chars: &[char],
    i: &mut usize,
    depth: usize,
) -> Option<serde_json::Value> {
    use serde_json::Value;
    if depth > 64 {
        return None;
    }
    skip_json_whitespace(chars, i);
    match *chars.get(*i)? {
        '"' => {
            let (text, end) = read_json_string(chars, *i)?;
            *i = end;
            Some(Value::String(text))
        }
        '{' => {
            *i += 1;
            let mut object = serde_json::Map::new();
            loop {
                skip_json_whitespace(chars, i);
                if chars.get(*i) == Some(&'}') {
                    *i += 1;
                    break;
                }
                let Some((key, end)) = read_json_string(chars, *i) else {
                    break;
                };
                *i = end;
                skip_json_whitespace(chars, i);
                if chars.get(*i) != Some(&':') {
                    break;
                }
                *i += 1;
                let Some(value) = read_partial_json_value(chars, i, depth + 1) else {
                    break;
                };
                object.insert(key, value);
                skip_json_whitespace(chars, i);
                if chars.get(*i) == Some(&',') {
                    *i += 1;
                } else {
                    if chars.get(*i) == Some(&'}') {
                        *i += 1;
                    }
                    break;
                }
            }
            Some(Value::Object(object))
        }
        '[' => {
            *i += 1;
            let mut items = Vec::new();
            loop {
                skip_json_whitespace(chars, i);
                if chars.get(*i) == Some(&']') {
                    *i += 1;
                    break;
                }
                let Some(value) = read_partial_json_value(chars, i, depth + 1) else {
                    break;
                };
                items.push(value);
                skip_json_whitespace(chars, i);
                if chars.get(*i) == Some(&',') {
                    *i += 1;
                } else {
                    if chars.get(*i) == Some(&']') {
                        *i += 1;
                    }
                    break;
                }
            }
            Some(Value::Array(items))
        }
        _ => {
            let start = *i;
            while *i < chars.len()
                && !chars[*i].is_whitespace()
                && !matches!(chars[*i], ',' | '}' | ']')
            {
                *i += 1;
            }
            serde_json::from_str(&chars[start..*i].iter().collect::<String>()).ok()
        }
    }
}

fn skip_json_whitespace(chars: &[char], i: &mut usize) {
    while chars.get(*i).is_some_and(|ch| ch.is_whitespace()) {
        *i += 1;
    }
}

/// 读取从 `start`(必须是 `"`)开始的 JSON 字符串,返回 (解码值, 结束位置)。
/// 字符串被截断时仍返回已读部分,便于展示。
fn read_json_string(chars: &[char], start: usize) -> Option<(String, usize)> {
    if chars.get(start).copied() != Some('"') {
        return None;
    }
    let mut raw = String::new();
    let mut i = start + 1;
    let mut closed = false;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\\' {
            if i + 1 < chars.len() {
                raw.push(ch);
                raw.push(chars[i + 1]);
                i += 2;
                continue;
            }
            break;
        }
        if ch == '"' {
            closed = true;
            i += 1;
            break;
        }
        raw.push(ch);
        i += 1;
    }
    if !closed {
        // 丢弃未完成的 Unicode 转义,以及失去低代理项的高代理项。
        // 循环用于先移除半个低代理项、再移除它前面的高代理项;合法代理对不动。
        while let Some(pos) = raw.rfind("\\u") {
            let suffix = &raw[pos + 2..];
            let slashes = raw[..pos]
                .chars()
                .rev()
                .take_while(|ch| *ch == '\\')
                .count();
            let incomplete = suffix.len() < 4 && suffix.chars().all(|ch| ch.is_ascii_hexdigit());
            let high_surrogate = suffix.len() == 4
                && u16::from_str_radix(suffix, 16)
                    .is_ok_and(|code| (0xd800..=0xdbff).contains(&code));
            if slashes % 2 != 0 || !(incomplete || high_surrogate) {
                break;
            }
            raw.truncate(pos);
        }
    }
    let decoded = serde_json::from_str::<String>(&format!("\"{raw}\""))
        .unwrap_or_else(|_| unescape_literal_escapes(&raw));
    Some((decoded, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_source_marker_variants() {
        assert_eq!(
            strip_internal_markers("预算为 500 万元 [SOURCE 3adca4ce-0b0a-511b]。"),
            "预算为 500 万元 。"
        );
        assert_eq!(
            strip_internal_markers("结论成立[SOURCE b1 | original][SOURCE b2 | original]。"),
            "结论成立。"
        );
        assert_eq!(
            strip_internal_markers("A 与 B 等价 [[SOURCE blk_1]] 见原文。"),
            "A 与 B 等价  见原文。"
        );
        assert_eq!(strip_internal_markers("证据(source b1)充分"), "证据充分");
    }

    #[test]
    fn keeps_ordinary_brackets_and_unclosed_markers() {
        assert_eq!(
            strip_internal_markers("见 [1] 与 [注意]"),
            "见 [1] 与 [注意]"
        );
        assert_eq!(
            strip_internal_markers("未闭合 [SOURCE 没有右括号"),
            "未闭合 [SOURCE 没有右括号"
        );
    }

    #[test]
    fn restores_literal_newlines_and_quotes() {
        let out = sanitize_generated_markdown("第一段\\n\\n第二段\\n引号: \\\"引用\\\"");
        assert_eq!(out, "第一段\n\n第二段\n引号: \"引用\"");
    }

    #[test]
    fn decodes_html_entities() {
        assert_eq!(
            sanitize_generated_markdown("A &amp; B &lt;tag&gt; &#39;q&#39; &quot;x&quot;"),
            "A & B <tag> 'q' \"x\""
        );
        // 未知实体保持原样,不猜测。
        assert_eq!(sanitize_generated_markdown("a &bogus; b"), "a &bogus; b");
    }

    #[test]
    fn does_not_touch_code_fences() {
        let input = "说明:\n```\nprint(\"a\\nb\")\n```\n结束";
        let out = sanitize_generated_markdown(input);
        assert!(out.contains("print(\"a\\nb\")"), "代码块内转义必须原样保留");
    }

    #[test]
    fn unwraps_json_envelope_instead_of_showing_json() {
        let out = sanitize_generated_markdown(
            r#"{"answer":"多道批处理提高吞吐量","segments":[{"text":"定义"}]}"#,
        );
        assert_eq!(out, "多道批处理提高吞吐量");
        let listed = sanitize_generated_markdown(r#"{"key_points":["甲","乙"]}"#);
        assert_eq!(listed, "- 甲\n- 乙");
        // 不是信封的 JSON(例如代码)保持原样。
        assert!(sanitize_generated_markdown(r#"{"foo":1}"#).contains("foo"));
    }

    #[test]
    fn sanitize_is_idempotent() {
        let once = sanitize_generated_markdown(
            "结论[SOURCE a][SOURCE b]\\n\\n- 要点 &amp; 说明\\n\\n\\n\\n尾",
        );
        let twice = sanitize_generated_markdown(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn keeps_markdown_structure() {
        let out =
            sanitize_generated_markdown("# 标题\n\n- 列表\n\n| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(out.starts_with("# 标题"));
        assert!(out.contains("- 列表"));
        assert!(out.contains("| a | b |"));
    }

    #[test]
    fn windows_paths_are_not_mangled() {
        assert_eq!(
            sanitize_generated_markdown(r"路径 C:\Temp\new\test"),
            r"路径 C:\Temp\new\test"
        );
    }

    #[test]
    fn salvages_truncated_answer_json_without_leaking_protocol() {
        // 真实故障形态:模型输出在 max tokens 处被截断,落入 plain_text_fallback。
        let raw = r#"{"schemaVersion":"answer.v1","status":"answered","segments":[{"id":"s1","kind":"document_fact","text":"最大熵原理是在满足约束条件的模型集合中选取熵最大的模型。","verificationStatus":"structurally_valid","citationIds":["c1"]},{"id":"s2","kind":"document_fact","text":"KL 散度不是距离,因为它不满足对称性。"#;
        let out = sanitize_generated_markdown(raw);
        assert!(out.contains("最大熵原理"), "必须保留可读正文: {out}");
        assert!(out.contains("KL 散度"), "截断前的后续段落也要保留: {out}");
        assert!(!out.contains("schemaVersion"), "不得泄漏协议字段: {out}");
        assert!(!out.contains("citationIds"), "不得泄漏引用字段: {out}");
        assert!(!out.contains('{'), "不得把原始 JSON 片段展示给用户: {out}");
    }

    #[test]
    fn unwraps_fenced_json_with_commentary_and_sections() {
        let raw = r#"好的,结果如下:
```json
{"schemaVersion":"summary.draft.v1","sections":[{"id":"s1","title":"要点","segments":[{"id":"p1","kind":"document_fact","text":"第一点","citationIds":[]},{"id":"p2","kind":"ai_inference","text":"第二点","citationIds":[]}]}]}
```
以上。"#;
        let out = sanitize_generated_markdown(raw);
        assert!(out.contains("## 要点"));
        assert!(out.contains("第一点"));
        assert!(out.contains("第二点"));
        assert!(!out.contains("schemaVersion"));
    }

    #[test]
    fn latex_commands_are_not_split_into_whitespace() {
        let input = r"公式 $\nabla \times \mathbf{E} = -\frac{\partial \mathbf{B}}{\partial t}$ 与 \theta、\neq、\right";
        let out = sanitize_generated_markdown(input);
        assert!(
            out.contains(r"\nabla \times \mathbf{E}"),
            "数学公式必须原样保留: {out}"
        );
        assert!(out.contains(r"\theta"));
        assert!(out.contains(r"\neq"));
        assert!(out.contains(r"\right"));
        assert!(
            !out.contains("\nablaabla"),
            "\\n 不得把 \\nabla 拆成换行: {out}"
        );
        assert!(
            !out.contains("\timesimes"),
            "\\t 不得把 \\times 拆成制表: {out}"
        );
        assert!(
            !out.contains("\rightight"),
            "\\r 不得把 \\right 拆成回车: {out}"
        );
    }

    #[test]
    fn literal_newlines_outside_math_are_restored() {
        assert_eq!(
            sanitize_generated_markdown("第一行\\n第二行"),
            "第一行\n第二行"
        );
    }

    #[test]
    fn partial_json_strings_are_decoded_not_shown_raw() {
        let raw = r#"{"text":"第一段\\n\\n第二段","kind":"general_note","#.to_string();
        let out = sanitize_generated_markdown(&raw);
        assert_eq!(out, "第一段\n\n第二段");
        assert!(!out.contains("\"text\""));
    }

    fn assert_readable(input: &str, expected: &str) {
        assert_eq!(
            sanitize_generated_markdown(input),
            expected,
            "输入: {input}"
        );
        assert_eq!(salvage_readable_markdown(input), expected);
        assert_eq!(sanitize_generated_markdown(expected), expected, "必须幂等");
    }

    #[test]
    fn ordinary_json_text_fields_and_commentary_stay_intact() {
        for json in [
            r#"{"text":"示例\\n不是正文 &amp; [SOURCE b1]","count":1}"#,
            r#"{"answer":"接口字段","content":{"text":"样例"}}"#,
            r#"{"schemaVersion":"example.v1","text":"样例"}"#,
            r#"{"schemaVersion":1,"text":"样例"}"#,
            r#"{"sections":[{"title":"示例","segments":[{"text":"样例"}]}]}"#,
            r#"{"payload":{"schemaVersion":"answer.v1","text":"内嵌协议示例"}}"#,
            r#"[{"text":"列表样例","kind":"label"}]"#,
            r#"{"text":"字符串里的 \"schemaVersion\":\"answer.v1\" 不是协议"}"#,
        ] {
            for input in [
                json.to_string(),
                format!("前文说明。\n{json}\n后文说明。"),
                format!("前文说明。\n```json\n{json}\n```\n后文说明。"),
            ] {
                assert_readable(&input, &input);
            }
        }
    }

    #[test]
    fn ordinary_json_is_protected_while_surrounding_prose_is_decoded() {
        let json = r#"{"text":"代码\\n &amp; [SOURCE b1]"}"#;
        let input = format!("前文 &amp; 说明\\n{json}\n后文 &#x4e2d;。");
        let expected = format!("前文 & 说明\n{json}\n后文 中。");
        assert_readable(&input, &expected);
    }

    #[test]
    fn recovers_answer_with_unknown_fields_and_missing_ids() {
        let input = r#"{"schemaVersion":"answer.v1","status":"answered","future":{"text":"扩展元数据不可见"},"segments":[{"kind":"document_fact","text":"事实 &amp; 依据[SOURCE 11111111-2222-4333-8444-555555555555]","unknown":true},{"kind":"ai_inference","text":"推论\\n第二行","citationIds":[]}],"citations":[{"text":"引用元数据不可见"}]}"#;
        assert_readable(input, "事实 & 依据\n\n推论\n第二行");
    }

    #[test]
    fn recovers_truncated_fences_with_prefix_and_suffix_commentary() {
        let json = r#"{"schemaVersion":"answer.v1","extra":17,"segments":[{"kind":"document_fact","text":"已恢复第一段"},{"text":"截断的第二段"#;
        for fence in ["```json", "```JSON", "~~~json", "````json", "```"] {
            let close = if fence.starts_with('~') {
                "~~~"
            } else if fence.starts_with("````") {
                "````"
            } else {
                "```"
            };
            for input in [
                format!("前文说明。\n{fence}\n{json}"),
                format!("前文说明。\n{fence}\n{json}\n{close}\n后文说明。"),
            ] {
                assert_readable(&input, "已恢复第一段\n\n截断的第二段");
            }
        }
        assert_readable(
            &format!("前文说明。\n{json}"),
            "已恢复第一段\n\n截断的第二段",
        );
    }

    #[test]
    fn synthetic_answer_v1_cut_at_4000_characters_recovers_actual_body() {
        // 合成脱敏夹具:仅复现 answer.v1 的 4000 字截断结构,并非用户原文。
        // 两段均缺 id,含未知字段、引用元数据,第二段在字符串中途被截断。
        let first = format!(
            "{}[SOURCE 11111111-2222-4333-8444-555555555555]",
            "合成事实：系统按照约束处理输入，并保留可核对的处理结果。".repeat(40)
        );
        let second = "合成推论：应比较不同条件下的结果，不把元数据作为正文展示。".repeat(200);
        let prefix = format!(
            r#"{{"schemaVersion":"answer.v1","status":"answered","futureMeta":{{"text":"不可展示的扩展元数据"}},"segments":[{{"kind":"document_fact","text":{},"verificationStatus":"structurally_valid","citationIds":["c1"]}},{{"kind":"ai_inference","text":""#,
            serde_json::to_string(&first).unwrap()
        );
        let complete = format!(
            "{prefix}{second}\",\"citationIds\":[]}}],\"citations\":[{{\"text\":\"不可展示的引用元数据\"}}]}}"
        );
        assert!(serde_json::from_str::<serde_json::Value>(&complete).is_ok());
        let cut = 4000;
        let remaining = cut - prefix.chars().count();
        assert!(remaining > 0 && remaining < second.chars().count());
        let raw: String = complete.chars().take(cut).collect();
        assert_eq!(raw.chars().count(), cut);
        assert!(serde_json::from_str::<serde_json::Value>(&raw).is_err());
        let expected = format!(
            "{}\n\n{}",
            first.split("[SOURCE").next().unwrap(),
            second.chars().take(remaining).collect::<String>()
        );
        assert_readable(&raw, &expected);
        assert_readable(&format!("以下是结果：\n```json\n{raw}"), &expected);
    }

    #[test]
    fn protocol_metadata_without_body_returns_empty() {
        for input in [
            r#"{"schemaVersion":"answer.v1","status":"answered","segments":[],"citations":[{"text":"引用不是正文"}]}"#,
            r#"{"schemaVersion":"answer.v1","unknown":{"text":"扩展不是正文"},"segments":[{"id":"s1","citationIds":["c1"]}]}"#,
            r#"{"schemaVersion":"answer.v1","segments":[{"text":"  "},{"text":null}]}"#,
            r#"{"schemaVersion":"answer.v1","segments":[{"text":"[SOURCE 11111111-2222-4333-8444-555555555555]"}]}"#,
            r#"{"schemaVersion":"summary.draft.v1","sections":[]}"#,
            r#"{"schemaVersion":"answer.v1","status":"answered","segments":[{"id":"s1","text":""#,
            r#"{"schemaVersion":"answer.v1","future":{"text":"不是正文"#,
            r#"{"status":"answered","segments":[],"citationIds":[]}"#,
            r#"{"kind":"document_fact","citationIds":["c1"]}"#,
        ] {
            assert_readable(input, "");
            assert_readable(&format!("以下是结果。\n```json\n{input}\n```\n以上。"), "");
        }
    }

    #[test]
    fn does_not_recover_text_from_nested_metadata() {
        let raw = r#"{"schemaVersion":"answer.v1","citations":[{"text":"引用正文不能冒充回答"}],"segments":[{"text":"可见回答","unknown":{"text":"扩展字段不能冒充回答"}},{"citationIds":["c1"],"unknown":{"text":"截断扩展字段"#;
        assert_readable(raw, "可见回答");
    }

    #[test]
    fn source_variables_remain_but_real_markers_are_removed() {
        let variables =
            r"a[source]、a[SOURCE]、[source_map]、[sourceful]、f(source)、$f(source)$、source = 1";
        assert_readable(variables, variables);
        assert_eq!(strip_internal_markers(variables), variables);
        for marker in [
            "[SOURCE 11111111-2222-4333-8444-555555555555]",
            "[[source: 11111111-2222-4333-8444-555555555555 | original]]",
            "(SoUrCe 11111111-2222-4333-8444-555555555555)",
        ] {
            assert_readable(&format!("{variables}{marker}。"), &format!("{variables}。"));
        }
        assert_readable(
            "[SOURCE 没闭合，后面有合法标记[SOURCE b1]。",
            "[SOURCE 没闭合，后面有合法标记。",
        );
    }

    #[test]
    fn strips_truncated_source_only_inside_confirmed_protocol() {
        let raw = r#"{"schemaVersion":"answer.v1","segments":[{"text":"可恢复的正文[SOURCE 11111111-2222-4333"#;
        assert_readable(raw, "可恢复的正文");
        let plain = "普通说明 [SOURCE 11111111-2222-4333";
        assert_readable(plain, plain);
    }

    #[test]
    fn protects_inline_code_fence_content_and_math_from_all_transforms() {
        let input = concat!(
            "说明 &amp;。\n",
            "`[SOURCE b1] &amp; \\n` 与 ``内含 ` 与 \\n [SOURCE b2]``\n",
            "````rust\n",
            "let s = r\"[SOURCE b1] &amp; \\n\";  \n\n\n\n",
            "```\n",
            "````\n",
            "~~~text\n[SOURCE b1] &amp; \\n\n~~~\n",
            r"$\nabla\times\right(a[source]\right)$ 与 $$x &amp; [SOURCE b1]$$",
            "\n",
            r"\(f(source)\) 与 \[\nabla &amp; [SOURCE b1]\]",
            "\n后文 &#x4e2d;。",
        );
        let expected = input
            .replacen("说明 &amp;。", "说明 &。", 1)
            .replace("后文 &#x4e2d;。", "后文 中。");
        assert_readable(input, &expected);
        assert_eq!(strip_internal_markers(input), input);
    }

    #[test]
    fn protocol_examples_in_inline_code_and_non_json_fences_are_not_unwrapped() {
        let json = r#"{"schemaVersion":"answer.v1","segments":[{"text":"示例 &amp;"}]}"#;
        for input in [
            format!("前文 `{json}` 后文"),
            format!("前文\n```rust\n{json}\n```\n后文"),
            format!("前文 ${json}$ 后文"),
        ] {
            assert_readable(&input, &input);
        }
    }

    #[test]
    fn decoded_answer_keeps_embedded_code_math_and_windows_paths() {
        let body = concat!(
            "正文 &amp; 说明\\n第二行[SOURCE b1]\n",
            r"`a[source] \n &amp;` 与 $\nabla\times\right(x)$",
            "\n",
            r"路径 C:\Temp\new\test、UNC \\server\share\new\test",
            "\n```json\n{\"text\":\"示例\\n &amp;\"}\n```",
        );
        let envelope =
            serde_json::json!({"schemaVersion": "answer.v1", "segments": [{"text": body}]})
                .to_string();
        let expected = body.replace("正文 &amp; 说明\\n第二行[SOURCE b1]", "正文 & 说明\n第二行");
        assert_readable(&envelope, &expected);
    }

    #[test]
    fn nested_entities_and_newly_exposed_code_are_idempotent() {
        assert_readable("&amp;amp; &#38;quot;引用&amp;quot;", "& \"引用\"");
        // 实体还原形成的代码区,后续步骤不得继续剥 SOURCE 或还原字面换行。
        assert_readable(
            "&#96;[SOURCE b1] &amp; \\n&#96; 后文&amp;。",
            "`[SOURCE b1] & \\n` 后文&。",
        );
    }

    #[test]
    fn english_literal_newlines_do_not_break_latex_commands_or_unc_paths() {
        assert_readable(
            r"First\nSecond\tThird\rFourth",
            "First\nSecond\tThird\rFourth",
        );
        assert_readable(r"首行\\n次行", "首行\n次行");
        let latex = r"\nabla \times \right \theta \neq \rho \textbf{bold} \newcommand";
        assert_readable(latex, latex);
        let paths = r"C:\new\test C:\Temp\note.txt \\server\share\new\test";
        assert_readable(paths, paths);
    }

    #[test]
    fn truncated_unicode_escapes_never_leak_json_fragments() {
        let prefix = r#"{"schemaVersion":"answer.v1","segments":[{"text":"正文"#;
        for suffix in [r"\u4e", r"\u", "\\", r"\uD83D\uDE", r"\uD83D"] {
            assert_readable(&format!("{prefix}{suffix}"), "正文");
        }
        assert_readable(&format!(r"{prefix}\u4e2d\u6587"), "正文中文");
    }
}
