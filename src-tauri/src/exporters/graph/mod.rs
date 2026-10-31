//! M12 纯 SVG 序列化器(《M12》§7)。
//!
//! 约束:
//! - 不依赖浏览器截图,不使用 `foreignObject`;全部元素为原生 SVG 形状与文本。
//! - 从规范化 GraphContent + LayoutResult 生成:背景、标题、节点矩形、
//!   文本换行(中文整字换行,不截断)、边折线、箭头、虚线、证据编号与图例。
//! - 图例必须说明虚线/不确定证据含义;节点证据状态以字形+文字呈现,
//!   不只靠颜色。
//! - Metadata 注释只含标题、Schema 版本与计数,不含本机路径、密钥或
//!   完整文档正文;输出确定性(无时间戳),支撑黄金样例比对。
//! - PNG 转换由前端 Canvas 完成(§7),Rust 侧只产出 SVG。

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::graph::layout::wrap_label;
use crate::graph::models::*;
use crate::graph::outline::edge_text_marks;
use crate::graph::validate::semantic_node_order;

/// 系统安全字体栈(中文可渲染,不含本机路径)。
const FONT_STACK: &str = "'Microsoft YaHei','PingFang SC','Noto Sans CJK SC',sans-serif";
const TEXT_COLOR: &str = "#1F2A36";
const NODE_FILL: &str = "#F7F9FC";
const NODE_STROKE: &str = "#43536A";
const EDGE_COLOR: &str = "#43536A";
const CAPTION_COLOR: &str = "#5A6B7B";
const LEGEND_FILL: &str = "#FFFFFF";
const LEGEND_STROKE: &str = "#B9C4D0";

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 注释安全文本:去掉 `--` 与控制字符(注释体内不得出现双连字符)。
fn comment_safe(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace("--", "−−")
}

/// 证据状态标记:字形 + 文字(不只靠颜色,《M12》§5)。
pub fn evidence_badge(status: GraphEvidenceStatus) -> &'static str {
    match status {
        GraphEvidenceStatus::Supported => "● 已支持",
        GraphEvidenceStatus::Unverified => "○ 未核验",
        GraphEvidenceStatus::Insufficient => "▲ 证据不足",
        GraphEvidenceStatus::Conflict => "✕ 冲突",
    }
}

/// 节点类型短标签。
fn node_kind_caption(kind: GraphNodeKind, origin: GraphItemOrigin) -> String {
    let kind_label = match kind {
        GraphNodeKind::Step => "步骤",
        GraphNodeKind::Decision => "判断",
        GraphNodeKind::Claim => "主张",
        GraphNodeKind::Premise => "前提",
        GraphNodeKind::Conclusion => "结论",
        GraphNodeKind::Group => "分组",
    };
    if origin == GraphItemOrigin::User {
        format!("{kind_label}·用户观点")
    } else {
        kind_label.to_string()
    }
}

/// 图内证据编号:节点按语义顺序、边按输入顺序(与大纲一致)。
pub fn citation_numbering(content: &GraphContent) -> HashMap<String, usize> {
    let mut numbering = HashMap::new();
    let mut next = 1usize;
    for &i in semantic_node_order(content).iter() {
        for cit in &content.nodes[i].citations {
            numbering.insert(cit.id.clone(), next);
            next += 1;
        }
    }
    for edge in &content.edges {
        for cit in &edge.citations {
            numbering.insert(cit.id.clone(), next);
            next += 1;
        }
    }
    numbering
}

/// 折线中点(标签摆放位置)。
fn polyline_midpoint(points: &[GraphPoint]) -> GraphPoint {
    if points.is_empty() {
        return GraphPoint { x: 0.0, y: 0.0 };
    }
    if points.len() == 1 {
        return points[0];
    }
    // 取相邻点对的累计长度中点,保证标签落在路径上。
    let mut total = 0.0;
    let mut segments: Vec<(f64, (GraphPoint, GraphPoint))> = Vec::new();
    for w in points.windows(2) {
        let len = (w[1].x - w[0].x).hypot(w[1].y - w[0].y);
        segments.push((len, (w[0], w[1])));
        total += len;
    }
    let mut remain = total / 2.0;
    for (len, (a, b)) in segments {
        if remain <= len || len == 0.0 {
            let t = if len == 0.0 { 0.0 } else { remain / len };
            return GraphPoint {
                x: a.x + (b.x - a.x) * t,
                y: a.y + (b.y - a.y) * t,
            };
        }
        remain -= len;
    }
    points[points.len() - 1]
}

