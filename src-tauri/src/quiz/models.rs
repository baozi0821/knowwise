//! M11 领域对象与对外 DTO(《01》§4.7/§5.4、《M11》§2–§8)。
//!
//! 契约:
//! - 题目内容本体是 `quiz.v1` ArtifactVersion 内容,归 M10 保存;
//!   M11 只实现题目 Codec、生成校验、作答记录与无效题反馈。
//! - 每道正式计分题必须有唯一合理答案、解析和有效原文依据;
//!   无依据、多解或结构无效的题目标 invalid,不计错,可反馈并重新生成。
//! - `validity` 是结构校验结论,不是语义唯一性的保证:系统校验通过只说明
//!   题面/答案/依据结构完整(《M11》§4)。
//! - 作答记录只呈现事实,不推断"掌握/未掌握"。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::paging::Page;

/// quiz.v1 Schema 常量;升级版本必须保留 quiz.v1 读取器(《01》§10)。
pub const QUIZ_SCHEMA_VERSION: &str = "quiz.v1";

// —— 枚举 ——

/// 题目难度:easy/medium/hard,生成时可组合(《M11》§2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuestionDifficulty {
    Easy,
    Medium,
    Hard,
}

impl QuestionDifficulty {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Medium => "medium",
            Self::Hard => "hard",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "easy" => Self::Easy,
            "medium" => Self::Medium,
            "hard" => Self::Hard,
            _ => return None,
        })
    }
}

/// 题目有效性状态(《M11》§4)。
/// - `unverified`:模型原始输出,未经结构校验。
/// - `valid`:结构校验通过(选项、唯一答案、解析、≥1 条结构有效 Citation)。
/// - `invalid`:结构无效;不计分,可反馈并重新生成。
/// - `disputed`:用户反馈存在争议(如多解);由用户编辑版本或后续流程携带,
///   生成校验不会自动产生该状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuestionValidity {
    Unverified,
    Valid,
    Invalid,
    Disputed,
}

impl QuestionValidity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Disputed => "disputed",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "unverified" => Self::Unverified,
            "valid" => Self::Valid,
            "invalid" => Self::Invalid,
            "disputed" => Self::Disputed,
            _ => return None,
        })
    }

    /// 该状态是否进入正确率统计:只有 valid 计分(《M11》§5)。
    pub fn counts_toward_score(&self) -> bool {
        matches!(self, Self::Valid)
    }
}

/// 题目标记 invalid 的机器可读原因(封闭枚举,呈现给界面与测试)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuestionInvalidReason {
    /// 题干/选项/解析结构不完整或长度非法。
    SchemaInvalid,
    /// 选项 ID 重复、数量越界或文本为空。
    OptionStructureInvalid,
    /// 正确选项 ID 未知(不在选项集合内)。
    CorrectOptionUnknown,
    /// 没有任何结构有效的 Citation(无依据题)。
    NoValidCitation,
    /// Citation 指向的块不在本次材料/快照范围内。
    CitationTargetMissing,
    /// Citation 引文与目标块原文不匹配。
    CitationQuoteMismatch,
    /// 解析为空,或 document_fact 解析段没有可用的有效引用。
    ExplanationUnsupported,
    /// 同次生成中重复的题目(去重,重复项不计数)。
    DuplicateQuestion,
    /// 题目 ID 缺失或与同批其他题重复。
    QuestionIdInvalid,
}

impl QuestionInvalidReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SchemaInvalid => "schema_invalid",
            Self::OptionStructureInvalid => "option_structure_invalid",
            Self::CorrectOptionUnknown => "correct_option_unknown",
            Self::NoValidCitation => "no_valid_citation",
            Self::CitationTargetMissing => "citation_target_missing",
            Self::CitationQuoteMismatch => "citation_quote_mismatch",
            Self::ExplanationUnsupported => "explanation_unsupported",
            Self::DuplicateQuestion => "duplicate_question",
            Self::QuestionIdInvalid => "question_id_invalid",
        }
    }
}

