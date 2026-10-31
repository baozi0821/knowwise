//! M09 结构化输出模型:AnswerContent V1(《01》§5.2)与
//! StructuredGenerationPort 类型(《M09》§8)。
//!
//! AI JSON 由 Schema 与 Serde 双校验;未知字段剥离(不静默接受会改变
//! 语义的自由结构),字段长度与数量有上限。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::AppError;

pub const ANSWER_SCHEMA_VERSION: &str = "answer.v1";
/// Prompt 指令版本:冻结系统指令、消息结构与引用规则(《M09》§6)。
/// v3:增加会话摘要层(压缩历史不是原文来源,不得引用)。
pub const PROMPT_INSTRUCTION_VERSION: &str = "qa-instruction.v6";

pub fn err_invalid(reason: &str) -> AppError {
    super::error::model_output_invalid(reason)
}

/// 答案状态(与 TaskOutcome 对齐;out_of_scope 表达范围外问题)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum AnswerStatus {
    Answered,
    InsufficientEvidence,
    Conflict,
    OutOfScope,
}

impl AnswerStatus {
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "answered" => Some(Self::Answered),
            "insufficient_evidence" => Some(Self::InsufficientEvidence),
            "conflict" => Some(Self::Conflict),
            "out_of_scope" => Some(Self::OutOfScope),
            _ => None,
        }
    }
}

/// 段类型(《01》§5.2)。document_fact 必须有结构有效的原文 Citation。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SegmentKind {
    DocumentFact,
    Inference,
    Example,
    UserView,
    GeneralNote,
}

