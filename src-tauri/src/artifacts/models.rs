//! M10 领域对象与对外 DTO(《01》§7、《M10》§2–§8)。
//!
//! 契约:Artifact 外壳与版本写入规则归本模块;AI 原版不可变,用户编辑产生
//! 可变新版本,封存后不可变,重新生成/恢复总是新增版本。任何写入前必须
//! 通过注册 Codec 的 Schema 校验(ARTIFACT_SCHEMA_INVALID 拒绝)。
//!
//! M09/M07 只提供内容与快照 ID,不直接写产物;M11/M12 的生成器经注册接入。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::shared::paging::Page;

/// 产物列表分页(artifact_list)。
pub type ArtifactPage = Page<ArtifactListItem>;

// —— 枚举 ——

/// 产物类型:至少 summary/quiz/graph;未来类型经注册扩展。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ArtifactKind(pub String);

/// 版本来源(《M10》§2):ai_draft/copy 不可变;user_edit 可变直至封版;
/// restore 是历史版本的新副本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ArtifactVersionOrigin {
    AiDraft,
    UserEdit,
    Copy,
    Restore,
}

impl ArtifactVersionOrigin {
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

/// 版本状态:mutable 可自动保存;sealed 不可变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ArtifactVersionState {
    Mutable,
    Sealed,
}

impl ArtifactVersionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mutable => "mutable",
            Self::Sealed => "sealed",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "mutable" => Self::Mutable,
            "sealed" => Self::Sealed,
            _ => return None,
        })
    }
}

// —— 摘要内容结构(《M10》§6)——

/// 段落类型:document_fact 必须有有效 Citation;user_view 是用户观点;
/// ai_inference 是模型推断;needs_verification 是待核实项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum SummarySegmentKind {
    DocumentFact,
    AiInference,
    UserView,
    NeedsVerification,
}

impl SummarySegmentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DocumentFact => "document_fact",
            Self::AiInference => "ai_inference",
            Self::UserView => "user_view",
            Self::NeedsVerification => "needs_verification",
        }
    }

    pub fn parse_db(s: &str) -> Option<Self> {
        Some(match s {
            "document_fact" => Self::DocumentFact,
            "ai_inference" => Self::AiInference,
            "user_view" => Self::UserView,
            "needs_verification" => Self::NeedsVerification,
            _ => return None,
        })
    }
}

/// 摘要段落:citationIds 指向 M05 Citation(owner=artifact_version, claimKey=segment id)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SummarySegment {
    pub id: String,
    pub kind: SummarySegmentKind,
    pub text: String,
    #[serde(default)]
    pub citation_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SummarySection {
    pub id: String,
    pub title: String,
    pub segments: Vec<SummarySegment>,
}

/// 覆盖报告(《M10》§5):complete=false 时未生成范围不得描述为全文覆盖。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct CoverageReport {
    pub complete: bool,
    /// 明确覆盖的原文范围(章节/选区描述);缺失范围不进入该列表。
    #[serde(default)]
    pub regions: Vec<CoverageRegion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct CoverageRegion {
    /// 章节标题或选区描述。
    pub label: String,
    #[ts(type = "number")]
    pub block_count: i64,
}

/// `summary.v1` 内容 Schema(《M10》§6)。Codect 校验入口:
/// `validate_summary_v1`,结构非法 ⇒ ARTIFACT_SCHEMA_INVALID。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SummaryContentV1 {
    pub schema_version: String,
    pub sections: Vec<SummarySection>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    pub coverage: CoverageReport,
}

// —— 视图 ——

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ArtifactVersionView {
    pub id: String,
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent_version_id: Option<String>,
    #[ts(type = "number")]
    pub version_no: i64,
    pub origin: ArtifactVersionOrigin,
    pub state: ArtifactVersionState,
    pub schema_version: String,
    /// 内容按 Schema 校验后的结构化 JSON(编辑器不直接暴露原始 JSON)。
    #[ts(type = "unknown")]
    pub content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_id: Option<String>,
    pub coverage: CoverageReport,
    #[ts(type = "number")]
    pub revision: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub sealed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ArtifactView {
    pub id: String,
    pub document_id: String,
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current_version_id: Option<String>,
    /// 当前版本摘要(artifact_get 展开为完整版本)。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current_version: Option<Box<ArtifactVersionView>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<ArtifactVersionView>,
    /// 来源数量(当前版本的 Citation 总数,来源面板用)。
    #[ts(type = "number")]
    pub citation_count: i64,
    #[ts(type = "number")]
    pub revision: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// 重新生成结果:同一 artifact 追加的新 ai_draft 版本。
/// `artifact.currentVersionId` 可能仍是用户编辑版本,前端必须用
/// `versionId` 选中新生成的版本,不得假设它已成为当前版本。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ArtifactRegenerateResult {
    pub artifact: ArtifactView,
    pub version_id: String,
}

/// 产物中心列表条目(《M10》§7)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ArtifactListItem {
    pub id: String,
    pub document_id: String,
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current_version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current_version_no: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current_version_state: Option<ArtifactVersionState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coverage_complete: Option<bool>,
    #[ts(type = "number")]
    pub citation_count: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// 删除预览(《M10》§9):引用、版本与关联待澄清项,不能删除原文件/笔记。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ArtifactDeletePreview {
    pub artifact_id: String,
    #[ts(type = "number")]
    pub version_count: i64,
    #[ts(type = "number")]
    pub citation_count: i64,
    #[ts(type = "number")]
    pub export_record_count: i64,
    /// 引用该产物的待澄清项数量(提示,不级联删除)。
    #[ts(type = "number")]
    pub clarification_ref_count: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ExportResult {
    pub record_id: String,
    pub artifact_version_id: String,
    pub format: String,
    /// 导出字节数(诊断,不含路径)。
    #[ts(type = "number")]
    pub bytes_written: i64,
}

/// 导出前预览(《M10》§8:展示将包含的正文、引用快照和覆盖信息)。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ArtifactExportPreview {
    pub artifact_id: String,
    pub version_id: String,
    pub format: String,
    /// 确定性 Markdown 预览(与实际导出同源)。
    pub markdown: String,
}

