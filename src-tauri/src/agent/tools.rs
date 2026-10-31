//! P2 只读工具集(WBS-3)。
//!
//! 所有工具只读取当前冻结快照允许的块;越界返回空结果 + 原因,不报错崩溃。
//! 单次返回 ≤ 8000 字符,截断时 truncated=true 并给出剩余匹配数。

use std::collections::HashSet;

use crate::artifacts::service::ArtifactService;
use crate::context::models::{ContextSnapshotView, ScopeKind};
use crate::model_privacy::provider::{ToolCall, ToolSpec};
use crate::parse_index::models::ContentBlockView;
use crate::parse_index::service::ParseIndexService;
use crate::shared::paging::PageRequest;
use crate::shared::AppError;

use super::models::{
    tool_args_invalid, AgentToolContext, ToolResult, ToolResultItem, ToolResultSummary,
    TOOL_ITEM_MAX_CHARS, TOOL_RESULT_MAX_CHARS,
};

/// 工具清单。描述用于模型选择;参数 Schema 为最小 JSON Schema。
pub fn tool_specs() -> Vec<ToolSpec> {
    let mut specs = vec![
        ToolSpec {
            name: "search_blocks".into(),
            description: "Search source blocks inside the frozen snapshot. Returns item sourceId values that can be cited.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Keywords or phrase to search."},
                    "k": {"type": "integer", "minimum": 1, "maximum": 10, "default": 5}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "read_section".into(),
            description: "Read blocks under a heading path (prefix match). Use the heading path returned by search results.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "headingPath": {
                        "oneOf": [
                            {"type": "array", "items": {"type": "string"}},
                            {"type": "string"}
                        ],
                        "description": "Heading path; string form uses ' / ' or '>' separators."
                    }
                },
                "required": ["headingPath"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "read_pages".into(),
            description: "Read PDF blocks by page range (1-based, inclusive). Non-PDF snapshots return an empty result with a reason.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "from": {"type": "integer", "minimum": 1},
                    "to": {"type": "integer", "minimum": 1}
                },
                "required": ["from", "to"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "get_selection".into(),
            description: "Read the frozen selection blocks of the current snapshot. If the snapshot is not a selection, returns empty with a reason.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "list_artifacts".into(),
            description: "List existing artifacts (summary/quiz/graph) of the current document. Metadata only; no artifact body.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 20, "default": 10}
                },
                "additionalProperties": false
            }),
        },
    ];
    specs.extend(super::write_tools::write_tool_specs());
    specs
}

impl AgentToolContext {
    /// 从冻结快照装载允许块;没有代次/块时返回空集合(工具会给出原因)。
    pub fn load(
        parse: &ParseIndexService,
        snapshot: &ContextSnapshotView,
    ) -> Result<Self, AppError> {
        let mut blocks = if !snapshot.block_ids.is_empty() {
            parse.get_blocks_by_ids(&snapshot.block_ids)?
        } else if let Some(generation_id) = snapshot.parse_generation_id.as_deref() {
            parse.list_generation_blocks(generation_id)?
        } else {
            Vec::new()
        };
        blocks.sort_by_key(|block| block.ordinal.unwrap_or(i64::MAX));
        blocks.retain(|block| !block.text.trim().is_empty());
        let page_scope = match &snapshot.scope {
            crate::context::models::ScopeDraft::Pages {
                start_page,
                end_page,
            } => Some((*start_page, *end_page)),
            _ => None,
        };
        Ok(Self {
            document_id: snapshot.document_id.clone(),
            snapshot_id: snapshot.id.clone(),
            document_version_id: snapshot.document_version_id.clone(),
            parse_generation_id: snapshot.parse_generation_id.clone(),
            scope_kind: snapshot.scope_kind,
            page_scope,
            blocks,
        })
    }

    fn no_blocks_reason(&self) -> Option<String> {
        if self.blocks.is_empty() {
            Some("snapshot_has_no_blocks".into())
        } else {
            None
        }
    }
}

