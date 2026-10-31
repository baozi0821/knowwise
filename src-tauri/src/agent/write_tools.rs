//! P3A Agent 写工具:create_artifact / create_quiz / export_file。
//!
//! 写工具绝不静默执行:Agent 循环先写 awaiting_confirmation 步骤,等待用户
//! 批准后才调用 M10 ArtifactService / M11 QuizGenerator / M10 导出器。
//! 本模块只做参数校验与编排,不直接写 artifact/version/export_record 表。

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::app_shell::quiz_port::QuizGenerationPortAdapter;
use crate::artifacts::models::{validate_summary_v1, CoverageRegion, CoverageReport};
use crate::artifacts::service::{AiDraftCommitInput, ArtifactService, CitationDraftForCommit};
use crate::context::models::{AgentConfirmationView, ContextSnapshotView};
use crate::evidence::models::{
    CitationInput, CitationOwnerType, CitationSourceType, ClaimKind, SaveCitationsRequest,
};
use crate::evidence::service::EvidenceService;
use crate::model_privacy::provider::{ToolCall, ToolSpec};
use crate::quiz::generator::{QuizGenerationContext, QuizGenerator};
use crate::quiz::models::{
    QuestionValidity, QuizContentV1, QuizGenerationConfig, QuizGenerationMaterials,
    QuizMaterialBlock, QUIZ_SCHEMA_VERSION,
};
use crate::quiz::validation::{validate_generation_config, validate_generation_materials};
use crate::retrieval_ai::ports::StructuredGenerationPort;
use crate::shared::{new_trace_id, AppError, Clock, CoreErrorCode, IdGenerator};

use super::models::{tool_args_invalid, AgentToolContext};

const MAX_WRITE_CONTENT_CHARS: usize = 200_000;
const MAX_TITLE_CHARS: usize = 80;
const MAX_QUIZ_BLOCKS: usize = 80;
const MAX_QUIZ_MATERIAL_CHARS: usize = 30_000;

pub fn is_write_tool(name: &str) -> bool {
    matches!(name, "create_artifact" | "create_quiz" | "export_file")
}

pub fn write_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "create_artifact".into(),
            description: "Create a new artifact draft from JSON content. This WRITE tool requires explicit user approval. kind must be a registered artifact kind (e.g. summary); content must satisfy that kind's schema.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "description": "Artifact kind, e.g. summary."},
                    "title": {"type": "string", "description": "Optional short title."},
                    "content": {"type": "object", "description": "Artifact JSON content."},
                    "citations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "claimKey": {"type": "string", "description": "Summary segment id."},
                                "blockId": {"type": "string"},
                                "quotedText": {"type": "string"},
                                "claimKind": {"type": "string"}
                            },
                            "required": ["claimKey", "blockId", "quotedText"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["kind", "content"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "create_quiz".into(),
            description: "Generate a quiz artifact from the frozen snapshot. This WRITE tool requires explicit user approval and performs one model call through M11 QuizGenerator.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "config": {
                        "type": "object",
                        "properties": {
                            "count": {"type": "integer", "minimum": 1, "maximum": 10, "default": 5},
                            "difficulties": {"type": "array", "items": {"enum": ["easy","medium","hard"]}},
                            "style": {"type": "string"},
                            "extraInstructions": {"type": "string"}
                        },
                        "additionalProperties": false
                    },
                    "title": {"type": "string"}
                },
                "required": ["config"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "export_file".into(),
            description: "Export an existing artifact version to a user-chosen file. This WRITE tool requires explicit user approval AND a destination path chosen in the confirmation dialog.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "artifactId": {"type": "string"},
                    "format": {"type": "string", "enum": ["markdown", "docx"]}
                },
                "required": ["artifactId", "format"],
                "additionalProperties": false
            }),
        },
    ]
}

#[derive(Debug, Clone)]
pub enum WritePayload {
    Artifact {
        kind: String,
        title: String,
        content: Value,
        coverage: CoverageReport,
        citations: Vec<CitationDraftForCommit>,
    },
    Quiz {
        config: QuizGenerationConfig,
        materials: QuizGenerationMaterials,
        title: String,
        materials_truncated: bool,
    },
    Export {
        artifact_id: String,
        version_id: String,
        format: String,
        artifact_title: String,
    },
}