// —— 输入 ——

/// `artifact_begin_edit`:首次编辑从 AI/封存版本复制创建 user_edit;
/// 已有可变用户版本则复用(不新建)。
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ArtifactBeginEditInput {
    pub artifact_id: String,
    pub base_version_id: String,
}

/// `artifact_save`:只有 user_edit + mutable 版本可保存;expectedRevision 冲突
/// 返回 CONFLICT_REVISION_MISMATCH,不覆盖。
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ArtifactSaveInput {
    pub artifact_id: String,
    pub version_id: String,
    #[ts(type = "unknown")]
    pub content: serde_json::Value,
    #[ts(type = "number")]
    pub expected_revision: i64,
}

/// 摘要段落编辑指令(《M10》§7):段落级结构,不暴露原始 JSON。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct SummaryEditOp {
    /// 目标段落 id;新段落时为 null。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub segment_id: Option<String>,
    /// set_text 修改文本;set_kind 改段落类型;add/remove 增删段落;
    /// rebind_citation 重绑/移除引用(settlement 后降级标记)。
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<SummarySegmentKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub citation_id: Option<String>,
}

// —— 生成器注册契约(《M10》§4)——

/// Codect:kind → Schema 校验/迁移。M13 注册内置实现(summary.v1);
/// M11/M12 注册 quiz/graph。未注册 kind 的内容不能写入产物。
pub trait ArtifactCodec: Send + Sync {
    fn kind(&self) -> &str;
    fn schema_version(&self) -> &str;
    /// 写入前校验:结构非法返回 false ⇒ ARTIFACT_SCHEMA_INVALID。
    fn validate(&self, content: &serde_json::Value) -> bool;
}

/// 内置 summary.v1 Codec。
pub struct SummaryV1Codec;

impl ArtifactCodec for SummaryV1Codec {
    fn kind(&self) -> &str {
        "summary"
    }

    fn schema_version(&self) -> &str {
        "summary.v1"
    }

    fn validate(&self, content: &serde_json::Value) -> bool {
        validate_summary_v1(content).is_ok()
    }
}

/// summary.v1 结构校验(《M10》§6)。
pub fn validate_summary_v1(content: &serde_json::Value) -> Result<SummaryContentV1, String> {
    let parsed: SummaryContentV1 = serde_json::from_value(content.clone())
        .map_err(|e| format!("summary.v1 parse failed: {e}"))?;
    if parsed.schema_version != "summary.v1" {
        return Err(format!(
            "unexpected schemaVersion: {}",
            parsed.schema_version
        ));
    }
    if parsed.sections.is_empty() {
        return Err("sections must not be empty".into());
    }
    let mut seen = std::collections::HashSet::new();
    for section in &parsed.sections {
        if section.id.is_empty() || section.title.is_empty() {
            return Err("section id/title must not be empty".into());
        }
        if !seen.insert(&section.id) {
            return Err(format!("duplicate section id: {}", section.id));
        }
        if section.segments.is_empty() {
            return Err("section segments must not be empty".into());
        }
        for seg in &section.segments {
            if seg.id.is_empty() {
                return Err("segment id must not be empty".into());
            }
            if seg.text.trim().is_empty() {
                return Err("segment text must not be empty".into());
            }
            if !seen.insert(&seg.id) {
                return Err(format!("duplicate segment id: {}", seg.id));
            }
            // 引用规则:document_fact 必须携带 citation;非事实段不得伪装。
            if seg.kind == SummarySegmentKind::DocumentFact && seg.citation_ids.is_empty() {
                return Err(format!(
                    "document_fact segment {} requires at least one citation",
                    seg.id
                ));
            }
        }
    }
    Ok(parsed)
}