/// 执行一个只读工具。错误返回 Err(循环会转成 failed 步骤并把错误码回填模型)。
pub fn execute_tool(
    ctx: &AgentToolContext,
    parse: &ParseIndexService,
    artifacts: &ArtifactService,
    call: &ToolCall,
) -> Result<ToolResult, AppError> {
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments_json).unwrap_or_else(|_| serde_json::json!({}));
    match call.name.as_str() {
        "search_blocks" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .ok_or_else(|| tool_args_invalid("search_query_missing"))?;
            let k = args
                .get("k")
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .clamp(1, 10) as usize;
            let (items, total) = search_blocks(ctx, parse, query, k)?;
            Ok(ToolResult {
                tool: "search_blocks".into(),
                items,
                truncated: false,
                total_matches: total,
                note: None,
                reason: ctx
                    .no_blocks_reason()
                    .or_else(|| (total == 0).then(|| "no_matching_blocks_in_snapshot".into())),
            })
        }
        "read_section" => {
            let path = parse_heading_path(&args)?;
            let matched: Vec<&ContentBlockView> = ctx
                .blocks
                .iter()
                .filter(|block| heading_prefix_matches(&block.heading_path, &path))
                .collect();
            let total = matched.len();
            let items = matched.into_iter().map(block_item).collect();
            Ok(ToolResult {
                tool: "read_section".into(),
                items,
                truncated: false,
                total_matches: total,
                note: None,
                reason: ctx
                    .no_blocks_reason()
                    .or_else(|| (total == 0).then(|| "section_not_in_snapshot".into())),
            })
        }
        "read_pages" => {
            let from = args
                .get("from")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| tool_args_invalid("pages_from_missing"))?;
            let to = args
                .get("to")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| tool_args_invalid("pages_to_missing"))?;
            if from < 1 || to < from {
                return Err(tool_args_invalid("pages_range_invalid"));
            }
            let has_pages = ctx
                .blocks
                .iter()
                .any(|block| block.locator.pdf_page().is_some());
            if !has_pages {
                return Ok(ToolResult {
                    tool: "read_pages".into(),
                    items: Vec::new(),
                    truncated: false,
                    total_matches: 0,
                    note: None,
                    reason: Some("snapshot_is_not_pdf".into()),
                });
            }
            let matched: Vec<&ContentBlockView> = ctx
                .blocks
                .iter()
                .filter(|block| {
                    block
                        .locator
                        .pdf_page()
                        .map(|page| page >= from && page <= to)
                        .unwrap_or(false)
                })
                .collect();
            let total = matched.len();
            let items = matched.into_iter().map(block_item).collect();
            // 范围外页面不是“空页”:明确告诉模型未读取,避免被当成全文缺失。
            let (note, reason) = match ctx.page_scope {
                Some((start, end)) if to < start || from > end => (
                    Some(format!(
                        "requested pages are outside the selected scope (pages {start}-{end}); \
                         they were NOT read and must not be treated as absent from the document"
                    )),
                    Some("outside_selected_page_scope".to_string()),
                ),
                Some((start, end)) if from < start || to > end => (
                    Some(format!(
                        "request extends outside the selected scope (pages {start}-{end}); \
                         only in-scope pages were read"
                    )),
                    None,
                ),
                _ => (
                    None,
                    (total == 0).then(|| "no_blocks_in_page_range".to_string()),
                ),
            };
            Ok(ToolResult {
                tool: "read_pages".into(),
                items,
                truncated: false,
                total_matches: total,
                note,
                reason,
            })
        }
        "get_selection" => {
            if ctx.scope_kind != ScopeKind::Selection {
                return Ok(ToolResult {
                    tool: "get_selection".into(),
                    items: Vec::new(),
                    truncated: false,
                    total_matches: 0,
                    note: None,
                    reason: Some("snapshot_is_not_a_selection".into()),
                });
            }
            let items: Vec<ToolResultItem> = ctx.blocks.iter().take(10).map(block_item).collect();
            let total = ctx.blocks.len();
            Ok(ToolResult {
                tool: "get_selection".into(),
                items,
                truncated: total > 10,
                total_matches: total,
                note: None,
                reason: ctx.no_blocks_reason(),
            })
        }
        "list_artifacts" => {
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10)
                .clamp(1, 20) as i64;
            let page = artifacts.list_artifacts(
                &ctx.document_id,
                None,
                PageRequest {
                    limit,
                    cursor: None,
                },
            )?;
            let total = page.total.unwrap_or(page.items.len() as i64).max(0) as usize;
            let items = page
                .items
                .into_iter()
                .map(|artifact| ToolResultItem {
                    source_id: artifact.id,
                    kind: "artifact".into(),
                    title: Some(artifact.title),
                    page: None,
                    text: format!(
                        "kind={} version={} citations={}",
                        artifact.kind,
                        artifact
                            .current_version_no
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "-".into()),
                        artifact.citation_count
                    ),
                })
                .collect();
            Ok(ToolResult {
                tool: "list_artifacts".into(),
                items,
                truncated: false,
                total_matches: total,
                note: None,
                reason: (total == 0).then(|| "no_artifacts".into()),
            })
        }
        other => {
            Err(tool_args_invalid("unknown_tool").with_detail("tool", other.to_string().into()))
        }
    }
}

