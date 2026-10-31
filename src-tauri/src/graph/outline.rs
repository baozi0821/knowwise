//! M12 文本大纲(《M12》§6)。
//!
//! 大纲不是截图替代,而是按语义顺序列出节点、证据位置与后继关系;
//! 证据不足、节点过多、布局失败或用户选择时作为可编辑降级产物保存,
//! 降级状态写入 format 元数据,不伪装为完整图形渲染成功。

use std::collections::HashMap;

use super::models::*;
use super::validate::semantic_node_order;

/// 证据位置查找表:ContentBlock ID → 原文位置(由服务层只读查询 M04 表)。
pub type EvidenceIndex = HashMap<String, EvidencePosition>;

/// 证据行的位置描述:"第 2 页,第 3 段";无定位时退化为"原文块(未定位)"。
fn evidence_line(pos: Option<&EvidencePosition>) -> String {
    match pos {
        Some(p) => {
            let mut parts = Vec::new();
            if let Some(page) = p.page_number {
                parts.push(format!("第 {page} 页"));
            }
            if let Some(ord) = p.ordinal {
                parts.push(format!("第 {ord} 段"));
            }
            if parts.is_empty() {
                "原文块(未定位)".to_string()
            } else {
                parts.join(",")
            }
        }
        None => "原文块(未定位)".to_string(),
    }
}

/// 边的文本标记:推断/不确定/证据档位——不只靠颜色与虚线(《M12》§3/§5)。
pub fn edge_text_marks(edge: &GraphEdge) -> Vec<&'static str> {
    let mut marks = Vec::new();
    match edge.certainty {
        GraphCertainty::Stated => {}
        GraphCertainty::Inferred => marks.push("[推断]"),
        GraphCertainty::Uncertain => marks.push("[不确定]"),
    }
    match edge.evidence_status {
        GraphEvidenceStatus::Insufficient => marks.push("[依据不足]"),
        GraphEvidenceStatus::Unverified => marks.push("[未核验]"),
        GraphEvidenceStatus::Conflict => marks.push("[冲突]"),
        GraphEvidenceStatus::Supported => {}
    }
    marks
}

/// 生成文本大纲:条目按语义顺序,含证据行与后继行(《M12》§6 示例格式)。
pub fn build_outline(content: &GraphContent, evidence: &EvidenceIndex) -> GraphOutline {
    let order = semantic_node_order(content);
    let mut entries = Vec::with_capacity(order.len());
    for (seq, &i) in order.iter().enumerate() {
        let node = &content.nodes[i];
        let mut evidence_lines = Vec::new();
        for cit in &node.citations {
            evidence_lines.push(format!(
                "证据:{}",
                evidence_line(evidence.get(&cit.block_id))
            ));
        }
        let mut successor_lines = Vec::new();
        for edge in &content.edges {
            if edge.source != node.id {
                continue;
            }
            let target_label = content
                .node(&edge.target)
                .map(|t| t.label.clone())
                .unwrap_or_else(|| edge.target.clone());
            let rel = edge
                .label
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty());
            let mut line = match rel {
                Some(l) => format!("后续:{target_label}({l})"),
                None => format!("后续:{target_label}"),
            };
            for mark in edge_text_marks(edge) {
                line.push_str(&format!(" {mark}"));
            }
            successor_lines.push(line);
        }
        entries.push(GraphOutlineEntry {
            index: seq as i64 + 1,
            node_id: node.id.clone(),
            label: node.label.clone(),
            evidence_lines,
            successor_lines,
        });
    }
    let text = serialize_outline_text(&entries);
    GraphOutline {
        title: content.title.clone(),
        graph_type: content.graph_type,
        entries,
        text,
    }
}

/// 规范文本格式(与《M12》§6 示例一致):
/// `1. 标签` / 缩进 `证据:…` / 缩进 `后续:…`。
pub fn serialize_outline_text(entries: &[GraphOutlineEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!("{}. {}\n", entry.index, entry.label));
        for line in &entry.evidence_lines {
            out.push_str(&format!("   {line}\n"));
        }
        for line in &entry.successor_lines {
            out.push_str(&format!("   {line}\n"));
        }
    }
    out
}
