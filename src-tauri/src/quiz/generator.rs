//! M11 题目生成编排(《M11》§2–§3)。
//!
//! 生成链:M10(artifact_generate)创建 Task → 调用本生成器 → 生成器经
//! `QuizGenerationPort`(M09 StructuredGenerationPort 的 M11 视图)取得
//! quiz.v1 草稿 → 校验流水线写回有效性 → 返回报告;是否保存为 ArtifactVersion
//! 由 M10 按版本保护规则决定。M11 不创建 Task、不写产物表(《M11》§1)。
//!
//! 端口替身只允许存在于测试;生产组装根在 M09/M10 落地后由 M13 接线。
//! 取消语义属于任务层:端口调用被取消即返回错误,调用方不保存不完整题目
//! 为正式版本(《M11》§3)。

use std::sync::Arc;

use crate::shared::{AppError, CoreErrorCode};

use super::models::*;
use super::validation::{
    quiz_content_to_json, validate_generated_quiz, validate_generation_config,
    validate_generation_materials, QuizValidationReport,
};

/// 题目生成 Prompt 版本;冻结系统指令与输出 Schema 约定(《M09》§6)。
pub const QUIZ_INSTRUCTION_VERSION: &str = "quiz-instruction-v1";

/// M09 结构化生成端口的 M11 视图:产出 quiz.v1 草稿。
/// M09 落地后由 M13 用真实端口适配;测试注入替身。
pub trait QuizGenerationPort: Send + Sync {
    fn generate_quiz(&self, request: &QuizPortRequest) -> Result<QuizPortResult, AppError>;
}

/// 生成入口上下文:任务/尝试/快照标识由调用方(M10 编排)提供。
#[derive(Debug, Clone)]
pub struct QuizGenerationContext {
    pub task_id: String,
    pub attempt_id: String,
    pub snapshot_id: String,
    pub locale: String,
    pub config: QuizGenerationConfig,
    /// 调用方从冻结快照导出的材料;M11 不自行扩展页面范围。
    pub materials: QuizGenerationMaterials,
}

/// 生成结果:校验报告 + 模型元数据(partial 草稿仍需界面明确标注)。
#[derive(Debug, Clone)]
pub struct GeneratedQuizOutcome {
    pub report: QuizValidationReport,
    pub model_provider_id: String,
    pub model_id: String,
    pub model_partial: bool,
    /// 模型未按 quiz schema 输出时的原始正文(用于可见兜底)。
    pub plain_text: Option<String>,
}

/// 题目生成器:校验输入 → 组装端口请求 → 校验产出。
/// 纯编排,无数据库依赖;可独立于 M09/M10 测试。
pub struct QuizGenerator {
    port: Arc<dyn QuizGenerationPort>,
}

impl QuizGenerator {
    pub fn new(port: Arc<dyn QuizGenerationPort>) -> Self {
        Self { port }
    }