impl SegmentKind {
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "document_fact" => Some(Self::DocumentFact),
            "inference" => Some(Self::Inference),
            "example" => Some(Self::Example),
            "user_view" => Some(Self::UserView),
            "general_note" => Some(Self::GeneralNote),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnswerSegment {
    pub id: String,
    pub kind: SegmentKind,
    pub text: String,
    /// 模型自检只能写 structurally_valid/unverified;最终状态由 M09 裁决。
    pub verification_status: String,
    pub citation_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnswerCitation {
    pub id: String,
    /// 指向 Prompt 中来源块的稳定 ID。
    pub block_id: String,
    pub quoted_text: String,
    pub claim_segment_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnswerContent {
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    pub status: AnswerStatus,
    pub segments: Vec<AnswerSegment>,
    pub citations: Vec<AnswerCitation>,
    #[serde(default)]
    pub conflicts: Vec<AnswerConflict>,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub unverified_notes: Vec<String>,
}

/// 冲突描述:不擅自裁决哪个事实正确,列出双方证据。
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AnswerConflict {
    pub description: String,
    pub citation_ids: Vec<String>,
}

/// 结构上限:防御性拒绝(《M09》§7)。
pub const MAX_SEGMENTS: usize = 32;
pub const MAX_CITATIONS: usize = 48;
pub const MAX_SEGMENT_TEXT_CHARS: usize = 4000;
pub const MAX_QUOTE_TEXT_CHARS: usize = 2000;

/// 纯文本降级答案的可见标注(紧凑标签,不是内部哨兵)。
/// 前端会原样展示 `unverified_notes`,因此这里必须是给用户看的话。
pub const FALLBACK_UNVERIFIED_NOTE: &str = "未校验:模型未按结构化格式返回,内容未经引用核对";

/// 把答案里的引用 ID 改写为落库后的稳定 ID。
///
/// 模型自造的 `c1/c2` 只在本次回答内有意义;前端点击引用时会拿这个 ID
/// 调 `evidence_resolve_citation`,必须与 M05 表里的真实 ID 一致,否则
/// 永远“无法定位该引用”。
///
/// `mapping` = 模型 ID → 持久化 ID(按 answer.citations 顺序一一对应)。
/// 同时改写段落 citationIds、冲突 citationIds 与 citation.claimSegmentIds
/// 所引用的段落(段落 ID 不变,只改引用侧)。
pub fn rewrite_citation_ids(answer: &mut AnswerContent, mapping: &[(String, String)]) {
    if mapping.is_empty() {
        return;
    }
    let resolve = |id: &str| -> Option<String> {
        mapping
            .iter()
            .find(|(old, _)| old == id)
            .map(|(_, new)| new.clone())
    };
    for citation in &mut answer.citations {
        if let Some(new_id) = resolve(&citation.id) {
            citation.id = new_id;
        }
    }
    for segment in &mut answer.segments {
        segment.citation_ids = segment
            .citation_ids
            .iter()
            .filter_map(|id| resolve(id))
            .collect();
    }
    for conflict in &mut answer.conflicts {
        conflict.citation_ids = conflict
            .citation_ids
            .iter()
            .filter_map(|id| resolve(id))
            .collect();
    }
}

/// 原文抽取兜底答案(模型超时/不可用/输出不可用时使用)。
///
/// 只使用真实原文块的首句,不做任何推断;段类型固定 general_note,
/// `verification_status=unverified`,并带一条紧凑可见说明。
/// 没有任何可用原文时返回 None(调用方应改走 `assistant_notice`)。
pub fn extractive_fallback_answer(
    sentences: &[(String, Option<i64>)],
    note: impl Into<String>,
) -> Option<AnswerContent> {
    let segments: Vec<AnswerSegment> = sentences
        .iter()
        .enumerate()
        .map(|(index, (text, page))| {
            let label = page.map(|page| format!("p.{page} ")).unwrap_or_default();
            AnswerSegment {
                id: format!("fallback-{}", index + 1),
                kind: SegmentKind::GeneralNote,
                text: format!("{label}{text}"),
                verification_status: "unverified".into(),
                citation_ids: Vec::new(),
            }
        })
        .filter(|segment| !segment.text.trim().is_empty())
        .collect();
    if segments.is_empty() {
        return None;
    }
    Some(AnswerContent {
        schema_version: ANSWER_SCHEMA_VERSION.to_string(),
        status: AnswerStatus::Answered,
        segments,
        citations: Vec::new(),
        conflicts: Vec::new(),
        suggestions: Vec::new(),
        unverified_notes: vec![note.into()],
    })
}

/// 兼容保留:内部来源标记剥离统一委托给 `shared::sanitize`。
/// 新代码请直接调用 `shared::sanitize::sanitize_generated_markdown`
/// (它还处理字面 `\n`、HTML 实体与 JSON 信封)。
pub fn strip_source_markers(text: &str) -> String {
    crate::shared::sanitize::strip_internal_markers(text)
}

/// Parse + 归一化 + 基线结构检查。语义级校验在编排层
/// 结合候选白名单执行(《M09》§7 的校验顺序)。
///
/// 容错优先:模型常见的漏字段、字段别名、未知字段、字符串分段都在这里
/// 补齐为可落库结构;只有完全无法恢复时才返回错误。未知字段被丢弃,
/// 但正文不会被丢弃。
pub fn parse_answer_content(raw: &serde_json::Value) -> Result<AnswerContent, AppError> {
    let mut value = normalize_answer_value(raw);
    let object = value
        .as_object_mut()
        .ok_or_else(|| err_invalid("answer_decode_failed"))?;
    normalize_segments(object);
    normalize_citations(object);
    normalize_string_array(object, "suggestions");
    normalize_string_array(object, "unverifiedNotes");
    normalize_conflicts(object);

    let mut content: AnswerContent =
        serde_json::from_value(value).map_err(|_| err_invalid("answer_decode_failed"))?;
    if content.schema_version.trim().is_empty() {
        content.schema_version = ANSWER_SCHEMA_VERSION.to_string();
    }
    if content.segments.len() > MAX_SEGMENTS {
        // 保留内容:超出结构上限时截断而不是整轮失败。
        content.segments.truncate(MAX_SEGMENTS);
    }
    if content.citations.len() > MAX_CITATIONS {
        content.citations.truncate(MAX_CITATIONS);
    }
    let mut expanded: Vec<AnswerSegment> = Vec::with_capacity(content.segments.len());
    for segment in content.segments.drain(..) {
        for (index, text) in split_segment_text(&segment.text).into_iter().enumerate() {
            let mut item = segment.clone();
            if index > 0 {
                item.id = format!("{}-{}", segment.id, index + 1);
            }
            item.text = text;
            expanded.push(item);
        }
    }
    content.segments = expanded;
    for segment in &mut content.segments {
        if segment.verification_status != "structurally_valid"
            && segment.verification_status != "unverified"
        {
            segment.verification_status = "unverified".into();
        }
        if segment.kind != SegmentKind::DocumentFact
            && segment.verification_status == "structurally_valid"
        {
            segment.verification_status = "unverified".into();
        }
    }
    Ok(content)
}

/// 把各种模型输出形态归一化为 answer.v1 对象。
fn normalize_answer_value(raw: &serde_json::Value) -> serde_json::Value {
    if let Some(text) = raw.as_str() {
        return serde_json::json!({
            "schemaVersion": ANSWER_SCHEMA_VERSION,
            "status": "answered",
            "segments": [{
                "id": "s1",
                "kind": "general_note",
                "text": text,
                "verificationStatus": "unverified",
                "citationIds": []
            }],
            "citations": []
        });
    }
    let mut value = raw.clone();
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    object
        .entry("schemaVersion")
        .or_insert_with(|| serde_json::Value::String(ANSWER_SCHEMA_VERSION.to_string()));
    let has_segments = object
        .get("segments")
        .and_then(|v| v.as_array())
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    object.entry("status").or_insert_with(|| {
        serde_json::Value::String(
            if has_segments {
                "answered"
            } else {
                "insufficient_evidence"
            }
            .into(),
        )
    });
    // 模型有时把答案写成 answer/text 字段而不是 segments。
    if !has_segments {
        let text = object
            .get("answer")
            .or_else(|| object.get("text"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty());
        if let Some(text) = text {
            object.insert(
                "segments".into(),
                serde_json::json!([{
                    "id": "s1",
                    "kind": "general_note",
                    "text": text,
                    "verificationStatus": "unverified",
                    "citationIds": []
                }]),
            );
            object.insert(
                "status".into(),
                serde_json::Value::String("answered".into()),
            );
        } else {
            object
                .entry("segments")
                .or_insert_with(|| serde_json::json!([]));
        }
    }
    object
        .entry("citations")
        .or_insert_with(|| serde_json::json!([]));
    value
}

fn normalize_segments(object: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(items) = object.get_mut("segments").and_then(|v| v.as_array_mut()) else {
        return;
    };
    let mut normalized = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let segment = match item {
            serde_json::Value::String(text) => serde_json::json!({
                "id": format!("s{}", index + 1),
                "kind": "general_note",
                "text": text,
                "verificationStatus": "unverified",
                "citationIds": []
            }),
            serde_json::Value::Object(map) => {
                let mut map = map.clone();
                map.entry("id")
                    .or_insert_with(|| serde_json::Value::String(format!("s{}", index + 1)));
                if map
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
                {
                    map.insert(
                        "id".into(),
                        serde_json::Value::String(format!("s{}", index + 1)),
                    );
                }
                let kind = map
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map(normalize_segment_kind)
                    .unwrap_or("general_note");
                map.insert("kind".into(), serde_json::Value::String(kind.into()));
                if map
                    .get("text")
                    .and_then(|v| v.as_str())
                    .is_none_or(|t| t.trim().is_empty())
                {
                    let text = map
                        .get("content")
                        .or_else(|| map.get("claim"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    map.insert("text".into(), serde_json::Value::String(text.to_string()));
                }
                map.entry("verificationStatus")
                    .or_insert_with(|| serde_json::Value::String("unverified".into()));
                map.entry("citationIds")
                    .or_insert_with(|| serde_json::json!([]));
                serde_json::Value::Object(map)
            }
            _ => continue,
        };
        normalized.push(segment);
    }
    *items = normalized;
}

fn normalize_segment_kind(kind: &str) -> &'static str {
    match kind.trim().to_ascii_lowercase().as_str() {
        "document_fact" | "documentfact" | "fact" | "原文事实" => "document_fact",
        "inference" | "ai_inference" | "推断" => "inference",
        "example" | "举例" => "example",
        "user_view" | "userview" | "用户观点" => "user_view",
        _ => "general_note",
    }
}

fn normalize_citations(object: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(items) = object.get_mut("citations").and_then(|v| v.as_array_mut()) else {
        return;
    };
    let mut normalized = Vec::with_capacity(items.len());
    let mut seen = std::collections::HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let Some(map) = item.as_object() else {
            continue;
        };
        let mut map = map.clone();
        let mut id = map
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if id.is_empty() {
            id = format!("c{}", index + 1);
        }
        if !seen.insert(id.clone()) {
            id = format!("{}-{}", id, index + 1);
            seen.insert(id.clone());
        }
        map.insert("id".into(), serde_json::Value::String(id));
        let block_id = map
            .get("blockId")
            .or_else(|| map.get("block_id"))
            .or_else(|| map.get("sourceId"))
            .or_else(|| map.get("source_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        map.insert("blockId".into(), serde_json::Value::String(block_id));
        let quote = map
            .get("quotedText")
            .or_else(|| map.get("quoted_text"))
            .or_else(|| map.get("quote"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        map.insert("quotedText".into(), serde_json::Value::String(quote));
        map.entry("claimSegmentIds")
            .or_insert_with(|| serde_json::json!([]));
        normalized.push(serde_json::Value::Object(map));
    }
    *items = normalized;
}

fn normalize_string_array(object: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    let Some(value) = object.get_mut(key) else {
        object.insert(key.into(), serde_json::json!([]));
        return;
    };
    let items: Vec<String> = match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::String(text) => vec![text.clone()],
        _ => Vec::new(),
    };
    *value = serde_json::json!(items);
}

fn normalize_conflicts(object: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(items) = object.get_mut("conflicts").and_then(|v| v.as_array_mut()) else {
        object.insert("conflicts".into(), serde_json::json!([]));
        return;
    };
    let mut normalized = Vec::new();
    for item in items.iter() {
        let Some(map) = item.as_object() else {
            continue;
        };
        let description = map
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if description.trim().is_empty() {
            continue;
        }
        let citation_ids: Vec<String> = map
            .get("citationIds")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        normalized.push(serde_json::json!({
            "description": description,
            "citationIds": citation_ids
        }));
    }
    *items = normalized;
}

/// 纯文本答案 → 可落库段(按段落/长度切分,不丢正文)。
pub fn plain_text_segments(text: &str) -> Vec<AnswerSegment> {
    split_segment_text(text)
        .into_iter()
        .enumerate()
        .map(|(index, text)| AnswerSegment {
            id: format!("fallback-{}", index + 1),
            kind: SegmentKind::GeneralNote,
            text,
            verification_status: "unverified".into(),
            citation_ids: Vec::new(),
        })
        .collect()
}

/// 单段过长时按段落/硬切分,保留内容而不是整轮失败。
fn split_segment_text(text: &str) -> Vec<String> {
    if text.chars().count() <= MAX_SEGMENT_TEXT_CHARS {
        return vec![text.to_string()];
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    for paragraph in text.split_inclusive('\n') {
        if current.chars().count() + paragraph.chars().count() > MAX_SEGMENT_TEXT_CHARS
            && !current.is_empty()
        {
            parts.push(std::mem::take(&mut current));
        }
        if paragraph.chars().count() > MAX_SEGMENT_TEXT_CHARS {
            let mut chunk = String::new();
            for ch in paragraph.chars() {
                if chunk.chars().count() >= MAX_SEGMENT_TEXT_CHARS {
                    parts.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            if !chunk.is_empty() {
                current.push_str(&chunk);
            }
        } else {
            current.push_str(paragraph);
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// 证据不足时的规范 AnswerContent:不编造、不带无据引文。
pub fn insufficient_evidence_answer(note: Option<String>) -> AnswerContent {
    AnswerContent {
        schema_version: ANSWER_SCHEMA_VERSION.to_string(),
        status: AnswerStatus::InsufficientEvidence,
        segments: Vec::new(),
        citations: Vec::new(),
        conflicts: Vec::new(),
        suggestions: Vec::new(),
        unverified_notes: note.into_iter().collect(),
    }
}

// —— StructuredGenerationPort 类型(《M09》§8)——

/// 生成任务类型;M10–M12 复用同一 Port,持久化归产物模块。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum TaskKind {
    Qa,
    SummaryMap,
    SummaryReduce,
    Quiz,
    Graph,
    /// 会话滚动摘要(长上下文 WBS-5):压缩历史,不是文档产物。
    ConversationSummary,
    /// 2A 动态卡片:只生成卡片正文与来源,不落库;写入由 CardService 校验。
    InsightCard,
    /// 2B 混合题型题目(quiz.v2)。
    QuizV2,
    /// 2C 段落翻译(translation.v1)。
    Translation,
}

/// 来源块角色:original(原文块)/ user_note / ai_draft(《M09》§6)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SourceRole {
    Original,
    UserNote,
    AiDraft,
}

/// 会话历史单轮(M09 只消费投影,不保存消息)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatTurnRole {
    User,
    Assistant,
}

/// 最近对话轮次:由 M13 从 M07 消息投影,按时间升序传入。
#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: ChatTurnRole,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct StructuredGenerationRequest {
    pub task_id: String,
    pub attempt_id: String,
    /// 文档粒度授权目标(M08 EgressGateway 按 document×位置×提供方×用途
    /// 校验许可);端口实现不得丢弃或替换。
    pub document_id: String,
    pub snapshot_id: String,
    pub schema_version: String,
    /// JSON Schema(schemars 生成;此处只透传给出口实现)。
    pub schema: serde_json::Value,
    pub instruction_version: String,
    pub task_kind: TaskKind,
    pub inputs: Vec<GenerationInput>,
    pub locale: String,
    /// 用户问题(QA;其他任务为任务指令)。
    pub question: String,
    /// 最近对话轮次(按时间升序;不含当前问题)。仅 QA 使用,
    /// 由 M13 按 token 预算裁剪,裁剪原因随任务结果记录。
    pub history: Vec<ChatTurn>,
    /// 更早会话的滚动摘要(压缩历史;不是文档来源,不得引用)。
    /// 与 `history` 一起构成「摘要 + 最近原文」的分层上下文。
    pub history_summary: Option<String>,
    /// 随本轮问题发送的图片附件(已压缩;最多 3 张)。
    /// M13 从附件服务读取并校验;M09 只透传给出口实现。
    pub attachments: Vec<GenerationAttachment>,
    /// 2F 文档地图(标题树 + 页范围,紧凑文本)。
    /// 只描述“文档有哪些章节、在哪几页”,不是原文证据,不得被引用;
    /// 长文档用它先定位章节,再由检索读具体块。
    pub document_map: Option<String>,
    /// 请求作用域:普通任务为 Task(必须绑定活动 Task/Attempt);
    /// 卡片等 UI 直接触发的单次生成使用 Document(不绑定 Task,
    /// 但授权、载荷哈希、来源白名单、密钥与审计全部保留)。
    pub scope: crate::model_privacy::gateway::EgressScope,
}

/// 图片附件在生成链路中的最小投影。
#[derive(Debug, Clone)]
pub struct GenerationAttachment {
    pub attachment_id: String,
    pub mime: String,
    /// 附件字节 sha256 hex,用于许可载荷绑定与读取校验。
    pub sha256: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct GenerationInput {
    /// Prompt 内稳定来源 ID(模型引用时使用)。
    pub source_id: String,
    pub role: SourceRole,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct StructuredGenerationResult {
    /// 已通过 Schema 解析的结构化内容;Port 实现保证为合法 JSON 值。
    pub content: serde_json::Value,
    pub model_provider_id: String,
    pub model_id: String,
    /// partial=true 表示本次结果来自非流式降级或截断恢复,仅供进度。
    pub partial: bool,
}