#[derive(Debug, Clone)]
pub struct PreparedWriteTool {
    pub payload: WritePayload,
    pub confirmation: AgentConfirmationView,
    pub args_summary: String,
}

#[derive(Debug)]
pub struct WriteExecution {
    pub tool_result: Value,
    pub step_summary: String,
}

pub struct WriteToolDeps<'a> {
    pub artifacts: &'a ArtifactService,
    pub evidence: &'a EvidenceService,
    pub structured_generation: Option<&'a Arc<dyn StructuredGenerationPort>>,
    pub clock: &'a Arc<dyn Clock>,
    pub ids: &'a Arc<dyn IdGenerator>,
    pub document_id: &'a str,
    pub task_id: &'a str,
    pub attempt_id: &'a str,
    pub run_id: &'a str,
    pub step_id: &'a str,
    pub snapshot: &'a ContextSnapshotView,
}

/// 只做静态/只读校验并构造确认投影;不会写 artifact、文件或导出记录。
pub fn prepare_write_tool(
    call: &ToolCall,
    ctx: &AgentToolContext,
    snapshot: &ContextSnapshotView,
    artifacts: &ArtifactService,
) -> Result<PreparedWriteTool, AppError> {
    let args: Value =
        serde_json::from_str(&call.arguments_json).unwrap_or_else(|_| serde_json::json!({}));
    match call.name.as_str() {
        "create_artifact" => prepare_artifact(&args, ctx, snapshot),
        "create_quiz" => prepare_quiz(&args, ctx, snapshot),
        "export_file" => prepare_export(&args, artifacts),
        other => {
            Err(tool_args_invalid("unknown_write_tool")
                .with_detail("tool", other.to_string().into()))
        }
    }
}

fn prepare_artifact(
    args: &Value,
    ctx: &AgentToolContext,
    snapshot: &ContextSnapshotView,
) -> Result<PreparedWriteTool, AppError> {
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tool_args_invalid("artifact_kind_missing"))?
        .to_string();
    if kind == "quiz" {
        // 题目必须走 create_quiz(QuizGenerator 的材料/引用/有效性校验),
        // 不允许用自由 content 绕过。
        return Err(tool_args_invalid("use_create_quiz_for_quiz"));
    }
    let content = args
        .get("content")
        .cloned()
        .ok_or_else(|| tool_args_invalid("artifact_content_missing"))?;
    if !content.is_object() {
        return Err(tool_args_invalid("artifact_content_not_object"));
    }
    let content_chars = serde_json::to_string(&content)
        .map_err(|e| AppError::internal(format!("artifact_content_encode_failed: {e}")))?
        .chars()
        .count();
    if content_chars > MAX_WRITE_CONTENT_CHARS {
        return Err(tool_args_invalid("artifact_content_too_large")
            .with_detail("maxChars", (MAX_WRITE_CONTENT_CHARS as i64).into()));
    }
    // 已知 kind 在确认前先做结构校验;未知 kind 由 ArtifactService Codec 拒绝。
    let parsed_summary = if kind == "summary" {
        Some(validate_summary_v1(&content).map_err(|_| {
            AppError::new(CoreErrorCode::ArtifactSchemaInvalid, new_trace_id())
                .with_detail("reason", "summary_content_invalid".into())
        })?)
    } else {
        None
    };
    // 可选的 citations 与 summary 的 document_fact 段绑定;写入 M05 时
    // claim_key = 段 id,claim_kind/quote 逐条校验。
    let citations = parse_artifact_citations(args, ctx)?;
    if let Some(summary_content) = &parsed_summary {
        for section in &summary_content.sections {
            for segment in &section.segments {
                if segment.kind == crate::artifacts::models::SummarySegmentKind::DocumentFact
                    && !citations
                        .iter()
                        .any(|citation| citation.claim_key == segment.id)
                {
                    return Err(AppError::new(
                        CoreErrorCode::ArtifactSchemaInvalid,
                        new_trace_id(),
                    )
                    .with_detail("reason", "summary_citation_missing".into()));
                }
            }
        }
    }
    let title = title_or(
        args.get("title").and_then(|v| v.as_str()),
        format!("产物 · {}", snapshot.summary.name),
    );
    let coverage = coverage_for(snapshot, snapshot.summary.block_count.max(0));
    let summary = format!(
        "类型={kind};标题={};内容 {content_chars} 字符;引用 {} 条",
        display_title(&title),
        citations.len()
    );
    Ok(PreparedWriteTool {
        payload: WritePayload::Artifact {
            kind,
            title,
            content,
            coverage,
            citations,
        },
        confirmation: AgentConfirmationView {
            tool: "create_artifact".into(),
            summary,
            requires_path: false,
            artifact_id: None,
            format: None,
            suggested_filename: None,
        },
        args_summary: format!(
            "kind_chars={}, content_chars={}, title_chars={}",
            args.get("kind")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0),
            content_chars,
            args.get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0)
        ),
    })
}

