//! quiz.v2:单选/多选/判断/填空/问答/混合。
//!
//! - 服务端裁决 validity/invalidReason;模型自报无效或有效都不被信任。
//! - 问答不做自动判分,返回 rubric 并明确标注“自评”;填空/多选按确定性规则。
//! - quiz.v1 读取器与作答记录保留不变。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::textnorm::{normalize_v1, normalized_hash};
use crate::shared::{AppError, CoreErrorCode};

use super::models::{
    QuestionDifficulty, QuestionInvalidReason, QuestionValidity, QuizCitation,
    QuizExplanationSegment, QuizOption, QuizSegmentKind,
};

pub const QUIZ_V2_SCHEMA_VERSION: &str = "quiz.v2";
pub const QUIZ_V2_INSTRUCTION_VERSION: &str = "quiz-instruction-v3";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuestionTypeV2 {
    SingleChoice,
    MultipleChoice,
    TrueFalse,
    FillBlank,
    ShortAnswer,
}

impl QuestionTypeV2 {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SingleChoice => "single_choice",
            Self::MultipleChoice => "multiple_choice",
            Self::TrueFalse => "true_false",
            Self::FillBlank => "fill_blank",
            Self::ShortAnswer => "short_answer",
        }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "single_choice" => Self::SingleChoice,
            "multiple_choice" => Self::MultipleChoice,
            "true_false" => Self::TrueFalse,
            "fill_blank" => Self::FillBlank,
            "short_answer" => Self::ShortAnswer,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizBlankV2 {
    pub id: String,
    pub accepted_answers: Vec<String>,
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizAnswerV2 {
    #[serde(default)]
    pub correct_option_ids: Vec<String>,
    /// 兼容模型沿用 quiz.v1 的 `correctOptionId`;服务端归一化后再校验。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub correct_option_id: Option<String>,
    #[serde(default)]
    pub reference_answer: String,
    #[serde(default)]
    pub rubric: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizQuestionV2 {
    pub id: String,
    #[serde(rename = "type")]
    pub question_type: QuestionTypeV2,
    pub prompt: String,
    #[serde(default)]
    pub options: Vec<QuizOption>,
    #[serde(default)]
    pub blanks: Vec<QuizBlankV2>,
    pub answer: QuizAnswerV2,
    pub explanation_segments: Vec<QuizExplanationSegment>,
    pub citations: Vec<QuizCitation>,
    pub difficulty: QuestionDifficulty,
    /// 兼容模型把 v1 的 correctOptionId 放在题目顶层。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub correct_option_id: Option<String>,
    #[serde(default = "default_validity")]
    pub validity: QuestionValidity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub invalid_reason: Option<QuestionInvalidReason>,
}

fn default_validity() -> QuestionValidity {
    QuestionValidity::Unverified
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizContentV2 {
    pub schema_version: String,
    pub questions: Vec<QuizQuestionV2>,
}

// —— 生成配置 ——

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizV2GenerationConfig {
    #[serde(default = "default_count")]
    pub count: u8,
    #[serde(default)]
    pub types: Vec<QuestionTypeV2>,
    #[serde(default)]
    pub difficulties: Vec<QuestionDifficulty>,
    #[serde(default)]
    pub style: String,
    /// 分题型数量:全 0 = 未指定,按 `count` 在 `types` 内自动分配;
    /// 一旦任一题型 > 0,总数 = 各题型之和(与 `count` 冲突时以分题型为准)。
    #[serde(default)]
    pub per_type_counts: QuizPerTypeCounts,
}

/// 分题型数量(单选/多选/判断/填空/问答)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizPerTypeCounts {
    #[serde(default)]
    pub single_choice: u8,
    #[serde(default)]
    pub multiple_choice: u8,
    #[serde(default)]
    pub true_false: u8,
    #[serde(default)]
    pub fill_blank: u8,
    #[serde(default)]
    pub short_answer: u8,
}

impl QuizV2GenerationConfig {
    /// 归一化:显式分题型时总数取各题型之和,并据此收敛 `types`。
    /// 总数超过上限或为 0 时返回原因(调用方转成可见错误)。
    pub fn normalized(mut self) -> Result<Self, &'static str> {
        const MAX_TOTAL: u8 = 20;
        if self.per_type_counts.is_explicit() {
            let total = self.per_type_counts.total();
            if total > MAX_TOTAL {
                return Err("quiz_count_out_of_range");
            }
            self.count = total;
            self.types = self
                .per_type_counts
                .entries()
                .into_iter()
                .map(|(kind, _)| kind)
                .collect();
        } else {
            if self.count == 0 || self.count > MAX_TOTAL {
                return Err("quiz_count_out_of_range");
            }
            if self.types.is_empty() {
                self.types = vec![
                    QuestionTypeV2::SingleChoice,
                    QuestionTypeV2::MultipleChoice,
                    QuestionTypeV2::TrueFalse,
                    QuestionTypeV2::FillBlank,
                    QuestionTypeV2::ShortAnswer,
                ];
            }
        }
        Ok(self)
    }

    /// 生成 Prompt 用的题型指令:显式分题型时逐项列出数量。
    pub fn type_instruction(&self) -> String {
        if self.per_type_counts.is_explicit() {
            self.per_type_counts
                .entries()
                .into_iter()
                .map(|(kind, count)| format!("{} × {}", kind.as_str(), count))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            self.types
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

impl QuizPerTypeCounts {
    /// 是否显式指定了任一题型数量。
    pub fn is_explicit(&self) -> bool {
        self.total() > 0
    }

    pub fn total(&self) -> u8 {
        self.single_choice
            .saturating_add(self.multiple_choice)
            .saturating_add(self.true_false)
            .saturating_add(self.fill_blank)
            .saturating_add(self.short_answer)
    }

    /// 展开为 (题型, 数量) 列表(数量为 0 的题型不出现)。
    pub fn entries(&self) -> Vec<(QuestionTypeV2, u8)> {
        [
            (QuestionTypeV2::SingleChoice, self.single_choice),
            (QuestionTypeV2::MultipleChoice, self.multiple_choice),
            (QuestionTypeV2::TrueFalse, self.true_false),
            (QuestionTypeV2::FillBlank, self.fill_blank),
            (QuestionTypeV2::ShortAnswer, self.short_answer),
        ]
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .collect()
    }
}

fn default_count() -> u8 {
    5
}

impl Default for QuizV2GenerationConfig {
    fn default() -> Self {
        Self {
            count: default_count(),
            types: vec![
                QuestionTypeV2::SingleChoice,
                QuestionTypeV2::MultipleChoice,
                QuestionTypeV2::TrueFalse,
                QuestionTypeV2::FillBlank,
                QuestionTypeV2::ShortAnswer,
            ],
            difficulties: vec![QuestionDifficulty::Medium],
            style: String::new(),
            per_type_counts: QuizPerTypeCounts::default(),
        }
    }
}

// —— 判分 ——

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuizGradingMode {
    Auto,
    SelfAssessed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizV2GradeResult {
    pub attempt_id: String,
    pub question_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub is_correct: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub score: Option<f64>,
    pub max_score: f64,
    pub grading: QuizGradingMode,
    pub correct_option_ids: Vec<String>,
    pub reference_answer: String,
    pub rubric: Vec<String>,
}

pub struct GradeInput<'a> {
    pub selected_option_ids: &'a [String],
    pub blank_answers: &'a [String],
    pub answer_text: Option<&'a str>,
}

/// 确定性判分:单选/判断/多选集合相等;填空逐空归一化匹配;问答自评。
pub fn grade_v2(question: &QuizQuestionV2, input: &GradeInput<'_>) -> QuizV2GradeResult {
    let max_score = 1.0;
    let selected: std::collections::BTreeSet<&str> = input
        .selected_option_ids
        .iter()
        .map(String::as_str)
        .collect();
    let correct: std::collections::BTreeSet<&str> = question
        .answer
        .correct_option_ids
        .iter()
        .map(String::as_str)
        .collect();
    let (is_correct, score, grading) = match question.question_type {
        QuestionTypeV2::SingleChoice
        | QuestionTypeV2::TrueFalse
        | QuestionTypeV2::MultipleChoice => {
            let hit = selected == correct;
            (
                Some(hit),
                Some(if hit { 1.0 } else { 0.0 }),
                QuizGradingMode::Auto,
            )
        }
        QuestionTypeV2::FillBlank => {
            if question.blanks.is_empty() {
                (Some(false), Some(0.0), QuizGradingMode::Auto)
            } else {
                let mut hits = 0usize;
                for (index, blank) in question.blanks.iter().enumerate() {
                    let Some(given) = input.blank_answers.get(index) else {
                        continue;
                    };
                    let given = normalize_fill(given, blank.case_sensitive);
                    if blank
                        .accepted_answers
                        .iter()
                        .any(|accepted| normalize_fill(accepted, blank.case_sensitive) == given)
                    {
                        hits += 1;
                    }
                }
                let score = hits as f64 / question.blanks.len() as f64;
                (
                    Some(hits == question.blanks.len()),
                    Some(score),
                    QuizGradingMode::Auto,
                )
            }
        }
        QuestionTypeV2::ShortAnswer => {
            // 不采信模型/关键词自动判分:返回 rubric,由用户自评。
            (None, None, QuizGradingMode::SelfAssessed)
        }
    };
    let _ = input.answer_text;
    QuizV2GradeResult {
        attempt_id: String::new(),
        question_id: question.id.clone(),
        is_correct,
        score,
        max_score,
        grading,
        correct_option_ids: question.answer.correct_option_ids.clone(),
        reference_answer: question.answer.reference_answer.clone(),
        rubric: question.answer.rubric.clone(),
    }
}

fn normalize_fill(text: &str, case_sensitive: bool) -> String {
    use unicode_normalization::UnicodeNormalization;
    let normalized: String = text.nfkc().collect::<String>().trim().to_string();
    if case_sensitive {
        normalized
    } else {
        normalized.to_lowercase()
    }
}

// —— 解析与校验 ——

fn quiz_error(reason: &str) -> AppError {
    AppError::new(
        CoreErrorCode::QuizQuestionInvalid,
        crate::shared::new_trace_id(),
    )
    .with_detail("reason", reason.into())
}

pub struct QuizV2Codec;

impl QuizV2Codec {
    pub fn kind(&self) -> &str {
        "quiz"
    }
    pub fn schema_version(&self) -> &str {
        QUIZ_V2_SCHEMA_VERSION
    }
    /// 写入前结构校验(不携带材料,不判定引文命中)。
    pub fn validate(&self, content: &serde_json::Value) -> bool {
        let Ok(parsed) = serde_json::from_value::<QuizContentV2>(content.clone()) else {
            return false;
        };
        if parsed.schema_version != QUIZ_V2_SCHEMA_VERSION || parsed.questions.is_empty() {
            return false;
        }
        let mut seen_ids = std::collections::HashSet::new();
        let mut seen_prompts = std::collections::HashSet::new();
        parsed.questions.iter().all(|q| {
            if q.id.trim().is_empty() || seen_ids.contains(&q.id) {
                return false;
            }
            // 无效题仍需可见保存,但不能借 invalid 绕过引文本身的结构检查。
            let mut citation_ids = std::collections::HashSet::new();
            if q.citations.iter().any(|c| {
                c.id.trim().is_empty()
                    || !citation_ids.insert(c.id.as_str())
                    || c.block_id.trim().is_empty()
                    || c.quoted_text.trim().is_empty()
            }) {
                return false;
            }
            if q.validity == QuestionValidity::Invalid {
                if q.invalid_reason.is_none() {
                    return false;
                }
            } else if validate_question_structure(q, &seen_ids, &seen_prompts).is_some() {
                return false;
            }
            seen_ids.insert(q.id.clone());
            if q.validity != QuestionValidity::Invalid {
                seen_prompts.insert(normalized_hash(&normalize_v1(&q.prompt).normalized));
            }
            true
        })
    }
}

pub fn parse_quiz_v2(raw: &str) -> Result<QuizContentV2, AppError> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|error| {
        quiz_error("content_unparseable").with_detail(
            "parse_error",
            crate::shared::error::DetailValue::Str(error.to_string()),
        )
    })?;
    if value["schemaVersion"].as_str() != Some(QUIZ_V2_SCHEMA_VERSION) {
        return Err(quiz_error("schema_version_mismatch"));
    }
    let questions = value["questions"]
        .as_array()
        .ok_or_else(|| quiz_error("content_unparseable"))?;
    // 读取兼容既有大批次;新生成上限由配置及 schema 的 20 题约束。
    if questions.is_empty() || questions.len() > 100 {
        return Err(quiz_error("question_count_out_of_range"));
    }
    let ids = repaired_ids(questions, |index| format!("q{}", index + 1), false);
    Ok(QuizContentV2 {
        schema_version: QUIZ_V2_SCHEMA_VERSION.into(),
        questions: questions
            .iter()
            .zip(ids)
            .map(|(question, id)| recover_question(question, id))
            .collect(),
    })
}

fn json_field<'a>(value: &'a serde_json::Value, names: &[&str]) -> &'a serde_json::Value {
    names
        .iter()
        .find_map(|name| value.get(*name).filter(|v| !v.is_null()))
        .unwrap_or(&serde_json::Value::Null)
}

