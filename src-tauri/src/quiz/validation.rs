//! M11 题目 Codec 与校验流水线(《M11》§2–§4)。
//!
//! 校验分层:
//! 1. Codec:JSON ↔ quiz.v1 严格解析(schemaVersion 必须精确匹配;
//!    未知字段拒绝,不静默接受改变语义的自由结构)。
//! 2. 生成配置/材料校验:数量、难度、限长;选区过短、覆盖不足、
//!    无原文证据时在调用模型前拒绝。
//! 3. 题目校验:选项 ID 唯一、正确答案唯一、Citation 属于材料范围且
//!    引文匹配原文、解析有依据、同次生成去重。
//!
//! 结构校验通过只说明题面/答案/依据结构完整,不等于语义上唯一正确
//! (《M11》§4);语义争议走用户反馈(disputed),不在这里推断。
//! 单题无效不把整个产物删成空白:无效题保留为占位并携带原因,
//! 界面显示"此题无效,不计分,可重新生成"。

use crate::shared::textnorm::{normalize_v1, normalized_hash};
use crate::shared::{AppError, CoreErrorCode};

use super::models::*;

/// 题干上限(字符)。
pub const MAX_PROMPT_CHARS: usize = 2_000;
/// 单个选项文本上限。
pub const MAX_OPTION_TEXT_CHARS: usize = 1_000;
/// 单个解析段上限。
pub const MAX_SEGMENT_CHARS: usize = 4_000;
/// 选项数量下/上限(《M11》§2 建议 3–5)。
pub const MIN_OPTIONS: usize = 3;
pub const MAX_OPTIONS: usize = 5;
/// 单次生成数量界限(1–10,MVP 默认 5)。
pub const MIN_GENERATE_COUNT: u8 = 1;
pub const MAX_GENERATE_COUNT: u8 = 10;
/// 单个产物题目数量 sanity 上限(重复生成的版本可能累积)。
pub const MAX_QUESTIONS_PER_CONTENT: usize = 100;
/// 题目风格上限。
pub const MAX_STYLE_CHARS: usize = 200;
/// 用户附加要求上限。
pub const MAX_EXTRA_INSTRUCTIONS_CHARS: usize = 2_000;
/// 生成材料总字符下限:低于该值视为选区过短,拒绝生成。
pub const MIN_MATERIAL_CHARS: usize = 200;
/// 反馈说明上限(纯文本,不执行 Markdown)。
pub const MAX_FEEDBACK_COMMENT_CHARS: usize = 2_000;

fn invalid(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
        .with_detail("reason", reason.into())
}

/// 《02》§7 `QUIZ_QUESTION_INVALID`:题目/题目集无效(不可重试)。
/// 用于保存内容损坏、题目集结构非法等题目本体问题。
pub fn quiz_question_invalid(reason: &str) -> AppError {
    AppError::new(
        CoreErrorCode::QuizQuestionInvalid,
        crate::shared::new_trace_id(),
    )
    .with_detail("reason", reason.into())
}

/// quiz.v1 产物 Codec:与 M10 `ArtifactCodec` 契约(《M10》§4:
/// kind/schemaVersion/validate)形状一致。M10 模块进入编译图后由 M13
/// 注册(`impl crate::artifacts::models::ArtifactCodec for QuizV1Codec`
/// 为逐字段转发);未注册 kind 的内容不能写入产物。
pub struct QuizV1Codec;

impl QuizV1Codec {
    pub fn kind(&self) -> &str {
        "quiz"
    }

    pub fn schema_version(&self) -> &str {
        QUIZ_SCHEMA_VERSION
    }

    /// 写入前校验:结构完整(选项、唯一答案、解析、Citation 形状)。
    /// 与生成校验的区别:此处不携带材料,不判定引文是否命中原文块,
    /// 只拒绝结构性损坏;作答计分仍以生成时写入的 validity 为准。
    pub fn validate(&self, content: &serde_json::Value) -> bool {
        let Ok(parsed) = serde_json::from_value::<QuizContentV1>(content.clone()) else {
            return false;
        };
        if parsed.schema_version != QUIZ_SCHEMA_VERSION || parsed.questions.is_empty() {
            return false;
        }
        let mut ids = std::collections::HashSet::new();
        parsed.questions.iter().all(|q| {
            ids.insert(q.id.as_str())
                && !q.prompt.trim().is_empty()
                && (MIN_OPTIONS..=MAX_OPTIONS).contains(&q.options.len())
                && q.options.iter().any(|o| o.id == q.correct_option_id)
                && !q.explanation_segments.is_empty()
                && !q.citations.is_empty()
        })
    }
}