fn prepare_quiz(
    args: &Value,
    ctx: &AgentToolContext,
    snapshot: &ContextSnapshotView,
) -> Result<PreparedWriteTool, AppError> {
    let config_value = args.get("config").cloned().unwrap_or_else(|| args.clone());
    let config: QuizGenerationConfig = serde_json::from_value(config_value)
        .map_err(|_| tool_args_invalid("quiz_config_invalid"))?;
    validate_generation_config(&config)?;
    let (materials, materials_truncated) = quiz_materials(ctx, snapshot);
    validate_generation_materials(&materials)?;
    let title = title_or(
        args.get("title").and_then(|v| v.as_str()),
        format!("练习题 · {}", snapshot.summary.name),
    );
    let difficulties = config
        .difficulties
        .iter()
        .map(|d| d.as_str())
        .collect::<Vec<_>>()
        .join("/");
    let style = if config.style.trim().is_empty() {
        "概念理解".to_string()
    } else {
        config.style.trim().to_string()
    };
    let summary = format!(
        "题目数量={};难度={};风格={};材料块={}{}",
        config.count,
        difficulties,
        display_title(&style),
        materials.blocks.len(),
        if materials_truncated {
            "(已按预算截断)"
        } else {
            ""
        }
    );
    Ok(PreparedWriteTool {
        payload: WritePayload::Quiz {
            config,
            materials,
            title,
            materials_truncated,
        },
        confirmation: AgentConfirmationView {
            tool: "create_quiz".into(),
            summary,
            requires_path: false,
            artifact_id: None,
            format: None,
            suggested_filename: None,
        },
        args_summary: format!(
            "count={}, difficulties={}, style_chars={}",
            args.get("config")
                .and_then(|v| v.get("count"))
                .and_then(|v| v.as_u64())
                .unwrap_or(5),
            args.get("config")
                .and_then(|v| v.get("difficulties"))
                .and_then(|v| v.as_array())
                .map(|v| v.len())
                .unwrap_or(0),
            args.get("config")
                .and_then(|v| v.get("style"))
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0)
        ),
    })
}