/// 用户反馈类型(《M11》§4 封闭枚举)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuizFeedbackType {
    MultipleAnswers,
    NoEvidence,
    WrongExplanation,
    Ambiguous,
}

impl QuizFeedbackType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MultipleAnswers => "multiple_answers",
            Self::NoEvidence => "no_evidence",
            Self::WrongExplanation => "wrong_explanation",
            Self::Ambiguous => "ambiguous",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "multiple_answers" => Self::MultipleAnswers,
            "no_evidence" => Self::NoEvidence,
            "wrong_explanation" => Self::WrongExplanation,
            "ambiguous" => Self::Ambiguous,
            _ => return None,
        })
    }
}

/// 反馈处理状态:open → accepted/dismissed;只有 accepted 触发重生成建议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuizFeedbackStatus {
    Open,
    Accepted,
    Dismissed,
}

impl QuizFeedbackStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Accepted => "accepted",
            Self::Dismissed => "dismissed",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "accepted" => Self::Accepted,
            "dismissed" => Self::Dismissed,
            _ => return None,
        })
    }
}

// —— quiz.v1 内容(《M11》§2 Schema)——

/// quiz.v1 产物内容。严格模式:未知字段拒绝,不静默接受改变语义的自由结构。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizContentV1 {
    pub schema_version: String,
    pub questions: Vec<QuizQuestion>,
}

/// 题目:题干、选项、唯一答案、解析、Citation、难度与有效状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizQuestion {
    pub id: String,
    pub prompt: String,
    pub options: Vec<QuizOption>,
    pub correct_option_id: String,
    pub explanation_segments: Vec<QuizExplanationSegment>,
    pub citations: Vec<QuizCitation>,
    pub difficulty: QuestionDifficulty,
    pub validity: QuestionValidity,
    /// validity=invalid 时的机器可读原因;其余状态为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub invalid_reason: Option<QuestionInvalidReason>,
}

/// 题目选项:ID 在题目内唯一;3–5 个(《M11》§2)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizOption {
    pub id: String,
    pub text: String,
}

/// 解析段:document_fact 必须携带有效引用;inference 显式标注非原文事实。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizExplanationSegment {
    /// 模型偶尔省略;服务端在 quiz.v2 解析时生成稳定 id。
    #[serde(default)]
    pub id: String,
    pub kind: QuizSegmentKind,
    pub text: String,
    #[serde(default)]
    pub citation_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuizSegmentKind {
    DocumentFact,
    Inference,
}

impl QuizSegmentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DocumentFact => "document_fact",
            Self::Inference => "inference",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "document_fact" => Self::DocumentFact,
            "inference" => Self::Inference,
            _ => return None,
        })
    }
}

/// 题目内 Citation:指向本次快照中的 ContentBlock + 原文引文。
/// 结构有效(块在范围内且引文匹配)≠ 语义支持(《M11》§4)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizCitation {
    #[serde(default)]
    pub id: String,
    pub block_id: String,
    pub quoted_text: String,
    #[serde(default)]
    pub claim_key: QuizClaimKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuizClaimKey {
    /// 引文支持解析(explanation)。
    #[default]
    Explanation,
}

impl QuizClaimKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Explanation => "explanation",
        }
    }
}

// —— 生成配置与材料(《M11》§2)——

/// 生成请求配置:数量 1–10(MVP 默认 5)、难度组合、风格与附加要求。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizGenerationConfig {
    /// 生成数量;1–10,默认 5。
    #[serde(default = "default_quiz_count")]
    pub count: u8,
    /// 难度组合;非空,默认 [medium]。
    #[serde(default = "default_difficulties")]
    pub difficulties: Vec<QuestionDifficulty>,
    /// 题目风格(如"概念边界/因果/条件差异")。
    #[serde(default)]
    pub style: String,
    /// 用户附加要求;限长,纯文本。
    #[serde(default)]
    pub extra_instructions: String,
}