// —— Codec ——

/// 严格解析 quiz.v1 内容 JSON;schemaVersion 不匹配即失败(升级版本另加读取器)。
pub fn parse_quiz_content(raw: &str) -> Result<QuizContentV1, AppError> {
    let mut content: QuizContentV1 = serde_json::from_str(raw).map_err(|e| {
        quiz_question_invalid("content_unparseable").with_detail("serde", e.to_string().into())
    })?;
    if content.schema_version != QUIZ_SCHEMA_VERSION {
        return Err(quiz_question_invalid("schema_version_mismatch")
            .with_detail("expected", QUIZ_SCHEMA_VERSION.into())
            .with_detail("actual", content.schema_version.clone().into()));
    }
    for question in &mut content.questions {
        question.prompt = crate::shared::sanitize::sanitize_generated_markdown(&question.prompt);
        for option in &mut question.options {
            option.text = crate::shared::sanitize::sanitize_generated_markdown(&option.text);
        }
        for segment in &mut question.explanation_segments {
            segment.text = crate::shared::sanitize::sanitize_generated_markdown(&segment.text);
        }
    }
    if content.questions.len() > MAX_QUESTIONS_PER_CONTENT {
        return Err(quiz_question_invalid("too_many_questions")
            .with_detail("max", (MAX_QUESTIONS_PER_CONTENT as i64).into()));
    }
    Ok(content)
}

/// 序列化 quiz.v1(保存路径经 M10 Codec;此处保证确定性形状)。
pub fn quiz_content_to_json(content: &QuizContentV1) -> Result<String, AppError> {
    serde_json::to_string(content).map_err(|e| {
        AppError::internal(crate::shared::new_trace_id())
            .with_detail("reason", "quiz_content_serialize_failed".into())
            .with_detail("serde", e.to_string().into())
    })
}

// —— 生成配置与材料校验 ——

/// 生成配置校验(《M11》§2):数量 1–10、难度非空去重、限长、无脚本内容。
pub fn validate_generation_config(config: &QuizGenerationConfig) -> Result<(), AppError> {
    if !(MIN_GENERATE_COUNT..=MAX_GENERATE_COUNT).contains(&config.count) {
        return Err(invalid("quiz_count_out_of_range")
            .with_detail("min", (MIN_GENERATE_COUNT as i64).into())
            .with_detail("max", (MAX_GENERATE_COUNT as i64).into()));
    }
    if config.difficulties.is_empty() {
        return Err(invalid("quiz_difficulties_empty"));
    }
    let mut seen = Vec::new();
    for d in &config.difficulties {
        if seen.contains(d) {
            return Err(invalid("quiz_difficulties_duplicated"));
        }
        seen.push(*d);
    }
    if config.style.chars().count() > MAX_STYLE_CHARS {
        return Err(
            invalid("quiz_style_too_long").with_detail("maxChars", (MAX_STYLE_CHARS as i64).into())
        );
    }
    validate_plain_instructions(
        &config.extra_instructions,
        MAX_EXTRA_INSTRUCTIONS_CHARS,
        "quiz_extra_instructions",
    )?;
    Ok(())
}

/// 反馈说明校验(《M11》§4):可选、限长、纯文本。
pub fn validate_feedback_comment(comment: &str) -> Result<(), AppError> {
    validate_plain_instructions(comment, MAX_FEEDBACK_COMMENT_CHARS, "quiz_feedback_comment")
}

/// 纯文本策略:限长 + 禁止脚本类内容(与 M06 笔记策略一致的保守子集)。
fn validate_plain_instructions(text: &str, max_chars: usize, reason: &str) -> Result<(), AppError> {
    if text.chars().count() > max_chars {
        return Err(invalid(&format!("{reason}_too_long"))
            .with_detail("maxChars", (max_chars as i64).into()));
    }
    let lower = text.to_lowercase();
    for banned in ["<script", "javascript:", "data:text/html"] {
        if lower.contains(banned) {
            return Err(invalid(&format!("{reason}_unsafe")));
        }
    }
    Ok(())
}