fn parse_artifact_citations(
    args: &Value,
    ctx: &AgentToolContext,
) -> Result<Vec<CitationDraftForCommit>, AppError> {
    let Some(items) = args.get("citations").and_then(|value| value.as_array()) else {
        return Ok(Vec::new());
    };
    if items.len() > 64 {
        return Err(tool_args_invalid("artifact_citations_too_many"));
    }
    let mut citations = Vec::with_capacity(items.len());
    for item in items {
        let claim_key = item
            .get("claimKey")
            .or_else(|| item.get("claim_key"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_args_invalid("artifact_claim_key_missing"))?;
        let block_id = item
            .get("blockId")
            .or_else(|| item.get("block_id"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_args_invalid("artifact_citation_block_missing"))?;
        let quoted_text = item
            .get("quotedText")
            .or_else(|| item.get("quoted_text"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| tool_args_invalid("artifact_citation_quote_missing"))?;
        let kind_raw = item
            .get("claimKind")
            .or_else(|| item.get("claim_kind"))
            .and_then(|value| value.as_str())
            .unwrap_or("document_fact");
        let claim_kind = ClaimKind::parse_db(kind_raw)
            .ok_or_else(|| tool_args_invalid("artifact_claim_kind_invalid"))?;
        let Some(block) = ctx.blocks.iter().find(|block| block.id == block_id) else {
            return Err(tool_args_invalid("artifact_citation_block_not_allowed"));
        };
        if !crate::quiz::validation::quote_matches_block(&block.text, quoted_text) {
            return Err(tool_args_invalid("artifact_citation_quote_mismatch"));
        }
        citations.push(CitationDraftForCommit {
            claim_key: claim_key.to_string(),
            claim_kind,
            content_block_id: block_id.to_string(),
            quoted_text: quoted_text.to_string(),
        });
    }
    Ok(citations)
}

fn prepare_export(
    args: &Value,
    artifacts: &ArtifactService,
) -> Result<PreparedWriteTool, AppError> {
    let artifact_id = args
        .get("artifactId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tool_args_invalid("export_artifact_id_missing"))?
        .to_string();
    let format = args
        .get("format")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tool_args_invalid("export_format_missing"))?
        .to_string();
    crate::exporters::parse_export_format(&format)?;
    let view = artifacts.get_artifact(&artifact_id, None)?;
    let version_id = view
        .current_version_id
        .clone()
        .ok_or_else(|| tool_args_invalid("export_artifact_version_missing"))?;
    let version_no = view
        .current_version
        .as_ref()
        .map(|version| version.version_no)
        .or_else(|| {
            view.versions
                .iter()
                .find(|version| version.id == version_id)
                .map(|version| version.version_no)
        })
        .unwrap_or(0);
    let suggested_filename = suggested_export_filename(&view.title, version_no, &format);
    let summary = format!(
        "产物={};第 {version_no} 版;格式={format}",
        display_title(&view.title)
    );
    Ok(PreparedWriteTool {
        payload: WritePayload::Export {
            artifact_id: artifact_id.clone(),
            version_id,
            format: format.clone(),
            artifact_title: view.title,
        },
        confirmation: AgentConfirmationView {
            tool: "export_file".into(),
            summary,
            requires_path: true,
            artifact_id: Some(artifact_id),
            format: Some(format.clone()),
            suggested_filename: Some(suggested_filename),
        },
        args_summary: format!(
            "artifact_id_chars={}, format={format}",
            args.get("artifactId")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0)
        ),
    })
}

/// 执行已批准的写工具。调用方必须在批准后、执行前再次检查取消。
pub fn execute_write_tool(
    deps: &WriteToolDeps<'_>,
    prepared: &PreparedWriteTool,
    path: Option<&str>,
    cancel: &CancellationToken,
) -> Result<WriteExecution, AppError> {
    if cancel.is_cancelled() {
        return Err(AppError::new(CoreErrorCode::TaskCanceled, new_trace_id()));
    }
    match &prepared.payload {
        WritePayload::Artifact {
            kind,
            title,
            content,
            coverage,
            citations,
        } => {
            let idempotency_key = format!("agent:{}:{}:artifact", deps.run_id, deps.step_id);
            let view = deps.artifacts.commit_ai_draft(
                &AiDraftCommitInput {
                    document_id: deps.snapshot.document_id.clone(),
                    kind: kind.clone(),
                    title: title.clone(),
                    content: content.clone(),
                    coverage: coverage.clone(),
                    snapshot_id: Some(deps.snapshot.id.clone()),
                    task_id: None,
                    citations: Vec::new(),
                },
                &idempotency_key,
            )?;
            let version_id = view
                .current_version_id
                .clone()
                .ok_or_else(|| AppError::internal("artifact_version_missing"))?;
            save_artifact_citations(deps, &version_id, citations)?;
            Ok(WriteExecution {
                tool_result: serde_json::json!({
                    "tool": "create_artifact",
                    "status": "ok",
                    "artifactId": view.id,
                    "artifactVersionId": version_id,
                    "kind": view.kind,
                    "title": view.title,
                    "citationCount": citations.len(),
                }),
                step_summary: format!(
                    "artifact_id={}, version_id={}, kind={}, citations={}",
                    view.id,
                    version_id,
                    view.kind,
                    citations.len()
                ),
            })
        }
        WritePayload::Quiz {
            config,
            materials,
            title,
            materials_truncated,
        } => {
            let port = deps.structured_generation.ok_or_else(|| {
                AppError::new(CoreErrorCode::Internal, new_trace_id())
                    .with_detail("reason", "quiz_port_not_wired".into())
            })?;
            let adapter = Arc::new(
                QuizGenerationPortAdapter::new(Arc::clone(port), deps.document_id.to_string())
                    .with_cancel(cancel.clone()),
            );
            let generator = QuizGenerator::new(adapter);
            let outcome = generator.generate(&QuizGenerationContext {
                task_id: deps.task_id.to_string(),
                attempt_id: deps.attempt_id.to_string(),
                snapshot_id: deps.snapshot.id.clone(),
                locale: "zh-CN".into(),
                config: config.clone(),
                materials: materials.clone(),
            })?;
            if cancel.is_cancelled() {
                return Err(AppError::new(CoreErrorCode::TaskCanceled, new_trace_id()));
            }
            let valid_questions: Vec<_> = outcome
                .report
                .content
                .questions
                .into_iter()
                .filter(|question| question.validity == QuestionValidity::Valid)
                .collect();
            if valid_questions.is_empty() {
                return Err(crate::quiz::validation::quiz_question_invalid(
                    "no_valid_questions",
                ));
            }
            let content = QuizContentV1 {
                schema_version: QUIZ_SCHEMA_VERSION.into(),
                questions: valid_questions,
            };
            let question_count = content.questions.len();
            let invalid_count = outcome.report.invalid_count;
            let citation_drafts: Vec<CitationDraftForCommit> = content
                .questions
                .iter()
                .flat_map(|question| {
                    question
                        .citations
                        .iter()
                        .map(move |citation| CitationDraftForCommit {
                            claim_key: question.id.clone(),
                            claim_kind: ClaimKind::DocumentFact,
                            content_block_id: citation.block_id.clone(),
                            quoted_text: citation.quoted_text.clone(),
                        })
                })
                .collect();
            let content_json =
                serde_json::to_value(&content).map_err(|e| AppError::internal(e.to_string()))?;
            let mut coverage = coverage_for(deps.snapshot, materials.blocks.len() as i64);
            if *materials_truncated {
                coverage.complete = false;
            }
            let idempotency_key = format!("agent:{}:{}:quiz", deps.run_id, deps.step_id);
            let view = deps.artifacts.commit_ai_draft(
                &AiDraftCommitInput {
                    document_id: deps.snapshot.document_id.clone(),
                    kind: "quiz".into(),
                    title: title.clone(),
                    content: content_json,
                    coverage,
                    snapshot_id: Some(deps.snapshot.id.clone()),
                    task_id: None,
                    citations: Vec::new(),
                },
                &idempotency_key,
            )?;
            let version_id = view
                .current_version_id
                .clone()
                .ok_or_else(|| AppError::internal("artifact_version_missing"))?;
            save_artifact_citations(deps, &version_id, &citation_drafts)?;
            Ok(WriteExecution {
                tool_result: serde_json::json!({
                    "tool": "create_quiz",
                    "status": "ok",
                    "artifactId": view.id,
                    "artifactVersionId": version_id,
                    "kind": "quiz",
                    "questionCount": question_count,
                    "invalidCount": invalid_count,
                    "citationCount": citation_drafts.len(),
                }),
                step_summary: format!(
                    "artifact_id={}, version_id={}, questions={}, invalid={}, citations={}",
                    view.id,
                    version_id,
                    question_count,
                    invalid_count,
                    citation_drafts.len()
                ),
            })
        }
        WritePayload::Export {
            artifact_id,
            version_id,
            format,
            artifact_title,
        } => {
            let path = path
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| tool_args_invalid("export_path_required"))?;
            let export_deps = crate::exporters::ExportDeps {
                service: deps.artifacts,
                clock: deps.clock.clone(),
                ids: deps.ids.clone(),
                variant: "bilingual",
            };
            let result = crate::exporters::export_artifact(
                &export_deps,
                artifact_id,
                version_id,
                format,
                Path::new(path),
            )?;
            Ok(WriteExecution {
                tool_result: serde_json::json!({
                    "tool": "export_file",
                    "status": "ok",
                    "artifactId": artifact_id,
                    "artifactVersionId": result.artifact_version_id,
                    "format": result.format,
                    "bytesWritten": result.bytes_written,
                    "exportRecordId": result.record_id,
                    "artifactTitle": artifact_title,
                }),
                step_summary: format!(
                    "artifact_version_id={}, export_record_id={}, bytes={}, format={}",
                    result.artifact_version_id,
                    result.record_id,
                    result.bytes_written,
                    result.format
                ),
            })
        }
    }
}

fn save_artifact_citations(
    deps: &WriteToolDeps<'_>,
    version_id: &str,
    drafts: &[CitationDraftForCommit],
) -> Result<(), AppError> {
    if drafts.is_empty() {
        return Ok(());
    }
    let citations: Vec<CitationInput> = drafts
        .iter()
        .map(|draft| CitationInput {
            claim_key: draft.claim_key.clone(),
            claim_kind: draft.claim_kind,
            source_type: CitationSourceType::ContentBlock,
            source_id: Some(draft.content_block_id.clone()),
            anchor_id: None,
            content_block_id: Some(draft.content_block_id.clone()),
            quoted_text: draft.quoted_text.clone(),
            locator: None,
            support_status: None,
            support_source: None,
        })
        .collect();
    let request = SaveCitationsRequest {
        owner_type: CitationOwnerType::ArtifactVersion,
        owner_id: version_id.to_string(),
        document_id: deps.snapshot.document_id.clone(),
        document_version_id: deps.snapshot.document_version_id.clone(),
        citations,
        idempotency_key: format!("agent:{}:{}:citations", deps.run_id, deps.step_id),
    };
    deps.evidence.save_citations(&request)?;
    Ok(())
}

fn quiz_materials(
    ctx: &AgentToolContext,
    snapshot: &ContextSnapshotView,
) -> (QuizGenerationMaterials, bool) {
    let mut blocks = Vec::new();
    let mut total_chars = 0usize;
    let mut truncated = false;
    for block in &ctx.blocks {
        if blocks.len() >= MAX_QUIZ_BLOCKS {
            truncated = true;
            break;
        }
        let chars = block.text.chars().count();
        if total_chars + chars > MAX_QUIZ_MATERIAL_CHARS {
            truncated = true;
            break;
        }
        total_chars += chars;
        blocks.push(QuizMaterialBlock {
            block_id: block.id.clone(),
            ordinal: block.ordinal.unwrap_or(0),
            text: block.text.clone(),
        });
    }
    (
        QuizGenerationMaterials {
            document_version_id: snapshot.document_version_id.clone(),
            blocks,
        },
        truncated,
    )
}

fn coverage_for(snapshot: &ContextSnapshotView, block_count: i64) -> CoverageReport {
    CoverageReport {
        complete: snapshot.coverage.scope_covered,
        regions: vec![CoverageRegion {
            label: snapshot.summary.name.clone(),
            block_count,
        }],
    }
}

fn title_or(requested: Option<&str>, fallback: String) -> String {
    let candidate = requested
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .unwrap_or(fallback);
    candidate.chars().take(MAX_TITLE_CHARS).collect()
}

/// 确认框只显示安全标题:路径分隔符替换,避免暴露完整路径。
fn display_title(title: &str) -> String {
    let cleaned: String = title
        .trim()
        .chars()
        .map(|c| if c == '\\' || c == '/' { '-' } else { c })
        .collect();
    cleaned.chars().take(MAX_TITLE_CHARS).collect()
}

fn suggested_export_filename(title: &str, version_no: i64, format: &str) -> String {
    let sanitized: String = title
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ => c,
        })
        .collect();
    let trimmed = sanitized.trim().trim_matches('-');
    let base = if trimmed.is_empty() {
        "导出"
    } else {
        trimmed
    };
    let extension = if format == "docx" { "docx" } else { "md" };
    format!("{base}-v{version_no}.{extension}")
        .chars()
        .take(120)
        .collect()
}