fn default_quiz_count() -> u8 {
    5
}

fn default_difficulties() -> Vec<QuestionDifficulty> {
    vec![QuestionDifficulty::Medium]
}

impl Default for QuizGenerationConfig {
    fn default() -> Self {
        Self {
            count: default_quiz_count(),
            difficulties: default_difficulties(),
            style: String::new(),
            extra_instructions: String::new(),
        }
    }
}

/// 生成材料:调用方(M10 编排/M07 快照)冻结的原文块投影。
/// M11 不自行扩展页面范围;选区过短、覆盖不足或无原文证据时拒绝生成。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizGenerationMaterials {
    pub document_version_id: String,
    pub blocks: Vec<QuizMaterialBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizMaterialBlock {
    pub block_id: String,
    #[ts(type = "number")]
    pub ordinal: i64,
    pub text: String,
}

// —— 生成端口(M09 StructuredGenerationPort 的 M11 视图)——

/// 结构化生成请求(《M09》§8 taskKind="quiz" 的 M11 投影)。
/// M09 落地后由 M13 适配接线;测试注入替身,替身不得进入生产组装根。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizPortRequest {
    pub task_id: String,
    pub attempt_id: String,
    pub snapshot_id: String,
    pub schema_version: String,
    pub instruction_version: String,
    pub locale: String,
    pub config: QuizGenerationConfig,
    /// 按原文顺序装填的来源块;角色与《01》§5.3 sourceRole 对齐。
    pub inputs: Vec<QuizPortInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizPortInput {
    pub source_id: String,
    pub role: QuizSourceRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuizSourceRole {
    Original,
    UserNote,
    AiDraft,
}

impl QuizSourceRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::UserNote => "user_note",
            Self::AiDraft => "ai_draft",
        }
    }
}

/// M09 结构化生成结果(《M09》§8)的 M11 投影;由生成端口返回。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizPortResult {
    pub content: QuizContentV1,
    pub model_provider_id: String,
    pub model_id: String,
    /// 模型自报的部分输出;无论真假,题目仍需通过 M11 校验流水线。
    pub partial: bool,
    /// 模型未按 schema 输出时保留的原始正文(仅用于可见兜底,不冒充题目)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plain_text: Option<String>,
}

// —— Artifact 版本读取端口(M10 ArtifactService 的 M11 视图)——

/// 题目产物版本投影。M11 不读 artifact_version 表,一切经此端口
/// (《M11》§1:所有保存与读取通过 ArtifactService,不直接改版本表)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizArtifactVersion {
    pub id: String,
    pub artifact_id: String,
    pub document_id: String,
    pub schema_version: String,
    pub origin: QuizArtifactOrigin,
    /// quiz.v1 内容 JSON(由 M10 Codec 校验后保存的原文)。
    pub content_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum QuizArtifactOrigin {
    AiDraft,
    UserEdit,
    Copy,
    Restore,
}

impl QuizArtifactOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AiDraft => "ai_draft",
            Self::UserEdit => "user_edit",
            Self::Copy => "copy",
            Self::Restore => "restore",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "ai_draft" => Self::AiDraft,
            "user_edit" => Self::UserEdit,
            "copy" => Self::Copy,
            "restore" => Self::Restore,
            _ => return None,
        })
    }
}

/// Artifact 版本读取端口;M13 在 M10 落地时接线真实实现。
pub trait QuizArtifactSource: Send + Sync {
    fn load_quiz_version(
        &self,
        artifact_version_id: &str,
    ) -> Result<Option<QuizArtifactVersion>, crate::shared::AppError>;
}

// —— 视图 DTO ——

/// 作答记录视图:呈现事实,不推断掌握度(《M11》§5)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ComprehensionRecordView {
    pub id: String,
    pub artifact_version_id: String,
    pub question_id: String,
    #[ts(type = "number")]
    pub attempt_no: i64,
    pub selected_option_id: String,
    pub is_correct: bool,
    pub validity_status: QuestionValidity,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feedback_type: Option<QuizFeedbackType>,
    #[ts(type = "number")]
    pub created_at: i64,
}