fn json_text(value: &serde_json::Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn json_array(value: &serde_json::Value) -> &[serde_json::Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn json_strings(value: &serde_json::Value) -> Vec<String> {
    // 非字符串不能静默剔除,否则可能把错误答案集合缩成一个看似正确的集合。
    json_array(value)
        .iter()
        .map(|v| json_text(v).to_owned())
        .collect()
}

fn repaired_ids(
    values: &[serde_json::Value],
    fallback: impl Fn(usize) -> String,
    replace_duplicates: bool,
) -> Vec<String> {
    let reserved: std::collections::HashSet<&str> = values
        .iter()
        .map(|v| json_text(&v["id"]))
        .filter(|id| !id.trim().is_empty())
        .collect();
    let mut used = std::collections::HashSet::new();
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let original = json_text(&value["id"]);
            let generated =
                original.trim().is_empty() || (replace_duplicates && used.contains(original));
            let base = if generated {
                fallback(index)
            } else {
                original.into()
            };
            let mut id = base.clone();
            let mut suffix = 2;
            while used.contains(&id)
                || ((generated || id != base) && reserved.contains(id.as_str()))
            {
                id = format!("{base}-{suffix}");
                suffix += 1;
            }
            used.insert(id.clone());
            id
        })
        .collect()
}

/// 原始 ID 优先;重复 ID 不选第一个。只为缺 ID 的条目接受明确的位置别名。
fn repaired_reference(
    reference: &str,
    originals: &[serde_json::Value],
    ids: &[String],
    prefix: &str,
) -> Option<String> {
    let matches: Vec<usize> = originals
        .iter()
        .enumerate()
        .filter(|(_, value)| json_text(&value["id"]) == reference)
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Some(ids[*index].clone()),
        [] => originals.iter().enumerate().find_map(|(index, value)| {
            (json_text(&value["id"]).trim().is_empty()
                && (reference == format!("{prefix}{}", index + 1) || reference == ids[index]))
                .then(|| ids[index].clone())
        }),
        _ => None,
    }
}