    /// 执行一次生成。输入校验失败在调用模型前终止并说明原因
    /// (选区过短/覆盖不足/无原文证据,《M11》§2)。
    pub fn generate(&self, ctx: &QuizGenerationContext) -> Result<GeneratedQuizOutcome, AppError> {
        validate_generation_config(&ctx.config)?;
        validate_generation_materials(&ctx.materials)?;

        let request = QuizPortRequest {
            task_id: ctx.task_id.clone(),
            attempt_id: ctx.attempt_id.clone(),
            snapshot_id: ctx.snapshot_id.clone(),
            schema_version: QUIZ_SCHEMA_VERSION.to_string(),
            instruction_version: QUIZ_INSTRUCTION_VERSION.to_string(),
            locale: ctx.locale.clone(),
            config: ctx.config.clone(),
            // 材料按原文顺序装填;快照来源分层由调用方在材料中体现。
            inputs: ctx
                .materials
                .blocks
                .iter()
                .map(|b| QuizPortInput {
                    source_id: b.block_id.clone(),
                    role: QuizSourceRole::Original,
                    text: b.text.clone(),
                })
                .collect(),
        };
        let result = self.port.generate_quiz(&request)?;
        if let Some(text) = result.plain_text.clone() {
            return Ok(GeneratedQuizOutcome {
                report: QuizValidationReport {
                    content: result.content,
                    outcomes: Vec::new(),
                    valid_count: 0,
                    invalid_count: 0,
                },
                model_provider_id: result.model_provider_id,
                model_id: result.model_id,
                model_partial: true,
                plain_text: Some(text),
            });
        }
        if result.content.schema_version != QUIZ_SCHEMA_VERSION {
            return Err(AppError::new(
                CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "quiz_model_output_schema_mismatch".into()));
        }
        // 规范化一次序列化再解析,防止端口替身/适配器绕过严格 Codec。
        let raw = quiz_content_to_json(&result.content)?;
        let draft = super::validation::parse_quiz_content(&raw)?;
        let report = validate_generated_quiz(draft, &ctx.materials);
        Ok(GeneratedQuizOutcome {
            report,
            model_provider_id: result.model_provider_id,
            model_id: result.model_id,
            model_partial: result.partial,
            plain_text: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn reason_of(e: &crate::shared::AppError) -> String {
        match &e.details.as_ref().unwrap()["reason"] {
            crate::shared::error::DetailValue::Str(s) => s.clone(),
            _ => panic!("reason detail 不是字符串"),
        }
    }

    struct FakePort {
        result: QuizContentV1,
        seen_requests: Mutex<Vec<QuizPortRequest>>,
    }

    impl QuizGenerationPort for FakePort {
        fn generate_quiz(&self, request: &QuizPortRequest) -> Result<QuizPortResult, AppError> {
            self.seen_requests.lock().unwrap().push(request.clone());
            Ok(QuizPortResult {
                content: self.result.clone(),
                model_provider_id: "ollama".into(),
                model_id: "qwen3".into(),
                partial: false,
                plain_text: None,
            })
        }
    }

    fn materials() -> QuizGenerationMaterials {
        let text = "知识管理系统保持可检索的证据链。检索先限定范围再排序,禁止全文 TopK 后过滤。\
                    快照不可变,追问复用同一快照。引用校验保证文档事实可回跳原文块。\
                    隐私边界要求模型请求经统一出口授权,密钥不进入日志与数据库。\
                    证据不足时返回明确状态;取消后不保存半成品为正式版本。\n                    作答记录保留全部尝试并只统计有效题,无效题不计错,用户可反馈并重新生成。\n                    重新生成总是创建新草稿版本,不覆盖用户已编辑内容;范围由冻结快照决定。";
        QuizGenerationMaterials {
            document_version_id: "v1".into(),
            blocks: vec![QuizMaterialBlock {
                block_id: "b1".into(),
                ordinal: 1,
                text: text.into(),
            }],
        }
    }

    fn draft() -> QuizContentV1 {
        QuizContentV1 {
            schema_version: QUIZ_SCHEMA_VERSION.into(),
            questions: vec![QuizQuestion {
                id: "q1".into(),
                prompt: "快照创建后能否修改?".into(),
                options: vec![
                    QuizOption {
                        id: "a".into(),
                        text: "不可修改".into(),
                    },
                    QuizOption {
                        id: "b".into(),
                        text: "可原地更新".into(),
                    },
                    QuizOption {
                        id: "c".into(),
                        text: "随问答扩围".into(),
                    },
                ],
                correct_option_id: "a".into(),
                explanation_segments: vec![QuizExplanationSegment {
                    id: "e1".into(),
                    kind: QuizSegmentKind::DocumentFact,
                    text: "快照不可变,追问复用同一快照。".into(),
                    citation_ids: vec!["c1".into()],
                }],
                citations: vec![QuizCitation {
                    id: "c1".into(),
                    block_id: "b1".into(),
                    quoted_text: "快照不可变".into(),
                    claim_key: QuizClaimKey::Explanation,
                }],
                difficulty: QuestionDifficulty::Easy,
                validity: QuestionValidity::Unverified,
                invalid_reason: None,
            }],
        }
    }

    fn ctx(materials: QuizGenerationMaterials) -> QuizGenerationContext {
        QuizGenerationContext {
            task_id: "t1".into(),
            attempt_id: "a1".into(),
            snapshot_id: "s1".into(),
            locale: "zh-CN".into(),
            config: QuizGenerationConfig::default(),
            materials,
        }
    }

    #[test]
    fn generate_validates_inputs_before_model_call() {
        let port = Arc::new(FakePort {
            result: draft(),
            seen_requests: Mutex::new(vec![]),
        });
        let gen = QuizGenerator::new(port.clone());
        // 材料为空:模型前终止,零端口调用。
        let mut bad = ctx(materials());
        bad.materials.blocks.clear();
        let err = gen.generate(&bad).unwrap_err();
        assert_eq!(err.code, "INVALID_REQUEST");
        assert!(port.seen_requests.lock().unwrap().is_empty());

        // 正常路径:请求携带 schemaVersion 与指令版本,产出经校验。
        let outcome = gen.generate(&ctx(materials())).unwrap();
        assert_eq!(outcome.report.valid_count, 1);
        let req = &port.seen_requests.lock().unwrap()[0];
        assert_eq!(req.schema_version, QUIZ_SCHEMA_VERSION);
        assert_eq!(req.instruction_version, QUIZ_INSTRUCTION_VERSION);
        assert_eq!(req.inputs.len(), 1);
        assert_eq!(req.inputs[0].role.as_str(), "original");
        assert_eq!(outcome.model_provider_id, "ollama");
    }

    #[test]
    fn generate_rejects_wrong_schema_from_port() {
        let mut content = draft();
        content.schema_version = "quiz.v2".into();
        let port = Arc::new(FakePort {
            result: content,
            seen_requests: Mutex::new(vec![]),
        });
        let gen = QuizGenerator::new(port);
        let err = gen.generate(&ctx(materials())).unwrap_err();
        assert_eq!(err.code, "INVALID_REQUEST");
        assert_eq!(reason_of(&err), "quiz_model_output_schema_mismatch");
    }
}