pub type ComprehensionRecordPage = Page<ComprehensionRecordView>;

/// 无效题反馈视图(《M11》§4)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuestionFeedbackView {
    pub id: String,
    pub artifact_version_id: String,
    pub question_id: String,
    pub feedback_type: QuizFeedbackType,
    pub comment: String,
    pub status: QuizFeedbackStatus,
    #[ts(type = "number")]
    pub revision: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

pub type QuestionFeedbackPage = Page<QuestionFeedbackView>;

/// 单题聚合:题目 + 全部尝试 + 最新答案 + 反馈摘要。
/// 未作答且未判定无效的题不返回正确答案(不泄漏答案;
/// 无效题需要完整展示"此题无效"的依据,故携带全部字段)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizQuestionSummary {
    pub question_id: String,
    pub prompt: String,
    pub options: Vec<QuizOption>,
    pub difficulty: QuestionDifficulty,
    pub validity: QuestionValidity,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub invalid_reason: Option<QuestionInvalidReason>,
    /// 解析与引用:仅在该题已作答或题目无效/有争议时返回。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub explanation: Option<QuizQuestionExplanation>,
    /// 已作答时的正确选项(与 explanation 同条件返回)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub correct_option_id: Option<String>,
    /// 全部尝试(按 attemptNo 升序);重做保留历史。
    pub attempts: Vec<ComprehensionRecordView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub latest_attempt: Option<ComprehensionRecordView>,
    /// 最近一次 accepted 反馈(触发重生成建议的依据)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub accepted_feedback: Option<QuestionFeedbackView>,
    #[ts(type = "number")]
    pub open_feedback_count: i64,
}

/// 已揭示的解析投影(答案揭示后)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizQuestionExplanation {
    pub segments: Vec<QuizExplanationSegment>,
    pub citations: Vec<QuizCitation>,
}

/// 版本统计:正确率只统计 validity=valid 的题(《M11》§5)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizStats {
    #[ts(type = "number")]
    pub total_questions: i64,
    #[ts(type = "number")]
    pub valid_questions: i64,
    #[ts(type = "number")]
    pub invalid_questions: i64,
    /// 至少有一个作答尝试的题数(任意有效性)。
    #[ts(type = "number")]
    pub answered_questions: i64,
    /// 有效题中已作答数(按最新一次尝试统计正确率)。
    #[ts(type = "number")]
    pub answered_valid_questions: i64,
    /// 有效题最新尝试答对数。
    #[ts(type = "number")]
    pub correct_valid_questions: i64,
    /// 正确率 = correct_valid / answered_valid;未作答有效题为 0 时为 null。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub accuracy: Option<f64>,
}

/// 版本聚合视图:题目中心 UI 的读模型(《M11》§7)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizVersionSummary {
    pub artifact_version_id: String,
    pub artifact_id: String,
    pub document_id: String,
    pub schema_version: String,
    pub questions: Vec<QuizQuestionSummary>,
    pub stats: QuizStats,
}

// —— 输入 DTO ——

/// 作答输入(《02》§3.3 quiz_submit_answer)。
/// attemptNo 缺省时服务端分配"当前最大 + 1";显式携带时必须与
/// 服务端计算一致,否则拒绝(双击由幂等键兜底)。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizAnswerInput {
    pub artifact_version_id: String,
    pub question_id: String,
    pub selected_option_id: String,
    #[serde(default)]
    #[ts(optional)]
    pub attempt_no: Option<i64>,
}

/// 无效题反馈输入(《02》§3.3 quiz_feedback);不依赖是否已作答。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizFeedbackInput {
    pub artifact_version_id: String,
    pub question_id: String,
    pub feedback_type: QuizFeedbackType,
    #[serde(default)]
    pub comment: String,
}