fn model_question_type(value: &serde_json::Value) -> Option<QuestionTypeV2> {
    let name = json_text(value)
        .trim()
        .to_lowercase()
        .replace(['-', ' '], "_");
    Some(match name.as_str() {
        "single_choice" | "singlechoice" | "single" | "single_select" | "single_selection"
        | "mcq" | "单选" | "单选题" => QuestionTypeV2::SingleChoice,
        "multiple_choice" | "multiplechoice" | "multiple" | "multi_choice" | "multi_select"
        | "multiple_select" | "多选" | "多选题" => QuestionTypeV2::MultipleChoice,
        "true_false" | "truefalse" | "true/false" | "boolean" | "bool" | "judgment"
        | "judgement" | "判断" | "判断题" => QuestionTypeV2::TrueFalse,
        "fill_blank" | "fillblank" | "fill_in_blank" | "fill_in_the_blank"
        | "fill_in_the_blanks" | "cloze" | "填空" | "填空题" => QuestionTypeV2::FillBlank,
        "short_answer" | "shortanswer" | "short" | "essay" | "open_ended" | "free_response"
        | "问答" | "问答题" | "简答题" => QuestionTypeV2::ShortAnswer,
        _ => return None,
    })
}

fn recover_question(raw: &serde_json::Value, id: String) -> QuizQuestionV2 {
    use crate::shared::sanitize::sanitize_generated_markdown as sanitize;
    let kind = model_question_type(json_field(raw, &["type", "questionType", "question_type"]));
    let options = json_array(&raw["options"]);
    let option_ids = repaired_ids(options, |index| format!("o{}", index + 1), false);
    let answer = &raw["answer"];
    let correct_value = json_field(answer, &["correctOptionIds", "correct_option_ids"]);
    let malformed_correct = json_array(correct_value).iter().any(|v| !v.is_string());
    let mut correct = json_strings(correct_value);
    if correct.is_empty() {
        if let Some(single) = json_field(answer, &["correctOptionId", "correct_option_id"])
            .as_str()
            .or_else(|| json_field(raw, &["correctOptionId", "correct_option_id"]).as_str())
        {
            correct.push(single.into());
        }
    }
    let resolved: Option<Vec<String>> = if malformed_correct {
        None
    } else {
        correct
            .iter()
            .map(|reference| repaired_reference(reference, options, &option_ids, "o"))
            .collect()
    };
    let ambiguous_answer = resolved.is_none();
    let resolved = resolved.unwrap_or_default();
    let blanks = json_array(&raw["blanks"]);
    let blank_ids = repaired_ids(blanks, |index| format!("b{}", index + 1), false);
    let citations = json_array(&raw["citations"]);
    let citation_ids = repaired_ids(citations, |index| format!("{id}-c{}", index + 1), true);
    let segments = json_array(json_field(
        raw,
        &["explanationSegments", "explanation_segments"],
    ));
    let segment_ids = repaired_ids(segments, |index| format!("{id}-e{}", index + 1), true);
    let mut question = QuizQuestionV2 {
        id,
        // 未知题型只保留为无答案的自评容器,绝不猜题型对应的正确答案。
        question_type: kind.unwrap_or(QuestionTypeV2::ShortAnswer),
        prompt: sanitize(json_text(&raw["prompt"])),
        options: options
            .iter()
            .zip(option_ids)
            .map(|(option, id)| QuizOption {
                id,
                text: sanitize(json_text(&option["text"])),
            })
            .collect(),
        blanks: blanks
            .iter()
            .zip(blank_ids)
            .map(|(blank, id)| QuizBlankV2 {
                id,
                accepted_answers: json_strings(json_field(
                    blank,
                    &["acceptedAnswers", "answers", "accepted_answers"],
                )),
                case_sensitive: json_field(blank, &["caseSensitive", "case_sensitive"])
                    .as_bool()
                    .unwrap_or(false),
            })
            .collect(),
        answer: QuizAnswerV2 {
            correct_option_ids: if kind.is_some() { resolved } else { vec![] },
            // 归一化后移除旧单值入口,避免重读时重新引入有歧义的答案。
            correct_option_id: None,
            reference_answer: if kind.is_some() {
                json_text(json_field(answer, &["referenceAnswer", "reference_answer"])).into()
            } else {
                String::new()
            },
            rubric: if kind.is_some() {
                json_strings(&answer["rubric"])
            } else {
                vec![]
            },
        },
        explanation_segments: segments
            .iter()
            .zip(segment_ids)
            .map(|(segment, id)| {
                let kind = match json_text(&segment["kind"]) {
                    "document_fact" | "fact" => Some(QuizSegmentKind::DocumentFact),
                    "inference" => Some(QuizSegmentKind::Inference),
                    _ => None,
                };
                QuizExplanationSegment {
                    id,
                    kind: kind.unwrap_or(QuizSegmentKind::DocumentFact),
                    text: sanitize(json_text(&segment["text"])),
                    citation_ids: if kind.is_some() {
                        json_array(json_field(segment, &["citationIds", "citation_ids"]))
                            .iter()
                            .map(|reference| {
                                reference
                                    .as_str()
                                    .and_then(|reference| {
                                        repaired_reference(reference, citations, &citation_ids, "c")
                                    })
                                    .unwrap_or_default()
                            })
                            .collect()
                    } else {
                        vec![]
                    },
                }
            })
            .collect(),
        citations: citations
            .iter()
            .zip(citation_ids)
            .map(|(citation, id)| {
                // 只在 v2 的 Value 边界归一化,不改变共享的 quiz.v1 模型。
                serde_json::from_value(serde_json::json!({
                    "id": id,
                    "blockId": json_text(json_field(citation, &["blockId", "block_id"])),
                    "quotedText": json_text(json_field(citation, &["quotedText", "quoted_text"])),
                    "claimKey": "explanation"
                }))
                .expect("normalized citation has the fixed v2 structure")
            })
            .collect(),
        difficulty: serde_json::from_value(raw["difficulty"].clone())
            .unwrap_or(QuestionDifficulty::Medium),
        correct_option_id: None,
        // 读取已保存内容仍须保留 valid;生成路径随后重新执行服务端校验。
        validity: serde_json::from_value(raw["validity"].clone())
            .unwrap_or(QuestionValidity::Unverified),
        invalid_reason: serde_json::from_value(raw["invalidReason"].clone()).unwrap_or(None),
    };
    let reason = if kind.is_none() {
        Some(QuestionInvalidReason::SchemaInvalid)
    } else if ambiguous_answer
        && matches!(
            kind,
            Some(
                QuestionTypeV2::SingleChoice
                    | QuestionTypeV2::MultipleChoice
                    | QuestionTypeV2::TrueFalse
            )
        )
    {
        Some(QuestionInvalidReason::CorrectOptionUnknown)
    } else {
        validate_question_structure(&question, &Default::default(), &Default::default())
    };
    if let Some(reason) = reason {
        if ambiguous_answer
            || question.validity != QuestionValidity::Invalid
            || question.invalid_reason.is_none()
        {
            question.invalid_reason = Some(reason);
        }
        question.validity = QuestionValidity::Invalid;
    }
    question
}