fn search_blocks(
    ctx: &AgentToolContext,
    parse: &ParseIndexService,
    query: &str,
    k: usize,
) -> Result<(Vec<ToolResultItem>, usize), AppError> {
    let allowed: HashSet<&str> = ctx.blocks.iter().map(|block| block.id.as_str()).collect();
    let mut ordered_ids: Vec<String> = Vec::new();
    // 首选 M04 FTS(代次内检索);显式快照再按允许集合过滤。
    if let Some(generation_id) = ctx.parse_generation_id.as_deref() {
        if let Ok(hits) = parse.fts_search(generation_id, &[query.to_string()], 200) {
            for hit in hits {
                if allowed.contains(hit.block_id.as_str())
                    && !ordered_ids.iter().any(|id| id == &hit.block_id)
                {
                    ordered_ids.push(hit.block_id);
                }
            }
        }
    }
    if ordered_ids.is_empty() {
        // FTS 不可用/无命中:快照内大小写不敏感子串匹配,保证工具仍可用。
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .map(str::to_string)
            .collect();
        if terms.is_empty() {
            return Err(tool_args_invalid("search_query_empty"));
        }
        let mut scored: Vec<(usize, &ContentBlockView)> = ctx
            .blocks
            .iter()
            .filter_map(|block| {
                let lower = block.text.to_lowercase();
                let score = terms
                    .iter()
                    .filter(|term| lower.contains(term.as_str()))
                    .count();
                (score > 0).then_some((score, block))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| {
                a.1.ordinal
                    .unwrap_or(i64::MAX)
                    .cmp(&b.1.ordinal.unwrap_or(i64::MAX))
            })
        });
        ordered_ids = scored
            .into_iter()
            .map(|(_, block)| block.id.clone())
            .collect();
    }
    let total = ordered_ids.len();
    let items = ordered_ids
        .iter()
        .take(k)
        .filter_map(|id| ctx.blocks.iter().find(|block| &block.id == id))
        .map(block_item)
        .collect();
    Ok((items, total))
}

