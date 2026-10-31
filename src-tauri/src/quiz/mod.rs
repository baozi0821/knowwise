//! M11 理解校验题(《M11》)。
//!
//! 题目用于暴露概念边界、因果关系、条件差异和易混点,不用于增加生成
//! 数量或推断长期掌握度。每道正式计分题必须有唯一合理答案、解析和
//! 有效原文依据;无依据、多解或结构无效的题目标 invalid,不计错,
//! 用户可反馈并重新生成。
//!
//! 边界:
//! - 题目内容本体是 M10 的 quiz.v1 ArtifactVersion;本模块实现题目
//!   Codec、生成校验与编排(`QuizGenerator`),保存一律经 M10。
//! - 作答(ComprehensionRecord)与反馈(QuizQuestionFeedback)是本模块
//!   自有表;作答只呈现事实,不推断掌握度。
//! - M09/M10 尚未落地:跨模块依赖以端口表达
//!   (`QuizGenerationPort`/`QuizArtifactSource`),由 M13 在对应模块
//!   接入时接线;替身只存在于测试(《00》§1)。

pub mod generator;
pub mod models;
pub mod registration;
pub mod service;
pub mod v2;
pub mod validation;

#[cfg(feature = "desktop")]
pub mod commands;

pub use generator::{
    GeneratedQuizOutcome, QuizGenerationContext, QuizGenerationPort, QuizGenerator,
    QUIZ_INSTRUCTION_VERSION,
};
pub use models::*;
pub use service::{QuizService, UnwiredQuizArtifactSource};
pub use validation::{
    parse_quiz_content, quiz_content_to_json, quiz_question_invalid, validate_feedback_comment,
    validate_generated_quiz, validate_generation_config, validate_generation_materials,
    QuizV1Codec, QuizValidationReport, MAX_GENERATE_COUNT, MIN_GENERATE_COUNT,
};