/// 生成材料校验(《M11》§2):无块 = 无原文证据;总字符过低 = 选区过短;
/// 块 ID 重复/空文本 = 材料结构无效(覆盖不足)。
pub fn validate_generation_materials(materials: &QuizGenerationMaterials) -> Result<(), AppError> {
    if materials.blocks.is_empty() {
        return Err(invalid("quiz_material_no_evidence"));
    }
    let total: usize = materials
        .blocks
        .iter()
        .map(|b| b.text.chars().count())
        .sum();
    if total < MIN_MATERIAL_CHARS {
        return Err(invalid("quiz_material_too_short")
            .with_detail("minChars", (MIN_MATERIAL_CHARS as i64).into())
            .with_detail("actualChars", (total as i64).into()));
    }
    let mut ids = std::collections::HashSet::new();
    for b in &materials.blocks {
        if b.block_id.trim().is_empty() || !ids.insert(b.block_id.as_str()) {
            return Err(invalid("quiz_material_block_invalid"));
        }
        if b.text.trim().is_empty() {
            return Err(invalid("quiz_material_block_invalid"));
        }
    }
    Ok(())
}

// —— 引文匹配 ——

/// 引文是否属于块原文:精确子串 → 去首尾空白精确 → 规范化匹配
/// (与 M05 引文校验同一策略;规范化辅助值不覆盖原文,《01》§2)。
pub fn quote_matches_block(block_text: &str, quoted_text: &str) -> bool {
    if quoted_text.is_empty() {
        return false;
    }
    if block_text.contains(quoted_text) {
        return true;
    }
    let trimmed = quoted_text.trim();
    if !trimmed.is_empty() && block_text.contains(trimmed) {
        return true;
    }
    let needle_src = if trimmed.is_empty() {
        quoted_text
    } else {
        trimmed
    };
    let hay = normalize_v1(block_text);
    let needle = normalize_v1(needle_src);
    hay.find_original_range(&needle).is_some()
}

// —— 题目校验流水线(《M11》§3)——

/// 单条 Citation 的结构检查结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CitationCheckStatus {
    Valid,
    TargetMissing,
    QuoteMismatch,
}

#[derive(Debug, Clone)]
pub struct CitationCheck {
    pub citation_id: String,
    pub block_id: String,
    pub status: CitationCheckStatus,
}

/// 单题校验结论(与内容中的 validity/invalid_reason 对齐)。
#[derive(Debug, Clone)]
pub struct QuestionOutcome {
    pub question_id: String,
    pub validity: QuestionValidity,
    pub invalid_reason: Option<QuestionInvalidReason>,
    pub citation_checks: Vec<CitationCheck>,
}

/// 整批校验报告:内容(已写回有效性) + 逐题结论。
#[derive(Debug, Clone)]
pub struct QuizValidationReport {
    pub content: QuizContentV1,
    pub outcomes: Vec<QuestionOutcome>,
    pub valid_count: usize,
    pub invalid_count: usize,
}

impl QuizValidationReport {
    /// 需要重新生成的无效题 ID(允许只重生成无效题,不覆盖有效题)。
    pub fn invalid_question_ids(&self) -> Vec<String> {
        self.outcomes
            .iter()
            .filter(|o| o.validity == QuestionValidity::Invalid)
            .map(|o| o.question_id.clone())
            .collect()
    }
}

/// 题目去重指纹:规范化题干 + 规范化选项文本集合(顺序无关)。
/// 同次生成中指纹重复的题目标记 DuplicateQuestion,不计数。
fn question_fingerprint(q: &QuizQuestion) -> String {
    let mut options: Vec<String> = q.options.iter().map(|o| normalized_hash(&o.text)).collect();
    options.sort();
    format!(
        "{}|{}|{}",
        normalized_hash(&q.prompt),
        options.join(","),
        normalized_hash(&q.correct_option_id)
    )
}