/// 引文摘要(图例证据编号行;截断保护,非完整正文)。
fn quote_digest(quote: &str) -> String {
    let chars: Vec<char> = quote.chars().take(24).collect();
    if quote.chars().count() > 24 {
        format!("{}…", chars.into_iter().collect::<String>())
    } else {
        chars.into_iter().collect()
    }
}

/// 序列化 SVG(确定性输出,黄金样例可比对)。
pub fn serialize_svg(content: &GraphContent, layout: &LayoutResult) -> String {
    let numbering = citation_numbering(content);
    let node_geom: HashMap<&str, &LayoutNode> =
        layout.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // 图例行数 → 图例高度。
    let citation_count: usize = content
        .nodes
        .iter()
        .map(|n| n.citations.len())
        .sum::<usize>()
        + content
            .edges
            .iter()
            .map(|e| e.citations.len())
            .sum::<usize>();
    let legend_rows = 4 + citation_count; // 4 行固定说明 + 每条证据一行
    let legend_height = 28.0 + legend_rows as f64 * 20.0;
    let svg_width = layout.width.max(480.0);
    let svg_height = layout.height + legend_height + 16.0;

    let mut svg = String::with_capacity(16 * 1024);
    let _ = writeln!(svg, r##"<?xml version="1.0" encoding="UTF-8"?>"##);
    let _ = writeln!(
        svg,
        "<!-- knowwise graph export | schema={} | title={} | nodes={} | edges={} | citations={} -->",
        comment_safe(&content.schema_version),
        comment_safe(&content.title),
        content.nodes.len(),
        content.edges.len(),
        citation_count
    );
    let _ = writeln!(
        svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{svg_width}" height="{svg_height}" viewBox="0 0 {svg_width} {svg_height}" font-family="{FONT_STACK}">"##
    );
    let _ = writeln!(
        svg,
        r##"  <defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="{EDGE_COLOR}"/></marker></defs>"##
    );
    // 背景。
    let _ = writeln!(
        svg,
        r##"  <rect x="0" y="0" width="{svg_width}" height="{svg_height}" fill="#FFFFFF"/"##,
    );
    // 标题(左上;布局已预留 TITLE_SPACE)。
    let _ = writeln!(
        svg,
        r##"  <text x="40" y="38" font-size="20" font-weight="600" fill="{TEXT_COLOR}">{}</text>"##,
        xml_escape(&content.title)
    );

    // 边(先画边,节点覆盖端点)。
    for edge in &content.edges {
        let Some(le) = layout.edges.iter().find(|l| l.id == edge.id) else {
            continue;
        };
        if le.points.is_empty() {
            continue;
        }
        let mut d = String::new();
        let _ = write!(d, "M {} {}", le.points[0].x, le.points[0].y);
        for p in &le.points[1..] {
            let _ = write!(d, " L {} {}", p.x, p.y);
        }
        let dash = if le.dashed {
            r##" stroke-dasharray="7 5"##
        } else {
            ""
        };
        let _ = writeln!(
            svg,
            r##"  <path d="{d}" fill="none" stroke="{EDGE_COLOR}" stroke-width="1.6"{dash} marker-end="url(#arrow)"/"##,
        );
        // 边标签:关系文字 + 文本标记(不确定/推断不能只靠虚线,《M12》§3)。
        let mut label_text = edge.label.clone().unwrap_or_default();
        for mark in edge_text_marks(edge) {
            label_text.push(' ');
            label_text.push_str(mark);
        }
        for cit in &edge.citations {
            if let Some(no) = numbering.get(&cit.id) {
                let _ = write!(label_text, " [{no}]");
            }
        }
        if !label_text.trim().is_empty() {
            let mid = polyline_midpoint(&le.points);
            let _ = writeln!(
                svg,
                r##"  <text x="{}" y="{}" font-size="12" fill="{CAPTION_COLOR}" text-anchor="middle">{}</text>"##,
                mid.x,
                mid.y - 6.0,
                xml_escape(label_text.trim())
            );
        }
    }

    // 节点。
    for node in &content.nodes {
        let Some(ln) = node_geom.get(node.id.as_str()) else {
            continue;
        };
        let x = ln.x;
        let y = ln.y;
        let _ = writeln!(
            svg,
            r##"  <rect x="{x}" y="{y}" width="{}" height="{}" rx="6" fill="{NODE_FILL}" stroke="{NODE_STROKE}" stroke-width="1.4"/"##,
            ln.width, ln.height
        );
        let caption = node_kind_caption(node.node_type, node.origin);
        let _ = writeln!(
            svg,
            r##"  <text x="{}" y="{}" font-size="11" fill="{CAPTION_COLOR}">{}</text>"##,
            x + 10.0,
            y + 16.0,
            xml_escape(&caption)
        );
        // 标签:与布局同一折行算法,中文整字换行不截断。
        for (li, line) in wrap_label(&node.label).iter().enumerate() {
            let _ = writeln!(
                svg,
                r##"  <text x="{}" y="{}" font-size="14" fill="{TEXT_COLOR}">{}</text>"##,
                x + 10.0,
                y + 36.0 + li as f64 * 20.0,
                xml_escape(line)
            );
        }
        // 证据行:状态标记 + 编号(不只靠颜色)。
        let mut badge = evidence_badge(node.evidence_status).to_string();
        for cit in &node.citations {
            if let Some(no) = numbering.get(&cit.id) {
                let _ = write!(badge, " [{no}]");
            }
        }
        let badge_y = y + ln.height - 8.0;
        let _ = writeln!(
            svg,
            r##"  <text x="{}" y="{badge_y}" font-size="11" fill="{CAPTION_COLOR}">{}</text>"##,
            x + 10.0,
            xml_escape(&badge)
        );
    }

    // 图例:虚线/证据含义说明(《M12》§7)。
    let legend_y = layout.height + 8.0;
    let _ = writeln!(
        svg,
        r##"  <rect x="24" y="{legend_y}" width="{}" height="{legend_height}" fill="{LEGEND_FILL}" stroke="{LEGEND_STROKE}" stroke-width="1"/"##,
        svg_width - 48.0
    );
    let _ = writeln!(
        svg,
        r##"  <text x="36" y="{}" font-size="13" font-weight="600" fill="{TEXT_COLOR}">图例</text>"##,
        legend_y + 20.0
    );
    let mut row = 1usize;
    let mut legend_line = |svg: &mut String, text: &str| {
        let _ = writeln!(
            svg,
            r##"  <text x="36" y="{}" font-size="12" fill="{TEXT_COLOR}">{}</text>"##,
            legend_y + 20.0 + row as f64 * 20.0,
            xml_escape(text)
        );
        row += 1;
    };
    legend_line(&mut svg, "实线 = 原文明确(stated)");
    legend_line(
        &mut svg,
        "虚线 = 推断(inferred)或不确定(uncertain),非原文直接陈述",
    );
    legend_line(
        &mut svg,
        "节点标记:● 已支持 ○ 未核验 ▲ 证据不足 ✕ 冲突;[n] 为下方证据编号",
    );
    legend_line(&mut svg, "绕行折线 = 回路(环形关系,非直线顺序)");
    // 证据编号清单:编号 → 块与引文摘要(非完整正文)。
    let mut cited: Vec<(usize, &GraphCitation)> = Vec::new();
    for node in &content.nodes {
        for cit in &node.citations {
            if let Some(no) = numbering.get(&cit.id) {
                cited.push((*no, cit));
            }
        }
    }
    for edge in &content.edges {
        for cit in &edge.citations {
            if let Some(no) = numbering.get(&cit.id) {
                cited.push((*no, cit));
            }
        }
    }
    cited.sort_by_key(|(no, _)| *no);
    for (no, cit) in cited {
        legend_line(
            &mut svg,
            &format!(
                "[{no}] 原文块 {} “{}”",
                cit.block_id,
                quote_digest(&cit.quoted_text)
            ),
        );
    }

    svg.push_str("</svg>\n");
    svg
}