/// 用材料校验每一题,返回 (规范化后的内容, 无效题数量)。
/// 无效题保留原因及计数;非法引文无论题目状态如何都剔除。
pub fn validate_quiz_v2(
    mut content: QuizContentV2,
    materials: &[(String, String)],
) -> (QuizContentV2, usize) {
    let blocks: std::collections::HashMap<&str, &str> = materials
        .iter()
        .map(|(id, text)| (id.as_str(), text.as_str()))
        .collect();
    let mut seen_ids = std::collections::HashSet::new();
    let mut seen_prompts = std::collections::HashSet::new();
    let mut invalid_count = 0usize;
    for question in &mut content.questions {
        question.validity = QuestionValidity::Valid;
        question.invalid_reason = None;
        let reason = validate_question(question, &blocks, &seen_ids, &seen_prompts);
        // 必须在所有题上执行,不能因答案/结构先失败而把伪造引文带进存储。
        let mut citation_ids = std::collections::HashSet::new();
        question.citations.retain(|citation| {
            !citation.id.trim().is_empty()
                && !citation.block_id.trim().is_empty()
                && blocks
                    .get(citation.block_id.as_str())
                    .is_some_and(|text| contains_verbatim(text, &citation.quoted_text))
                && citation_ids.insert(citation.id.clone())
        });
        for segment in &mut question.explanation_segments {
            segment.citation_ids.retain(|id| citation_ids.contains(id));
        }
        if let Some(reason) = reason {
            question.validity = QuestionValidity::Invalid;
            question.invalid_reason = Some(reason);
            invalid_count += 1;
        } else {
            seen_prompts.insert(normalized_hash(&normalize_v1(&question.prompt).normalized));
        }
        seen_ids.insert(question.id.clone());
    }
    (content, invalid_count)
}

fn validate_question(
    q: &QuizQuestionV2,
    blocks: &std::collections::HashMap<&str, &str>,
    seen_ids: &std::collections::HashSet<String>,
    seen_prompts: &std::collections::HashSet<String>,
) -> Option<QuestionInvalidReason> {
    if let Some(reason) = validate_question_structure(q, seen_ids, seen_prompts) {
        return Some(reason);
    }
    for citation in &q.citations {
        let Some(block_text) = blocks.get(citation.block_id.as_str()) else {
            return Some(QuestionInvalidReason::CitationTargetMissing);
        };
        if !contains_verbatim(block_text, &citation.quoted_text) {
            return Some(QuestionInvalidReason::CitationQuoteMismatch);
        }
    }
    None
}

fn validate_question_structure(
    q: &QuizQuestionV2,
    seen_ids: &std::collections::HashSet<String>,
    seen_prompts: &std::collections::HashSet<String>,
) -> Option<QuestionInvalidReason> {
    if q.id.trim().is_empty() || seen_ids.contains(&q.id) {
        return Some(QuestionInvalidReason::QuestionIdInvalid);
    }
    if q.prompt.trim().is_empty() || q.prompt.chars().count() > 2_000 {
        return Some(QuestionInvalidReason::SchemaInvalid);
    }
    if seen_prompts.contains(&normalized_hash(&normalize_v1(&q.prompt).normalized)) {
        return Some(QuestionInvalidReason::DuplicateQuestion);
    }
    let option_ids: std::collections::HashSet<&str> =
        q.options.iter().map(|o| o.id.as_str()).collect();
    if option_ids.len() != q.options.len()
        || q.options
            .iter()
            .any(|o| o.id.trim().is_empty() || o.text.trim().is_empty())
    {
        return Some(QuestionInvalidReason::OptionStructureInvalid);
    }
    let correct_ids: std::collections::HashSet<&str> = q
        .answer
        .correct_option_ids
        .iter()
        .map(String::as_str)
        .collect();
    if correct_ids.len() != q.answer.correct_option_ids.len() {
        return Some(QuestionInvalidReason::CorrectOptionUnknown);
    }
    match q.question_type {
        QuestionTypeV2::SingleChoice | QuestionTypeV2::TrueFalse => {
            if q.options.len() < 2 || q.options.len() > 6 {
                return Some(QuestionInvalidReason::OptionStructureInvalid);
            }
            if q.question_type == QuestionTypeV2::TrueFalse && q.options.len() != 2 {
                return Some(QuestionInvalidReason::OptionStructureInvalid);
            }
            if q.answer.correct_option_ids.len() != 1 {
                return Some(QuestionInvalidReason::CorrectOptionUnknown);
            }
            if !q
                .options
                .iter()
                .any(|o| o.id == q.answer.correct_option_ids[0])
            {
                return Some(QuestionInvalidReason::CorrectOptionUnknown);
            }
        }
        QuestionTypeV2::MultipleChoice => {
            if q.options.len() < 3 || q.options.len() > 6 {
                return Some(QuestionInvalidReason::OptionStructureInvalid);
            }
            if q.answer.correct_option_ids.len() < 2
                || q.answer.correct_option_ids.len() >= q.options.len()
            {
                return Some(QuestionInvalidReason::CorrectOptionUnknown);
            }
            if q.answer
                .correct_option_ids
                .iter()
                .any(|id| !q.options.iter().any(|o| &o.id == id))
            {
                return Some(QuestionInvalidReason::CorrectOptionUnknown);
            }
        }
        QuestionTypeV2::FillBlank => {
            if q.blanks.is_empty() || q.blanks.len() > 8 {
                return Some(QuestionInvalidReason::SchemaInvalid);
            }
            let mut blank_ids = std::collections::HashSet::new();
            for blank in &q.blanks {
                if blank.id.trim().is_empty()
                    || !blank_ids.insert(blank.id.as_str())
                    || blank.accepted_answers.is_empty()
                    || blank.accepted_answers.iter().any(|a| a.trim().is_empty())
                {
                    return Some(QuestionInvalidReason::SchemaInvalid);
                }
            }
        }
        QuestionTypeV2::ShortAnswer => {
            if q.answer.reference_answer.trim().is_empty()
                || q.answer.rubric.is_empty()
                || q.answer.rubric.iter().any(|point| point.trim().is_empty())
            {
                return Some(QuestionInvalidReason::SchemaInvalid);
            }
        }
    }
    if q.citations.is_empty() {
        return Some(QuestionInvalidReason::NoValidCitation);
    }
    let mut citation_ids = std::collections::HashSet::new();
    for citation in &q.citations {
        if citation.id.trim().is_empty() || !citation_ids.insert(citation.id.as_str()) {
            return Some(QuestionInvalidReason::SchemaInvalid);
        }
        if citation.block_id.trim().is_empty() {
            return Some(QuestionInvalidReason::CitationTargetMissing);
        }
        if citation.quoted_text.trim().is_empty() {
            return Some(QuestionInvalidReason::CitationQuoteMismatch);
        }
    }
    if q.explanation_segments.is_empty() {
        return Some(QuestionInvalidReason::ExplanationUnsupported);
    }
    let mut segment_ids = std::collections::HashSet::new();
    for segment in &q.explanation_segments {
        if segment.id.trim().is_empty()
            || !segment_ids.insert(segment.id.as_str())
            || segment.text.trim().is_empty()
        {
            return Some(QuestionInvalidReason::SchemaInvalid);
        }
        if segment.kind == QuizSegmentKind::DocumentFact
            && (segment.citation_ids.is_empty()
                || segment
                    .citation_ids
                    .iter()
                    .any(|id| !citation_ids.contains(id.as_str())))
        {
            return Some(QuestionInvalidReason::ExplanationUnsupported);
        }
    }
    None
}