/// 校验模型产出的 quiz.v1 草稿(validity 应为 unverified)。
/// 返回写回有效性状态的整批内容:单题无效保留为占位,不删成空白。
///
/// 材料即调用方(M10 编排)从冻结快照导出的块投影;"Citation 属于当前
/// 快照"在此体现为 blockId 必须命中材料集合,引文必须匹配块原文。
pub fn validate_generated_quiz(
    draft: QuizContentV1,
    materials: &QuizGenerationMaterials,
) -> QuizValidationReport {
    let blocks: std::collections::HashMap<&str, &str> = materials
        .blocks
        .iter()
        .map(|b| (b.block_id.as_str(), b.text.as_str()))
        .collect();

    let mut content = draft;
    let mut outcomes = Vec::with_capacity(content.questions.len());
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_fingerprints: std::collections::HashSet<String> = std::collections::HashSet::new();

    for q in content.questions.iter_mut() {
        let (validity, reason, checks) =
            validate_question_struct(q, &blocks, &seen_ids, &mut seen_fingerprints);
        // 指纹对全部题去重;ID 集合只登记非空 ID(空 ID 题已判 QuestionIdInvalid)。
        if !q.id.trim().is_empty() {
            seen_ids.insert(q.id.clone());
        }
        q.validity = validity;
        q.invalid_reason = reason;
        outcomes.push(QuestionOutcome {
            question_id: q.id.clone(),
            validity,
            invalid_reason: reason,
            citation_checks: checks,
        });
    }

    let valid_count = outcomes
        .iter()
        .filter(|o| o.validity == QuestionValidity::Valid)
        .count();
    let invalid_count = outcomes.len() - valid_count;
    QuizValidationReport {
        content,
        outcomes,
        valid_count,
        invalid_count,
    }
}

type BlockMap<'a> = std::collections::HashMap<&'a str, &'a str>;

/// 单题结构校验(《M11》§3 表逐行);返回(有效性, 无效原因, 引文检查)。
fn validate_question_struct(
    q: &QuizQuestion,
    blocks: &BlockMap<'_>,
    seen_ids: &std::collections::HashSet<String>,
    seen_fingerprints: &mut std::collections::HashSet<String>,
) -> (
    QuestionValidity,
    Option<QuestionInvalidReason>,
    Vec<CitationCheck>,
) {
    use QuestionInvalidReason as R;

    // 题目 ID:非空且同批唯一。
    if q.id.trim().is_empty() || seen_ids.contains(&q.id) {
        return (
            QuestionValidity::Invalid,
            Some(R::QuestionIdInvalid),
            Vec::new(),
        );
    }
    // 题干:非空、限长。
    if q.prompt.trim().is_empty() || q.prompt.chars().count() > MAX_PROMPT_CHARS {
        return (
            QuestionValidity::Invalid,
            Some(R::SchemaInvalid),
            Vec::new(),
        );
    }
    // 选项:3–5 个,ID 唯一非空,文本非空限长。
    let mut option_ids = std::collections::HashSet::new();
    if q.options.len() < MIN_OPTIONS || q.options.len() > MAX_OPTIONS {
        return (
            QuestionValidity::Invalid,
            Some(R::OptionStructureInvalid),
            Vec::new(),
        );
    }
    for o in &q.options {
        if o.id.trim().is_empty() || !option_ids.insert(o.id.as_str()) {
            return (
                QuestionValidity::Invalid,
                Some(R::OptionStructureInvalid),
                Vec::new(),
            );
        }
        if o.text.trim().is_empty() || o.text.chars().count() > MAX_OPTION_TEXT_CHARS {
            return (
                QuestionValidity::Invalid,
                Some(R::OptionStructureInvalid),
                Vec::new(),
            );
        }
    }
    // 正确选项:必须在选项集合内(schema 单字段 ⇒ 天然唯一;未知即无效)。
    if !option_ids.contains(q.correct_option_id.as_str()) {
        return (
            QuestionValidity::Invalid,
            Some(R::CorrectOptionUnknown),
            Vec::new(),
        );
    }
    // Citation:块属于材料(快照)且引文匹配;至少一条有效。
    let mut checks = Vec::with_capacity(q.citations.len());
    let mut valid_citation_ids: Vec<String> = Vec::new();
    for c in &q.citations {
        let status = match blocks.get(c.block_id.as_str()) {
            None => CitationCheckStatus::TargetMissing,
            Some(text) => {
                if quote_matches_block(text, &c.quoted_text) {
                    CitationCheckStatus::Valid
                } else {
                    CitationCheckStatus::QuoteMismatch
                }
            }
        };
        if status == CitationCheckStatus::Valid {
            valid_citation_ids.push(c.id.clone());
        }
        checks.push(CitationCheck {
            citation_id: c.id.clone(),
            block_id: c.block_id.clone(),
            status,
        });
    }
    let citation_reason = if valid_citation_ids.is_empty() {
        // 无任何 Citation ⇒ 无依据;有 Citation 但全部失效 ⇒ 按首个失败类别。
        if q.citations.is_empty() {
            Some(R::NoValidCitation)
        } else {
            Some(
                match checks
                    .iter()
                    .find(|c| c.status != CitationCheckStatus::Valid)
                {
                    Some(c) if c.status == CitationCheckStatus::TargetMissing => {
                        R::CitationTargetMissing
                    }
                    Some(_) => R::CitationQuoteMismatch,
                    None => R::NoValidCitation,
                },
            )
        }
    } else {
        None
    };
    // 解析:非空;document_fact 段必须引用至少一条有效 Citation;
    // 整题至少一个受支持的 document_fact 段(解析必须解释正确答案)。
    let mut supported_fact = false;
    let mut segment_ids = std::collections::HashSet::new();
    let mut explanation_ok = !q.explanation_segments.is_empty();
    for s in &q.explanation_segments {
        if s.id.trim().is_empty() || !segment_ids.insert(s.id.as_str()) {
            explanation_ok = false;
            break;
        }
        if s.text.trim().is_empty() || s.text.chars().count() > MAX_SEGMENT_CHARS {
            explanation_ok = false;
            break;
        }
        if s.kind == QuizSegmentKind::DocumentFact {
            let supported = s
                .citation_ids
                .iter()
                .any(|cid| valid_citation_ids.contains(cid));
            if supported {
                supported_fact = true;
            } else if !s.citation_ids.is_empty() {
                // 引用了不存在的 Citation ID ⇒ 结构无效。
                explanation_ok = false;
                break;
            }
        }
    }
    if explanation_ok && !supported_fact {
        explanation_ok = false;
    }
    if !explanation_ok {
        let reason = citation_reason.unwrap_or(R::ExplanationUnsupported);
        return (QuestionValidity::Invalid, Some(reason), checks);
    }
    if let Some(r) = citation_reason {
        return (QuestionValidity::Invalid, Some(r), checks);
    }
    // 同次生成去重:指纹重复的后续题目无效,不计数(《M11》§3)。
    let fp = question_fingerprint(q);
    if !seen_fingerprints.insert(fp) {
        return (
            QuestionValidity::Invalid,
            Some(R::DuplicateQuestion),
            checks,
        );
    }
    (QuestionValidity::Valid, None, checks)
}