/// 产物内容 → 纯文本(供 M07 引用资料 / Prompt 装填)。
///
/// 只提取可读正文:不输出 JSON 字段名、内部 ID 或 [SOURCE] 标记;
/// 未知 schema 返回空串(调用方拒绝该来源,不把原始 JSON 灌进 Prompt)。
pub fn artifact_content_text(schema_version: &str, content_json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content_json) else {
        return String::new();
    };
    let sanitize = crate::shared::sanitize::sanitize_generated_markdown;
    let mut out: Vec<String> = Vec::new();
    match schema_version {
        "summary.v1" => {
            if let Some(sections) = value.get("sections").and_then(|v| v.as_array()) {
                for section in sections {
                    if let Some(title) = section.get("title").and_then(|v| v.as_str()) {
                        out.push(format!("## {}", sanitize(title)));
                    }
                    if let Some(segments) = section.get("segments").and_then(|v| v.as_array()) {
                        for segment in segments {
                            if let Some(text) = segment.get("text").and_then(|v| v.as_str()) {
                                out.push(sanitize(text));
                            }
                        }
                    }
                }
            }
            if let Some(questions) = value.get("openQuestions").and_then(|v| v.as_array()) {
                for question in questions {
                    if let Some(text) = question.as_str() {
                        out.push(format!("- {}", sanitize(text)));
                    }
                }
            }
        }
        "quiz.v1" | "quiz.v2" => {
            if let Some(questions) = value.get("questions").and_then(|v| v.as_array()) {
                for (index, question) in questions.iter().enumerate() {
                    if let Some(prompt) = question.get("prompt").and_then(|v| v.as_str()) {
                        out.push(format!("第 {} 题:{}", index + 1, sanitize(prompt)));
                    }
                    if let Some(options) = question.get("options").and_then(|v| v.as_array()) {
                        for option in options {
                            if let Some(text) = option.get("text").and_then(|v| v.as_str()) {
                                out.push(format!("- {}", sanitize(text)));
                            }
                        }
                    }
                    if let Some(reference) = question
                        .get("answer")
                        .and_then(|answer| answer.get("referenceAnswer"))
                        .and_then(|v| v.as_str())
                        .filter(|text| !text.trim().is_empty())
                    {
                        out.push(format!("参考答案:{}", sanitize(reference)));
                    }
                    if let Some(segments) = question
                        .get("explanationSegments")
                        .and_then(|v| v.as_array())
                    {
                        for segment in segments {
                            if let Some(text) = segment.get("text").and_then(|v| v.as_str()) {
                                out.push(sanitize(text));
                            }
                        }
                    }
                }
            }
        }
        "translation.v1" => {
            if let Some(segments) = value.get("segments").and_then(|v| v.as_array()) {
                for segment in segments {
                    if let Some(translated) = segment
                        .get("translatedText")
                        .and_then(|v| v.as_str())
                        .filter(|text| !text.trim().is_empty())
                    {
                        out.push(sanitize(translated));
                    }
                }
            }
            if let Some(glossary) = value.get("glossary").and_then(|v| v.as_array()) {
                for entry in glossary {
                    let source = entry.get("source").and_then(|v| v.as_str()).unwrap_or("");
                    let target = entry.get("target").and_then(|v| v.as_str()).unwrap_or("");
                    if !source.is_empty() && !target.is_empty() {
                        out.push(format!("{source} → {target}"));
                    }
                }
            }
        }
        "graph.v1" => {
            if let Some(nodes) = value.get("nodes").and_then(|v| v.as_array()) {
                for node in nodes {
                    if let Some(label) = node.get("label").and_then(|v| v.as_str()) {
                        out.push(format!("- {}", sanitize(label)));
                    }
                }
            }
            if let Some(edges) = value.get("edges").and_then(|v| v.as_array()) {
                for edge in edges {
                    let source = edge.get("source").and_then(|v| v.as_str()).unwrap_or("");
                    let target = edge.get("target").and_then(|v| v.as_str()).unwrap_or("");
                    let label = edge.get("label").and_then(|v| v.as_str()).unwrap_or("");
                    out.push(format!("{source} → {target} {label}").trim().to_string());
                }
            }
        }
        _ => return String::new(),
    }
    out.join("\n").trim().to_string()
}

#[cfg(test)]
mod content_text_tests {
    use super::artifact_content_text;

    #[test]
    fn summary_text_is_plain_markdown_without_internal_markers() {
        let json = serde_json::json!({
            "schemaVersion": "summary.v1",
            "sections": [{
                "id": "s1",
                "title": "预算[SOURCE blk_1]",
                "segments": [{"id": "g1", "kind": "document_fact", "text": "预算为 120 万。", "citationIds": ["c1"]}]
            }],
            "openQuestions": ["为何超支?"],
            "coverage": {"complete": true, "regions": []}
        })
        .to_string();
        let text = artifact_content_text("summary.v1", &json);
        assert!(text.contains("## 预算"));
        assert!(!text.contains("[SOURCE"), "内部标记不得进入 Prompt 正文");
        assert!(text.contains("预算为 120 万。"));
        assert!(text.contains("- 为何超支?"));
    }

    #[test]
    fn unknown_schema_yields_empty_text() {
        assert_eq!(
            artifact_content_text("future.v9", r#"{"a":1}"#),
            "",
            "未知 schema 不把原始 JSON 灌进 Prompt"
        );
        assert_eq!(artifact_content_text("summary.v1", "not json"), "");
    }
}