fn parse_heading_path(args: &serde_json::Value) -> Result<Vec<String>, AppError> {
    let value = args
        .get("headingPath")
        .ok_or_else(|| tool_args_invalid("heading_path_missing"))?;
    let parts: Vec<String> = match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        serde_json::Value::String(raw) => raw
            .split(['/', '>'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    if parts.is_empty() {
        return Err(tool_args_invalid("heading_path_empty"));
    }
    Ok(parts)
}

fn heading_prefix_matches(block_path: &[String], prefix: &[String]) -> bool {
    block_path.len() >= prefix.len() && block_path.iter().zip(prefix.iter()).all(|(a, b)| a == b)
}

fn block_item(block: &ContentBlockView) -> ToolResultItem {
    ToolResultItem {
        source_id: block.id.clone(),
        kind: block.kind.as_str().to_string(),
        title: if block.heading_path.is_empty() {
            None
        } else {
            Some(block.heading_path.join(" / "))
        },
        page: block.locator.pdf_page(),
        text: block.text.clone(),
    }
}

/// 序列化工具结果并施加 8000 字符预算。
/// 截断时保留 `truncated=true` 与“结果已截断，还有 N 处匹配”,不静默丢弃。
pub fn encode_result(mut result: ToolResult) -> (String, ToolResultSummary) {
    let total = result.total_matches;
    let mut truncated = result.truncated || result.items.len() < total;
    for item in &mut result.items {
        if item.text.chars().count() > TOOL_ITEM_MAX_CHARS {
            item.text = item
                .text
                .chars()
                .take(TOOL_ITEM_MAX_CHARS)
                .collect::<String>()
                + "…";
            truncated = true;
        }
    }
    loop {
        result.truncated = truncated;
        result.note = if truncated {
            let remaining = total.saturating_sub(result.items.len());
            Some(if remaining > 0 {
                format!("结果已截断，还有 {remaining} 处匹配")
            } else {
                "结果已截断(部分条目内容超出预算)".to_string()
            })
        } else {
            None
        };
        let json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
        if json.chars().count() <= TOOL_RESULT_MAX_CHARS {
            let summary = ToolResultSummary {
                items: result.items.len(),
                truncated,
                total_matches: total,
                reason: result.reason.clone(),
            };
            return (json, summary);
        }
        if result.items.pop().is_none() {
            let minimal = serde_json::json!({
                "tool": result.tool,
                "items": [],
                "truncated": true,
                "totalMatches": total,
                "note": "结果已截断，超出工具预算",
                "reason": result.reason,
            });
            let summary = ToolResultSummary {
                items: 0,
                truncated: true,
                total_matches: total,
                reason: result.reason.clone(),
            };
            return (minimal.to_string(), summary);
        }
        truncated = true;
    }
}

/// 步骤 argsSummary:只保留参数规模/页码等非正文信息。
pub fn args_summary(call: &ToolCall) -> String {
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments_json).unwrap_or_else(|_| serde_json::json!({}));
    match call.name.as_str() {
        "search_blocks" => format!(
            "query_chars={}, k={}",
            args.get("query")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0),
            args.get("k").and_then(|v| v.as_u64()).unwrap_or(5)
        ),
        "read_section" => format!(
            "heading_parts={}",
            args.get("headingPath")
                .and_then(|v| v.as_array())
                .map(|v| v.len())
                .unwrap_or(1)
        ),
        "read_pages" => format!(
            "from={}, to={}",
            args.get("from").and_then(|v| v.as_i64()).unwrap_or(0),
            args.get("to").and_then(|v| v.as_i64()).unwrap_or(0)
        ),
        "get_selection" => "scope=selection".into(),
        "list_artifacts" => format!(
            "limit={}",
            args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10)
        ),
        "create_artifact" => format!(
            "kind_chars={}, content_chars={}, title_chars={}",
            args.get("kind")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0),
            args.get("content")
                .map(|v| v.to_string().chars().count())
                .unwrap_or(0),
            args.get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0)
        ),
        "create_quiz" => format!(
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
        "export_file" => format!(
            "artifact_id_chars={}, format={}",
            args.get("artifactId")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0),
            args.get("format").and_then(|v| v.as_str()).unwrap_or("")
        ),
        _ => "unknown_tool".into(),
    }
}