/// 供摘要/作答路径复用的最小校验:已保存内容中的单题是否可计分
/// (validity=valid 且结构仍完整;内容经 M10 Codec 保存后不应漂移,
/// 此处只做防御性复核,不重写内容)。
pub fn question_is_scoreable(q: &QuizQuestion) -> bool {
    q.validity == QuestionValidity::Valid
        && !q.id.trim().is_empty()
        && (MIN_OPTIONS..=MAX_OPTIONS).contains(&q.options.len())
        && q.options.iter().any(|o| o.id == q.correct_option_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason_of(e: &crate::shared::AppError) -> String {
        match &e.details.as_ref().unwrap()["reason"] {
            crate::shared::error::DetailValue::Str(s) => s.clone(),
            _ => panic!("reason detail 不是字符串"),
        }
    }

    fn materials_fixture() -> QuizGenerationMaterials {
        let text = "知识管理系统的核心目标是在信息过载环境中保持可检索的证据链。\
                    检索阶段先限定范围再排序,禁止先对全文取 TopK 再过滤局部范围。\
                    引用校验保证每条文档事实都能回跳到原文块,历史证据不随后台重解析漂移。\
                    隐私边界要求模型请求在发送前经统一出口再次授权,密钥仅进入凭据存储。\
                    证据不足时返回明确状态,不以无依据的全文补写;取消后不保存半成品。\n                    每一次问答分别保存快照、检索清单与引用三层事实。";
        QuizGenerationMaterials {
            document_version_id: "v1".into(),
            blocks: vec![
                QuizMaterialBlock {
                    block_id: "b1".into(),
                    ordinal: 1,
                    text: text.into(),
                },
                QuizMaterialBlock {
                    block_id: "b2".into(),
                    ordinal: 2,
                    text: "快照在创建后不可修改;追问可复用同一快照,修改范围创建新快照。".into(),
                },
            ],
        }
    }

    fn valid_question(id: &str) -> QuizQuestion {
        QuizQuestion {
            id: id.into(),
            prompt: "局部范围检索的正确做法是什么?".into(),
            options: vec![
                QuizOption {
                    id: "a".into(),
                    text: "范围内检索,过滤先于排名".into(),
                },
                QuizOption {
                    id: "b".into(),
                    text: "全文 TopK 后再过滤局部".into(),
                },
                QuizOption {
                    id: "c".into(),
                    text: "只使用标题匹配".into(),
                },
                QuizOption {
                    id: "d".into(),
                    text: "随机抽样块".into(),
                },
            ],
            correct_option_id: "a".into(),
            explanation_segments: vec![QuizExplanationSegment {
                id: "e1".into(),
                kind: QuizSegmentKind::DocumentFact,
                text: "检索阶段先限定范围再排序,禁止先对全文取 TopK 再过滤局部范围。".into(),
                citation_ids: vec!["c1".into()],
            }],
            citations: vec![QuizCitation {
                id: "c1".into(),
                block_id: "b1".into(),
                quoted_text: "禁止先对全文取 TopK 再过滤局部范围".into(),
                claim_key: QuizClaimKey::Explanation,
            }],
            difficulty: QuestionDifficulty::Medium,
            validity: QuestionValidity::Unverified,
            invalid_reason: None,
        }
    }

    #[test]
    fn codec_rejects_wrong_schema_version_and_unknown_fields() {
        let c = QuizContentV1 {
            schema_version: QUIZ_SCHEMA_VERSION.into(),
            questions: vec![valid_question("q1")],
        };
        let json = quiz_content_to_json(&c).unwrap();
        assert!(parse_quiz_content(&json).is_ok());

        let wrong = json.replace("quiz.v1", "quiz.v2");
        assert!(parse_quiz_content(&wrong).is_err());

        let unknown = json.replace("\"questions\"", "\"extraField\":1,\"questions\"");
        assert!(parse_quiz_content(&unknown).is_err());
    }

    #[test]
    fn config_validation_bounds() {
        let mut cfg = QuizGenerationConfig::default();
        assert_eq!(cfg.count, 5);
        assert!(validate_generation_config(&cfg).is_ok());
        cfg.count = 0;
        assert!(validate_generation_config(&cfg).is_err());
        cfg.count = 11;
        assert!(validate_generation_config(&cfg).is_err());
        cfg.count = 10;
        cfg.difficulties = vec![];
        assert!(validate_generation_config(&cfg).is_err());
        cfg.difficulties = vec![QuestionDifficulty::Easy, QuestionDifficulty::Easy];
        assert!(validate_generation_config(&cfg).is_err());
        cfg.difficulties = vec![QuestionDifficulty::Easy, QuestionDifficulty::Hard];
        assert!(validate_generation_config(&cfg).is_ok());
        cfg.extra_instructions = "<script>x</script>".into();
        assert!(validate_generation_config(&cfg).is_err());
    }

    #[test]
    fn materials_validation_rejects_thin_selection() {
        let mut m = materials_fixture();
        assert!(validate_generation_materials(&m).is_ok());
        m.blocks.clear();
        let e = validate_generation_materials(&m).unwrap_err();
        assert_eq!(reason_of(&e), "quiz_material_no_evidence");

        let mut thin = materials_fixture();
        thin.blocks.truncate(1);
        thin.blocks[0].text = "太短".into();
        let e = validate_generation_materials(&thin).unwrap_err();
        assert_eq!(reason_of(&e), "quiz_material_too_short");

        let mut dup = materials_fixture();
        dup.blocks[1].block_id = dup.blocks[0].block_id.clone();
        assert!(validate_generation_materials(&dup).is_err());
    }

    #[test]
    fn valid_draft_passes_and_keeps_placeholders() {
        let m = materials_fixture();
        let mut q2 = valid_question("q2");
        q2.prompt = "快照创建后能否修改?".into();
        q2.correct_option_id = "a".into();
        q2.options[0].text = "不可修改,修改范围创建新快照".into();
        q2.options[1].text = "可以原地更新".into();
        q2.options[2].text = "随问答自动扩围".into();
        q2.options[3].text = "按标题合并".into();
        q2.citations[0].block_id = "b2".into();
        q2.citations[0].quoted_text = "快照在创建后不可修改".into();
        q2.explanation_segments[0].text = "快照在创建后不可修改;修改范围创建新快照。".into();
        let draft = QuizContentV1 {
            schema_version: QUIZ_SCHEMA_VERSION.into(),
            questions: vec![valid_question("q1"), q2],
        };
        let report = validate_generated_quiz(draft, &m);
        assert_eq!(report.valid_count, 2);
        assert_eq!(report.invalid_count, 0);
        assert!(report.invalid_question_ids().is_empty());
    }

    #[test]
    fn invalid_reasons_are_precise() {
        let m = materials_fixture();

        // 无 Citation ⇒ no_valid_citation。
        let mut no_cite = valid_question("q1");
        no_cite.citations.clear();
        no_cite.explanation_segments.clear();
        let r = validate_generated_quiz(
            QuizContentV1 {
                schema_version: QUIZ_SCHEMA_VERSION.into(),
                questions: vec![no_cite],
            },
            &m,
        );
        assert_eq!(
            r.outcomes[0].invalid_reason,
            Some(QuestionInvalidReason::NoValidCitation)
        );

        // 错块 ⇒ citation_target_missing。
        let mut wrong_block = valid_question("q1");
        wrong_block.citations[0].block_id = "b-other".into();
        let r = validate_generated_quiz(
            QuizContentV1 {
                schema_version: QUIZ_SCHEMA_VERSION.into(),
                questions: vec![wrong_block],
            },
            &m,
        );
        assert_eq!(
            r.outcomes[0].invalid_reason,
            Some(QuestionInvalidReason::CitationTargetMissing)
        );

        // 引文不匹配 ⇒ citation_quote_mismatch。
        let mut wrong_quote = valid_question("q1");
        wrong_quote.citations[0].quoted_text = "原文中不存在的句子".into();
        let r = validate_generated_quiz(
            QuizContentV1 {
                schema_version: QUIZ_SCHEMA_VERSION.into(),
                questions: vec![wrong_quote],
            },
            &m,
        );
        assert_eq!(
            r.outcomes[0].invalid_reason,
            Some(QuestionInvalidReason::CitationQuoteMismatch)
        );

        // 未知正确选项 ⇒ correct_option_unknown。
        let mut wrong_correct = valid_question("q1");
        wrong_correct.correct_option_id = "z".into();
        let r = validate_generated_quiz(
            QuizContentV1 {
                schema_version: QUIZ_SCHEMA_VERSION.into(),
                questions: vec![wrong_correct],
            },
            &m,
        );
        assert_eq!(
            r.outcomes[0].invalid_reason,
            Some(QuestionInvalidReason::CorrectOptionUnknown)
        );
    }

    #[test]
    fn duplicate_questions_are_invalid_but_kept_as_placeholder() {
        let m = materials_fixture();
        // 同指纹不同 ID:第二题按去重标记 invalid,保留占位。
        let draft = QuizContentV1 {
            schema_version: QUIZ_SCHEMA_VERSION.into(),
            questions: vec![valid_question("q1"), valid_question("q2")],
        };
        let report = validate_generated_quiz(draft, &m);
        assert_eq!(report.valid_count, 1);
        assert_eq!(report.content.questions.len(), 2);
        assert_eq!(report.outcomes[0].validity, QuestionValidity::Valid);
        assert_eq!(
            report.outcomes[1].invalid_reason,
            Some(QuestionInvalidReason::DuplicateQuestion)
        );
        assert_eq!(report.invalid_question_ids(), vec!["q2".to_string()]);

        // 同 ID 重复:第二题命中 QuestionIdInvalid。
        let same_id = QuizContentV1 {
            schema_version: QUIZ_SCHEMA_VERSION.into(),
            questions: vec![valid_question("q1"), valid_question("q1")],
        };
        let report = validate_generated_quiz(same_id, &m);
        assert_eq!(
            report.outcomes[1].invalid_reason,
            Some(QuestionInvalidReason::QuestionIdInvalid)
        );
    }

    #[test]
    fn quote_matching_supports_normalization() {
        let block = "跨行断词与  多余空白\n都应该在规范化匹配中命中。";
        assert!(quote_matches_block(block, "跨行断词与 多余空白 都应该"));
        assert!(quote_matches_block(block, "规范化匹配"));
        assert!(!quote_matches_block(block, "不存在的内容"));
        assert!(!quote_matches_block(block, ""));
    }
}
