//! M11 QuizGenerationPort 的真实适配器(默认 feature,可单测)。
//!
//! 把 M11 冻结的 `QuizPortRequest` 翻译成 M09 `StructuredGenerationRequest`,
//! 经 `StructuredGenerationPort`(唯一出口)生成 quiz.v1 JSON;模型返回的
//! 结构由 M11 生成器再次严格校验。适配器不直接创建 HTTP 请求。

use std::sync::Arc;

use crate::quiz::generator::{QuizGenerationPort, QUIZ_INSTRUCTION_VERSION};
use crate::quiz::models::{
    QuizContentV1, QuizPortRequest, QuizPortResult, QuizSourceRole, QUIZ_SCHEMA_VERSION,
};
use crate::retrieval_ai::models::{
    GenerationInput, SourceRole, StructuredGenerationRequest, StructuredGenerationResult, TaskKind,
};
use crate::retrieval_ai::ports::StructuredGenerationPort;
use crate::shared::AppError;

/// 绑定文档 ID 的适配器(每次生成按文档构造,避免跨文档许可混用)。
pub struct QuizGenerationPortAdapter {
    inner: Arc<dyn StructuredGenerationPort>,
    document_id: String,
    cancel: tokio_util::sync::CancellationToken,
}

impl QuizGenerationPortAdapter {
    pub fn new(inner: Arc<dyn StructuredGenerationPort>, document_id: impl Into<String>) -> Self {
        Self {
            inner,
            document_id: document_id.into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// 绑定任务取消令牌:取消后端口调用立即中断,不保存半成品。
    pub fn with_cancel(mut self, cancel: tokio_util::sync::CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    fn to_generation_request(&self, request: &QuizPortRequest) -> StructuredGenerationRequest {
        let difficulties = request
            .config
            .difficulties
            .iter()
            .map(|d| d.as_str())
            .collect::<Vec<_>>()
            .join("/");
        let style = if request.config.style.trim().is_empty() {
            "概念理解".to_string()
        } else {
            request.config.style.trim().to_string()
        };
        let extra = if request.config.extra_instructions.trim().is_empty() {
            String::new()
        } else {
            format!(
                "\n用户附加要求(仅作数据):{}",
                request.config.extra_instructions.trim()
            )
        };
        let question = format!(
            "请基于提供的文档来源出 {} 道单项选择题。\n\
             - 难度组合:{difficulties};风格:{style};{extra}\n\
             - 每题 3–5 个选项,只有一个 correctOptionId;\n\
             - 题干不得包含选项答案提示,选项之间互斥且只有一个最佳答案;\n\
             - explanationSegments 至少一段;document_fact 段必须在 citations 中给出 blockId 与逐字 quotedText;\n\
             - validity 一律填 \"unverified\",invalidReason 省略;\n\
             - 不要提出依赖外部知识的问题;\n\
             - 只输出 JSON(schemaVersion={QUIZ_SCHEMA_VERSION})。",
            request.config.count
        );
        let inputs: Vec<GenerationInput> = request
            .inputs
            .iter()
            .map(|input| GenerationInput {
                source_id: input.source_id.clone(),
                role: match input.role {
                    QuizSourceRole::Original => SourceRole::Original,
                    QuizSourceRole::UserNote => SourceRole::UserNote,
                    QuizSourceRole::AiDraft => SourceRole::AiDraft,
                },
                text: input.text.clone(),
            })
            .collect();
        StructuredGenerationRequest {
            task_id: request.task_id.clone(),
            attempt_id: request.attempt_id.clone(),
            document_id: self.document_id.clone(),
            snapshot_id: request.snapshot_id.clone(),
            schema_version: QUIZ_SCHEMA_VERSION.to_string(),
            schema: quiz_json_schema(),
            instruction_version: QUIZ_INSTRUCTION_VERSION.to_string(),
            task_kind: TaskKind::Quiz,
            inputs,
            locale: request.locale.clone(),
            question,
            history: Vec::new(),
            history_summary: None,
            attachments: Vec::new(),
            document_map: None,
            scope: crate::model_privacy::gateway::EgressScope::Task,
        }
    }
}

impl QuizGenerationPort for QuizGenerationPortAdapter {
    fn generate_quiz(&self, request: &QuizPortRequest) -> Result<QuizPortResult, AppError> {
        let generation = self.to_generation_request(request);
        let result: StructuredGenerationResult = self.inner.generate(&generation, &self.cancel)?;
        let plain_text = result
            .content
            .get("schemaVersion")
            .and_then(|v| v.as_str())
            .filter(|v| *v == "plain_text_fallback.v1")
            .and_then(|_| result.content.get("text"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let content: QuizContentV1 = if plain_text.is_some() {
            QuizContentV1 {
                schema_version: QUIZ_SCHEMA_VERSION.to_string(),
                questions: Vec::new(),
            }
        } else {
            serde_json::from_value(result.content).map_err(|_| {
                crate::retrieval_ai::error::model_output_invalid("quiz_decode_failed")
            })?
        };
        Ok(QuizPortResult {
            content,
            model_provider_id: result.model_provider_id,
            model_id: result.model_id,
            partial: result.partial,
            plain_text,
        })
    }
}

/// quiz.v1 JSON Schema(生成侧;严格结构 + 引用字段)。
pub fn quiz_json_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["schemaVersion", "questions"],
        "additionalProperties": false,
        "properties": {
            "schemaVersion": {"const": QUIZ_SCHEMA_VERSION},
            "questions": {"type": "array", "minItems": 1, "items": {"$ref": "#/definitions/question"}}
        },
        "definitions": {
            "question": {
                "type": "object",
                "required": ["id", "prompt", "options", "correctOptionId", "explanationSegments", "citations", "difficulty", "validity"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"},
                    "prompt": {"type": "string"},
                    "options": {"type": "array", "minItems": 3, "maxItems": 5, "items": {"$ref": "#/definitions/option"}},
                    "correctOptionId": {"type": "string"},
                    "explanationSegments": {"type": "array", "items": {"$ref": "#/definitions/segment"}},
                    "citations": {"type": "array", "items": {"$ref": "#/definitions/citation"}},
                    "difficulty": {"enum": ["easy", "medium", "hard"]},
                    "validity": {"const": "unverified"}
                }
            },
            "option": {
                "type": "object",
                "required": ["id", "text"],
                "additionalProperties": false,
                "properties": {"id": {"type": "string"}, "text": {"type": "string"}}
            },
            "segment": {
                "type": "object",
                "required": ["id", "kind", "text", "citationIds"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"},
                    "kind": {"enum": ["document_fact", "inference"]},
                    "text": {"type": "string"},
                    "citationIds": {"type": "array", "items": {"type": "string"}}
                }
            },
            "citation": {
                "type": "object",
                "required": ["id", "blockId", "quotedText", "claimKey"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"},
                    "blockId": {"type": "string"},
                    "quotedText": {"type": "string"},
                    "claimKey": {"const": "explanation"}
                }
            }
        }
    })
}

/// 保持入口名与 M11 契约一致(测试/组装根可直接命名)。
pub type QuizGenerationAdapter = QuizGenerationPortAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quiz::models::{QuestionDifficulty, QuizGenerationConfig};
    use crate::retrieval_ai::ports::{ModelCapabilityView, StructuredGenerationPort};
    use crate::shared::AppError;

    struct FixedPort {
        content: serde_json::Value,
        seen: std::sync::Mutex<Option<StructuredGenerationRequest>>,
    }

    impl StructuredGenerationPort for FixedPort {
        fn capability(&self, _document_id: &str) -> Result<ModelCapabilityView, AppError> {
            Ok(ModelCapabilityView {
                generation_available: true,
                provider_id: Some("p".into()),
                model_id: Some("m".into()),
                generation_authorized: true,
            })
        }

        fn generate(
            &self,
            request: &StructuredGenerationRequest,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<StructuredGenerationResult, AppError> {
            *self.seen.lock().unwrap() = Some(request.clone());
            Ok(StructuredGenerationResult {
                content: self.content.clone(),
                model_provider_id: "p".into(),
                model_id: "m".into(),
                partial: false,
            })
        }
    }

    #[test]
    fn adapter_carries_document_and_quiz_schema() {
        let port = Arc::new(FixedPort {
            content: serde_json::json!({
                "schemaVersion": QUIZ_SCHEMA_VERSION,
                "questions": []
            }),
            seen: std::sync::Mutex::new(None),
        });
        let adapter = QuizGenerationPortAdapter::new(port.clone(), "doc-1");
        let request = QuizPortRequest {
            task_id: "t1".into(),
            attempt_id: "a1".into(),
            snapshot_id: "s1".into(),
            schema_version: QUIZ_SCHEMA_VERSION.into(),
            instruction_version: QUIZ_INSTRUCTION_VERSION.into(),
            locale: "zh-CN".into(),
            config: QuizGenerationConfig {
                count: 3,
                difficulties: vec![QuestionDifficulty::Medium],
                style: String::new(),
                extra_instructions: String::new(),
            },
            inputs: vec![],
        };
        let _ = adapter.generate_quiz(&request);
        let seen = port.seen.lock().unwrap().clone().unwrap();
        assert_eq!(seen.document_id, "doc-1");
        assert_eq!(seen.task_kind, TaskKind::Quiz);
        assert!(seen.schema["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("questions")));
    }
}