fn contains_verbatim(haystack: &str, needle: &str) -> bool {
    !needle.trim().is_empty() && haystack.contains(needle)
}

pub fn quiz_v2_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["schemaVersion", "questions"],
        "properties": {
            "schemaVersion": {"const": QUIZ_V2_SCHEMA_VERSION},
            "questions": {
                "type": "array",
                "minItems": 1,
                "maxItems": 20,
                "items": {
                    "type": "object",
                    "additionalProperties": true,
                    "required": ["id", "type", "prompt", "answer", "explanationSegments", "citations", "difficulty"],
                    "properties": {
                        "id": {"type": "string"},
                        "type": {"enum": ["single_choice","multiple_choice","true_false","fill_blank","short_answer"]},
                        "prompt": {"type": "string"},
                        "options": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": true,
                                "required": ["id", "text"],
                                "properties": {
                                    "id": {"type": "string"},
                                    "text": {"type": "string"}
                                }
                            }
                        },
                        "blanks": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": true,
                                "required": ["id", "acceptedAnswers"],
                                "properties": {
                                    "id": {"type": "string"},
                                    "acceptedAnswers": {"type": "array", "items": {"type": "string"}},
                                    "caseSensitive": {"type": "boolean"}
                                }
                            }
                        },
                        "answer": {
                            "type": "object",
                            "additionalProperties": true,
                            "properties": {
                                "correctOptionIds": {"type": "array", "items": {"type": "string"}},
                                "correctOptionId": {"type": "string"},
                                "referenceAnswer": {"type": "string"},
                                "rubric": {"type": "array", "items": {"type": "string"}}
                            }
                        },
                        "explanationSegments": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "additionalProperties": true,
                                "required": ["id", "kind", "text", "citationIds"],
                                "properties": {
                                    "id": {"type": "string"},
                                    "kind": {"enum": ["document_fact", "inference"]},
                                    "text": {"type": "string"},
                                    "citationIds": {"type": "array", "items": {"type": "string"}}
                                }
                            }
                        },
                        "citations": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "additionalProperties": true,
                                "required": ["id", "blockId", "quotedText", "claimKey"],
                                "properties": {
                                    "id": {"type": "string"},
                                    "blockId": {"type": "string"},
                                    "quotedText": {"type": "string"},
                                    "claimKey": {"enum": ["explanation"]}
                                }
                            }
                        },
                        "difficulty": {"enum": ["easy","medium","hard"]}
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quiz::models::{QuizClaimKey, QuizOption};

    fn citation() -> QuizCitation {
        QuizCitation {
            id: "c1".into(),
            block_id: "b1".into(),
            quoted_text: "快照不可变".into(),
            claim_key: QuizClaimKey::Explanation,
        }
    }

    fn explanation() -> Vec<QuizExplanationSegment> {
        vec![QuizExplanationSegment {
            id: "e1".into(),
            kind: QuizSegmentKind::DocumentFact,
            text: "快照不可变".into(),
            citation_ids: vec!["c1".into()],
        }]
    }

    fn single() -> QuizQuestionV2 {
        QuizQuestionV2 {
            id: "q1".into(),
            question_type: QuestionTypeV2::SingleChoice,
            prompt: "快照能否修改?".into(),
            options: vec![
                QuizOption {
                    id: "a".into(),
                    text: "不能".into(),
                },
                QuizOption {
                    id: "b".into(),
                    text: "能".into(),
                },
            ],
            blanks: vec![],
            answer: QuizAnswerV2 {
                correct_option_ids: vec!["a".into()],
                correct_option_id: None,
                reference_answer: String::new(),
                rubric: vec![],
            },
            explanation_segments: explanation(),
            citations: vec![citation()],
            difficulty: QuestionDifficulty::Easy,
            correct_option_id: None,
            validity: QuestionValidity::Unverified,
            invalid_reason: None,
        }
    }

    #[test]
    fn grades_single_and_multiple_by_set_equality() {
        let q = single();
        let correct = grade_v2(
            &q,
            &GradeInput {
                selected_option_ids: &["a".into()],
                blank_answers: &[],
                answer_text: None,
            },
        );
        assert_eq!(correct.is_correct, Some(true));
        let wrong = grade_v2(
            &q,
            &GradeInput {
                selected_option_ids: &["b".into()],
                blank_answers: &[],
                answer_text: None,
            },
        );
        assert_eq!(wrong.is_correct, Some(false));
    }

    #[test]
    fn fill_blank_normalizes_and_scores_per_blank() {
        let mut q = single();
        q.question_type = QuestionTypeV2::FillBlank;
        q.options = vec![];
        q.blanks = vec![
            QuizBlankV2 {
                id: "b1".into(),
                accepted_answers: vec!["快照".into()],
                case_sensitive: false,
            },
            QuizBlankV2 {
                id: "b2".into(),
                accepted_answers: vec!["不可变".into()],
                case_sensitive: false,
            },
        ];
        q.answer = QuizAnswerV2 {
            correct_option_ids: vec![],
            correct_option_id: None,
            reference_answer: String::new(),
            rubric: vec![],
        };
        let result = grade_v2(
            &q,
            &GradeInput {
                selected_option_ids: &[],
                blank_answers: &[" 快照 ".into(), "错误".into()],
                answer_text: None,
            },
        );
        assert_eq!(result.score, Some(0.5));
        assert_eq!(result.is_correct, Some(false));
    }

    #[test]
    fn short_answer_is_self_assessed_with_rubric() {
        let mut q = single();
        q.question_type = QuestionTypeV2::ShortAnswer;
        q.options = vec![];
        q.answer = QuizAnswerV2 {
            correct_option_ids: vec![],
            correct_option_id: None,
            reference_answer: "参考答案".into(),
            rubric: vec!["要点一".into()],
        };
        let result = grade_v2(
            &q,
            &GradeInput {
                selected_option_ids: &[],
                blank_answers: &[],
                answer_text: Some("我的回答"),
            },
        );
        assert_eq!(result.grading, QuizGradingMode::SelfAssessed);
        assert_eq!(result.is_correct, None);
        assert_eq!(result.rubric, vec!["要点一".to_string()]);
    }

    #[test]
    fn invalid_question_is_retained_and_model_validity_is_not_trusted() {
        let mut q = single();
        q.validity = QuestionValidity::Valid; // 模型自报有效必须被覆盖
        q.citations[0].quoted_text = "不存在的原文".into();
        let content = QuizContentV2 {
            schema_version: QUIZ_V2_SCHEMA_VERSION.into(),
            questions: vec![q],
        };
        let (validated, invalid) =
            validate_quiz_v2(content, &[("b1".into(), "快照不可变。".into())]);
        assert_eq!(invalid, 1);
        assert_eq!(validated.questions.len(), 1);
        assert_eq!(validated.questions[0].validity, QuestionValidity::Invalid);
        assert_eq!(
            validated.questions[0].invalid_reason,
            Some(QuestionInvalidReason::CitationQuoteMismatch)
        );
        assert!(validated.questions[0].citations.is_empty());
        assert!(validated.questions[0].explanation_segments[0]
            .citation_ids
            .is_empty());
        assert!(QuizV2Codec.validate(&serde_json::to_value(&validated).unwrap()));
    }

    fn raw_single() -> serde_json::Value {
        serde_json::to_value(single()).unwrap()
    }

    fn parse_values(questions: Vec<serde_json::Value>) -> QuizContentV2 {
        parse_quiz_v2(
            &serde_json::json!({
                "schemaVersion": QUIZ_V2_SCHEMA_VERSION, "questions": questions, "extra": true
            })
            .to_string(),
        )
        .unwrap()
    }

    fn validate_values(questions: Vec<serde_json::Value>) -> (QuizContentV2, usize) {
        validate_quiz_v2(
            parse_values(questions),
            &[("b1".into(), "快照不可变。".into())],
        )
    }

    #[test]
    fn missing_ids_claim_keys_and_unknown_fields_still_grade() {
        use serde_json::json;
        for claim in [serde_json::Value::Null, json!("unknown_claim")] {
            let mut q = json!({
                "questionType": "singleChoice", "prompt": "快照能否修改?", "extra": true,
                "options": [{"text": "不能", "extra": true}, {"id": "", "text": "能"}],
                "answer": {"correctOptionIds": ["o1"], "extra": true},
                "explanationSegments": [{"kind": "fact", "text": "快照不可变", "citationIds": ["c1"], "extra": true}],
                "citations": [{"blockId": "b1", "quotedText": "快照不可变", "claimKey": claim, "extra": true}]
            });
            if q["citations"][0]["claimKey"].is_null() {
                q["citations"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("claimKey");
            }
            let (content, invalid) = validate_values(vec![q]);
            assert_eq!(invalid, 0);
            let q = &content.questions[0];
            assert_eq!(q.id, "q1");
            assert_eq!(
                q.options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
                ["o1", "o2"]
            );
            assert_eq!(q.citations[0].id, "q1-c1");
            assert_eq!(q.citations[0].claim_key, QuizClaimKey::Explanation);
            assert_eq!(q.explanation_segments[0].id, "q1-e1");
            assert_eq!(
                q.explanation_segments[0].kind,
                QuizSegmentKind::DocumentFact
            );
            assert_eq!(q.explanation_segments[0].citation_ids, ["q1-c1"]);
            assert_eq!(q.difficulty, QuestionDifficulty::Medium);
            assert!(q.answer.reference_answer.is_empty() && q.answer.rubric.is_empty());
            assert_eq!(
                grade_v2(
                    q,
                    &GradeInput {
                        selected_option_ids: &["o1".into()],
                        blank_answers: &[],
                        answer_text: None
                    }
                )
                .is_correct,
                Some(true)
            );
            assert!(QuizV2Codec.validate(&serde_json::to_value(&content).unwrap()));
            let reread = parse_quiz_v2(&serde_json::to_string(&content).unwrap()).unwrap();
            assert_eq!(reread, content, "已保存 valid 题重读仍可作答");
        }
    }

    #[test]
    fn id_repairs_reserve_explicit_names_and_replace_duplicate_child_ids() {
        use serde_json::json;
        let mut q = raw_single();
        q["id"] = json!("");
        q["citations"] = json!([
            {"id": "c", "blockId": "b1", "quotedText": "快照不可变"},
            {"id": "c", "blockId": "b1", "quotedText": "不可变"}
        ]);
        q["explanationSegments"] = json!([
            {"id": "e", "kind": "inference", "text": "推论一"},
            {"id": "e", "kind": "inference", "text": "推论二"}
        ]);
        let mut other = raw_single();
        other["prompt"] = json!("第二题?");
        let content = parse_values(vec![q, other.clone(), other]);
        assert_eq!(
            content
                .questions
                .iter()
                .map(|q| q.id.as_str())
                .collect::<Vec<_>>(),
            ["q1-2", "q1", "q1-3"]
        );
        assert_eq!(content.questions[0].citations[1].id, "q1-2-c2");
        assert_eq!(content.questions[0].explanation_segments[1].id, "q1-2-e2");
        assert!(content.questions[0].explanation_segments[0]
            .citation_ids
            .is_empty());
        let ids = repaired_ids(
            &[
                json!({}),
                json!({"id":"o1"}),
                json!({"id":"o1"}),
                json!({"id":"o1-2"}),
            ],
            |i| format!("o{}", i + 1),
            false,
        );
        assert_eq!(ids, ["o1-3", "o1", "o1-4", "o1-2"]);
    }

    #[test]
    fn duplicate_distractors_preserve_unambiguous_correct_options() {
        use serde_json::json;
        for multiple in [false, true] {
            let mut q = raw_single();
            q["options"] = json!([
                {"id":"a","text":"甲"}, {"id":"b","text":"乙"},
                {"id":"d","text":"丙"}, {"id":"d","text":"丁"}
            ]);
            q["type"] = json!(if multiple {
                "multiple_choice"
            } else {
                "single_choice"
            });
            q["answer"] = if multiple {
                json!({"correctOptionIds":["a","b"]})
            } else {
                json!({"correctOptionId":"a"})
            };
            let (content, invalid) = validate_values(vec![q]);
            assert_eq!(invalid, 0);
            let q = &content.questions[0];
            assert_eq!(q.options[3].id, "d-2");
            assert_eq!(
                grade_v2(
                    q,
                    &GradeInput {
                        selected_option_ids: &q.answer.correct_option_ids,
                        blank_answers: &[],
                        answer_text: None
                    }
                )
                .is_correct,
                Some(true)
            );
            assert_eq!(
                grade_v2(
                    q,
                    &GradeInput {
                        selected_option_ids: &["d-2".into()],
                        blank_answers: &[],
                        answer_text: None
                    }
                )
                .is_correct,
                Some(false)
            );
        }
    }

    #[test]
    fn ambiguous_or_malformed_correct_references_never_guess_or_resurrect() {
        use serde_json::json;
        for (ids, answer) in [
            (["a", "a"], json!({"correctOptionIds":["a"]})),
            (["a", "a"], json!({"correctOptionId":"a"})),
            (["a", "a"], json!({"correctOptionIds":["a-2"]})),
            (["", ""], json!({"correctOptionIds":[""]})),
            (["", "b"], json!({"correctOptionIds":[null]})),
        ] {
            let mut q = raw_single();
            q["options"][0]["id"] = json!(ids[0]);
            q["options"][1]["id"] = json!(ids[1]);
            q["answer"] = answer;
            q["correctOptionId"] = json!("a");
            q["validity"] = json!("valid");
            let parsed = parse_values(vec![q]);
            assert_eq!(
                parsed.questions[0].invalid_reason,
                Some(QuestionInvalidReason::CorrectOptionUnknown)
            );
            assert!(parsed.questions[0].answer.correct_option_ids.is_empty());
            let (content, invalid) =
                validate_quiz_v2(parsed, &[("b1".into(), "快照不可变".into())]);
            assert_eq!(invalid, 1);
            assert_eq!(
                content.questions[0].invalid_reason,
                Some(QuestionInvalidReason::CorrectOptionUnknown)
            );
            let reread = parse_quiz_v2(&serde_json::to_string(&content).unwrap()).unwrap();
            assert!(reread.questions[0].answer.correct_option_ids.is_empty());
            assert_eq!(reread.questions[0].validity, QuestionValidity::Invalid);
            assert!(QuizV2Codec.validate(&serde_json::to_value(content).unwrap()));
        }
    }

    #[test]
    fn unique_empty_option_id_updates_both_legacy_answer_locations() {
        use serde_json::json;
        for top_level in [false, true] {
            let mut q = raw_single();
            q["options"][0]["id"] = json!("");
            q["answer"] = json!({});
            if top_level {
                q["correctOptionId"] = json!("");
            } else {
                q["answer"]["correctOptionId"] = json!("");
            }
            let (content, invalid) = validate_values(vec![q]);
            assert_eq!(invalid, 0);
            assert_eq!(content.questions[0].answer.correct_option_ids, ["o1"]);
        }
    }

    #[test]
    fn five_mixed_types_recover_and_keep_server_grading() {
        use serde_json::json;
        let mut questions = Vec::new();
        for (index, alias) in [
            "single",
            "multi_select",
            "boolean",
            "fill_in_the_blank",
            "essay",
        ]
        .iter()
        .enumerate()
        {
            let mut q = raw_single();
            q["id"] = json!("");
            q["type"] = json!(alias);
            q["prompt"] = json!(format!("第{}题", index + 1));
            if index == 1 {
                q["options"] =
                    json!([{"id":"a","text":"甲"},{"id":"b","text":"乙"},{"id":"c","text":"丙"}]);
                q["answer"] = json!({"correctOptionIds":["a","b"]});
            } else if index == 3 {
                q["blanks"] = json!([
                    {"answers":["快照"], "extra":true},
                    {"id":"x", "accepted_answers":["不可变"]},
                    {"id":"x", "acceptedAnswers":["ABC"]}
                ]);
                q.as_object_mut().unwrap().remove("answer");
            } else if index == 4 {
                q["answer"] = json!({"referenceAnswer":"快照不可变", "rubric":["不可变性"]});
            }
            questions.push(q);
        }
        let (content, invalid) = validate_values(questions);
        assert_eq!(invalid, 0);
        assert_eq!(content.questions.len(), 5);
        assert_eq!(
            content.questions[3]
                .blanks
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>(),
            ["b1", "x", "x-2"]
        );
        assert!(content.questions[3].answer.reference_answer.is_empty());
        for (index, q) in content.questions.iter().enumerate() {
            let selected = if index == 1 {
                vec!["b".into(), "a".into()]
            } else {
                vec!["a".into()]
            };
            let result = grade_v2(
                q,
                &GradeInput {
                    selected_option_ids: &selected,
                    blank_answers: &["快照".into(), "不可变".into(), " abc ".into()],
                    answer_text: Some("自评"),
                },
            );
            if index == 4 {
                assert_eq!(result.grading, QuizGradingMode::SelfAssessed);
                assert_eq!(result.is_correct, None);
                assert_eq!(result.rubric, ["不可变性"]);
            } else {
                assert_eq!(result.grading, QuizGradingMode::Auto);
                assert_eq!(result.is_correct, Some(true));
            }
        }
        assert!(QuizV2Codec.validate(&serde_json::to_value(content).unwrap()));
    }

    #[test]
    fn malformed_questions_stay_visible_with_reasons_and_counts() {
        use serde_json::json;
        let mut no_citation = raw_single();
        no_citation["citations"] = json!([]);
        let mut no_answer = raw_single();
        no_answer.as_object_mut().unwrap().remove("answer");
        let mut malformed = raw_single();
        malformed["options"] = json!([null, {"text":42}]);
        malformed["explanationSegments"] = json!([{"kind":"mystery"}]);
        let mut unknown_type = raw_single();
        unknown_type["type"] = json!("unknown");
        let mut good = raw_single();
        good["validity"] = json!("invalid");
        good["invalidReason"] = json!("schema_invalid");
        let (content, invalid) = validate_values(vec![
            no_citation,
            no_answer,
            malformed,
            unknown_type,
            json!(null),
            good,
        ]);
        assert_eq!(content.questions.len(), 6);
        assert_eq!(invalid, 5);
        assert_eq!(
            content.questions[0].invalid_reason,
            Some(QuestionInvalidReason::NoValidCitation)
        );
        assert_eq!(
            content.questions[1].invalid_reason,
            Some(QuestionInvalidReason::CorrectOptionUnknown)
        );
        assert!(content.questions[..5]
            .iter()
            .all(|q| q.validity == QuestionValidity::Invalid && q.invalid_reason.is_some()));
        assert_eq!(content.questions[5].validity, QuestionValidity::Valid);
        assert!(QuizV2Codec.validate(&serde_json::to_value(&content).unwrap()));
        let mut value = serde_json::to_value(content).unwrap();
        value["questions"][0]["validity"] = json!("valid");
        assert!(!QuizV2Codec.validate(&value), "合法题不能放宽引文检查");
        value["questions"][0]["validity"] = json!("invalid");
        value["questions"][0]
            .as_object_mut()
            .unwrap()
            .remove("invalidReason");
        assert!(!QuizV2Codec.validate(&value), "invalid 必须附带原因");
    }

    #[test]
    fn forged_nonverbatim_and_empty_quotes_are_always_removed() {
        use serde_json::json;
        for (block, quote, reason) in [
            (
                "missing",
                "快照不可变",
                QuestionInvalidReason::CitationTargetMissing,
            ),
            (
                "b1",
                "快照 可变",
                QuestionInvalidReason::CitationQuoteMismatch,
            ),
            ("b1", "", QuestionInvalidReason::CitationQuoteMismatch),
            ("b1", "   ", QuestionInvalidReason::CitationQuoteMismatch),
        ] {
            for bad_answer in [false, true] {
                let mut q = raw_single();
                q["citations"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id":"bad", "blockId":block, "quotedText":quote}));
                q["explanationSegments"][0]["citationIds"] = json!(["c1", "bad"]);
                if bad_answer {
                    q["answer"] = json!({});
                }
                let (content, invalid) = validate_values(vec![q]);
                assert_eq!(invalid, 1);
                let q = &content.questions[0];
                assert_eq!(q.citations.len(), 1);
                assert_eq!(q.citations[0].id, "c1");
                assert_eq!(q.explanation_segments[0].citation_ids, ["c1"]);
                assert_eq!(
                    q.invalid_reason,
                    Some(if bad_answer {
                        QuestionInvalidReason::CorrectOptionUnknown
                    } else {
                        reason
                    })
                );
            }
        }
        assert!(!contains_verbatim("ABC  def", "abc  def"));
        assert!(!contains_verbatim("ABC  def", "ABC def"));
        assert!(contains_verbatim("ABC  def", "ABC  def"));
    }

    #[test]
    fn missing_or_ambiguous_fact_references_are_not_invented() {
        use serde_json::json;
        for references in [json!(null), json!(["c1", "unknown"]), json!([null])] {
            let mut q = raw_single();
            q["explanationSegments"][0]["citationIds"] = references;
            let (content, invalid) = validate_values(vec![q]);
            assert_eq!(invalid, 1);
            assert_eq!(
                content.questions[0].invalid_reason,
                Some(QuestionInvalidReason::ExplanationUnsupported)
            );
        }
        let mut q = raw_single();
        q["citations"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::to_value(citation()).unwrap());
        let (content, invalid) = validate_values(vec![q]);
        assert_eq!(invalid, 1);
        assert_eq!(content.questions[0].citations[1].id, "q1-c2");
        assert!(content.questions[0].explanation_segments[0]
            .citation_ids
            .is_empty());
    }

    #[test]
    fn schema_allows_twenty_and_batch_envelope_remains_strict() {
        use serde_json::json;
        assert_eq!(quiz_v2_schema()["properties"]["questions"]["maxItems"], 20);
        let questions = (0..20)
            .map(|index| {
                let mut q = raw_single();
                q["prompt"] = json!(format!("问题{index}"));
                q
            })
            .collect();
        let (content, invalid) = validate_values(questions);
        assert_eq!(invalid, 0);
        assert_eq!(content.questions.len(), 20);
        assert!(QuizV2Codec.validate(&serde_json::to_value(content).unwrap()));
        for raw in [
            "{",
            "{}",
            r#"{"schemaVersion":"quiz.v1","questions":[]}"#,
            r#"{"schemaVersion":"quiz.v2","questions":null}"#,
            r#"{"schemaVersion":"quiz.v2","questions":[]}"#,
        ] {
            assert!(parse_quiz_v2(raw).is_err());
        }
        assert!(parse_quiz_v2(
            &json!({"schemaVersion":"quiz.v2", "questions":vec![json!(null);101]}).to_string()
        )
        .is_err());
    }
}

#[cfg(test)]
mod per_type_tests {
    use super::*;

    #[test]
    fn per_type_counts_override_total_and_types() {
        let config = QuizV2GenerationConfig {
            count: 5,
            types: vec![QuestionTypeV2::SingleChoice],
            per_type_counts: QuizPerTypeCounts {
                single_choice: 2,
                multiple_choice: 1,
                true_false: 0,
                fill_blank: 3,
                short_answer: 1,
            },
            ..QuizV2GenerationConfig::default()
        }
        .normalized()
        .unwrap();
        // 总数 = 各题型之和,与 count 冲突时以分题型为准。
        assert_eq!(config.count, 7);
        assert_eq!(
            config.types,
            vec![
                QuestionTypeV2::SingleChoice,
                QuestionTypeV2::MultipleChoice,
                QuestionTypeV2::FillBlank,
                QuestionTypeV2::ShortAnswer
            ],
            "数量为 0 的题型不进入 types"
        );
        assert_eq!(
            config.type_instruction(),
            "single_choice × 2, multiple_choice × 1, fill_blank × 3, short_answer × 1"
        );
    }

    #[test]
    fn per_type_zero_everywhere_falls_back_to_count() {
        let config = QuizV2GenerationConfig {
            count: 4,
            types: Vec::new(),
            per_type_counts: QuizPerTypeCounts::default(),
            ..QuizV2GenerationConfig::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(config.count, 4);
        assert_eq!(config.types.len(), 5, "未指定时回到默认五题型");
        assert!(!config.per_type_counts.is_explicit());
    }

    #[test]
    fn per_type_total_over_limit_is_rejected() {
        let error = QuizV2GenerationConfig {
            per_type_counts: QuizPerTypeCounts {
                single_choice: 10,
                multiple_choice: 10,
                true_false: 5,
                fill_blank: 0,
                short_answer: 0,
            },
            ..QuizV2GenerationConfig::default()
        }
        .normalized()
        .unwrap_err();
        assert_eq!(error, "quiz_count_out_of_range");
    }

    #[test]
    fn plain_count_zero_or_too_large_is_rejected() {
        assert_eq!(
            QuizV2GenerationConfig {
                count: 0,
                ..QuizV2GenerationConfig::default()
            }
            .normalized()
            .unwrap_err(),
            "quiz_count_out_of_range"
        );
        assert_eq!(
            QuizV2GenerationConfig {
                count: 21,
                ..QuizV2GenerationConfig::default()
            }
            .normalized()
            .unwrap_err(),
            "quiz_count_out_of_range"
        );
    }
    #[test]
    fn parse_tolerates_missing_nested_ids_and_extra_model_fields() {
        let raw = r#"{
          "schemaVersion":"quiz.v2",
          "questions":[{
            "id":"q1","type":"single_choice","prompt":"预算阈值?","difficulty":"medium",
            "options":[{"id":"a","text":"500 万"},{"id":"b","text":"100 万"}],
            "answer":{"correctOptionIds":["a"]},
            "explanationSegments":[{"kind":"document_fact","text":"原文写 500 万","citationIds":["c1"],"extra":"ignored"}],
            "citations":[{"blockId":"b1","quotedText":"预算阈值为 500 万元","claimKey":"explanation","extra":"ignored"}],
            "modelComment":"extra field"
          }]
        }"#;
        let content = parse_quiz_v2(raw).expect("宽容解析");
        let q = &content.questions[0];
        assert_eq!(q.explanation_segments[0].id, "q1-e1");
        assert_eq!(q.citations[0].id, "q1-c1");
        assert_eq!(
            q.citations[0].claim_key,
            crate::quiz::QuizClaimKey::Explanation
        );
    }
}