/// 反馈状态流转输入(open → accepted/dismissed;
/// 只有 accepted 可触发重生成建议,历史题目版本不变)。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct QuizFeedbackUpdateInput {
    pub feedback_id: String,
    pub status: QuizFeedbackStatus,
    #[ts(type = "number")]
    pub expected_revision: i64,
}

// —— 数据库行(内部)——

#[derive(Debug, Clone)]
pub struct ComprehensionRecordRow {
    pub id: String,
    pub artifact_version_id: String,
    pub question_id: String,
    pub attempt_no: i64,
    pub selected_option_id: String,
    pub is_correct: bool,
    pub validity_status: QuestionValidity,
    pub feedback_type: Option<QuizFeedbackType>,
    pub idempotency_key: String,
    pub created_at: i64,
}

pub const RECORD_COLS: &str = "id, artifact_version_id, question_id, attempt_no, \
     selected_option_id, is_correct, validity_status, feedback_type, idempotency_key, created_at";

impl ComprehensionRecordRow {
    pub fn from_row(r: &rusqlite::Row<'_>) -> Result<Self, rusqlite::Error> {
        Ok(Self {
            id: r.get("id")?,
            artifact_version_id: r.get("artifact_version_id")?,
            question_id: r.get("question_id")?,
            attempt_no: r.get("attempt_no")?,
            selected_option_id: r.get("selected_option_id")?,
            is_correct: r.get::<_, i64>("is_correct")? != 0,
            validity_status: QuestionValidity::parse_db(&r.get::<_, String>("validity_status")?)
                .unwrap_or(QuestionValidity::Invalid),
            feedback_type: r
                .get::<_, Option<String>>("feedback_type")?
                .and_then(|s| QuizFeedbackType::parse_db(&s)),
            idempotency_key: r.get("idempotency_key")?,
            created_at: r.get("created_at")?,
        })
    }

    pub fn to_view(&self) -> ComprehensionRecordView {
        ComprehensionRecordView {
            id: self.id.clone(),
            artifact_version_id: self.artifact_version_id.clone(),
            question_id: self.question_id.clone(),
            attempt_no: self.attempt_no,
            selected_option_id: self.selected_option_id.clone(),
            is_correct: self.is_correct,
            validity_status: self.validity_status,
            feedback_type: self.feedback_type,
            created_at: self.created_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct QuestionFeedbackRow {
    pub id: String,
    pub artifact_version_id: String,
    pub question_id: String,
    pub feedback_type: QuizFeedbackType,
    pub comment: String,
    pub status: QuizFeedbackStatus,
    pub revision: i64,
    pub idempotency_key: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub const FEEDBACK_COLS: &str = "id, artifact_version_id, question_id, feedback_type, comment, \
     status, revision, idempotency_key, created_at, updated_at";

impl QuestionFeedbackRow {
    pub fn from_row(r: &rusqlite::Row<'_>) -> Result<Self, rusqlite::Error> {
        Ok(Self {
            id: r.get("id")?,
            artifact_version_id: r.get("artifact_version_id")?,
            question_id: r.get("question_id")?,
            feedback_type: QuizFeedbackType::parse_db(&r.get::<_, String>("feedback_type")?)
                .unwrap_or(QuizFeedbackType::Ambiguous),
            comment: r.get("comment")?,
            status: QuizFeedbackStatus::parse_db(&r.get::<_, String>("status")?)
                .unwrap_or(QuizFeedbackStatus::Open),
            revision: r.get("revision")?,
            idempotency_key: r.get("idempotency_key")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }

    pub fn to_view(&self) -> QuestionFeedbackView {
        QuestionFeedbackView {
            id: self.id.clone(),
            artifact_version_id: self.artifact_version_id.clone(),
            question_id: self.question_id.clone(),
            feedback_type: self.feedback_type,
            comment: self.comment.clone(),
            status: self.status,
            revision: self.revision,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
