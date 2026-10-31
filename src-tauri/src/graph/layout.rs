//! M12 分层布局(《M12》§4)。
//!
//! 设计采用与 Dagre 同族的分层(Sugiyama 简化)算法在 Rust 侧确定性实现:
//! 断环(DFS 回边)→ 最长路径定层 → 重心法排序 → 坐标分配 → 正交边路由。
//! React Flow 只消费 `LayoutResult`,其内部对象不持久化。
//!
//! 降级规则(不产出空白产物,《M12》§4/§6):
//! - 节点/边超过布局预算(与校验上限一致)→ `budget_exceeded`;
//! - 回边占比过半(无法合理分层)→ `cycle_dominated`;
//! - 空图 → `empty_graph`。
//!
//! 降级时返回原因,由调用方切换文本大纲呈现并写入 format 元数据。

use std::collections::{HashMap, HashSet};

use super::models::*;
use super::validate::{back_edge_ids, MAX_GRAPH_EDGES, MAX_GRAPH_NODES};

/// 布局几何常量(单位:SVG 用户坐标,px)。
const NODE_MIN_WIDTH: f64 = 140.0;
const NODE_HEIGHT_BASE: f64 = 40.0;
const NODE_LINE_HEIGHT: f64 = 20.0;
const NODE_GAP: f64 = 36.0;
const RANK_GAP: f64 = 84.0;
const CANVAS_MARGIN: f64 = 40.0;
const TITLE_SPACE: f64 = 64.0;
const BACK_EDGE_SIDE_OFFSET: f64 = 48.0;
/// 单行最大文本单元(CJK 记 2,其他记 1)。
const MAX_LINE_UNITS: usize = 24;
const CHAR_UNIT: f64 = 7.0;
/// 回边占比阈值:超过该比例视为无法合理分层(《M12》§4)。
const BACK_EDGE_DOMINANCE: f64 = 0.5;

/// 估算文本显示单元(CJK 全宽记 2)。
fn text_units(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            // CJK 统一表意、扩展、全角标点与假名视为全宽。
            let fullwidth = (0x4E00..=0x9FFF).contains(&u)
                || (0x3400..=0x4DBF).contains(&u)
                || (0xF900..=0xFAFF).contains(&u)
                || (0x3000..=0x303F).contains(&u)
                || (0xFF00..=0xFFEF).contains(&u)
                || (0x3040..=0x30FF).contains(&u);
            if fullwidth {
                2
            } else {
                1
            }
        })
        .sum()
}

/// 按最大单元宽度折行;中文不截断,只在整字处换行。
pub fn wrap_label(label: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut units = 0usize;
    for ch in label.chars() {
        let u = text_units(&ch.to_string());
        if units + u > MAX_LINE_UNITS && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            units = 0;
        }
        current.push(ch);
        units += u;
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// 由标签估算节点尺寸(SVG 与 React Flow 共用同一估算,保证一致)。
pub fn estimate_node_size(label: &str) -> (f64, f64) {
    let lines = wrap_label(label);
    let max_units = lines.iter().map(|l| text_units(l)).max().unwrap_or(0) as f64;
    let width = NODE_MIN_WIDTH.max(max_units * CHAR_UNIT + 24.0);
    let height = NODE_HEIGHT_BASE + lines.len() as f64 * NODE_LINE_HEIGHT;
    (width, height)
}

/// 边路由元数据(与内容边一一对应)。字段私有:外部只经
/// `edge_metas_of` 获取并回传,不自行构造。
pub struct EdgeMeta {
    id: String,
    source: String,
    target: String,
    dashed: bool,
    is_back: bool,
}

type NodeGeom = HashMap<String, (f64, f64, f64, f64)>; // id → (x, y, w, h)

/// 依据节点几何计算所有边折线:前向边正交折线,回边绕侧。
fn route_edges(geom: &NodeGeom, metas: &[EdgeMeta], direction: GraphDirection) -> Vec<LayoutEdge> {
    metas
        .iter()
        .filter_map(|m| {
            let (sx0, sy0, sw, sh) = *geom.get(&m.source)?;
            let (tx0, ty0, tw, th) = *geom.get(&m.target)?;
            let points = if m.is_back {
                match direction {
                    GraphDirection::Tb => {
                        let side_x = (sx0 + sw).max(tx0 + tw) + BACK_EDGE_SIDE_OFFSET;
                        vec![
                            GraphPoint {
                                x: sx0 + sw,
                                y: sy0 + sh / 2.0,
                            },
                            GraphPoint {
                                x: side_x,
                                y: sy0 + sh / 2.0,
                            },
                            GraphPoint {
                                x: side_x,
                                y: ty0 + th / 2.0,
                            },
                            GraphPoint {
                                x: tx0 + tw,
                                y: ty0 + th / 2.0,
                            },
                        ]
                    }
                    GraphDirection::Lr => {
                        let side_y = (sy0 + sh).max(ty0 + th) + BACK_EDGE_SIDE_OFFSET;
                        vec![
                            GraphPoint {
                                x: sx0 + sw / 2.0,
                                y: sy0 + sh,
                            },
                            GraphPoint {
                                x: sx0 + sw / 2.0,
                                y: side_y,
                            },
                            GraphPoint {
                                x: tx0 + tw / 2.0,
                                y: side_y,
                            },
                            GraphPoint {
                                x: tx0 + tw / 2.0,
                                y: ty0 + th,
                            },
                        ]
                    }
                }
            } else {
                match direction {
                    GraphDirection::Tb => {
                        let start = GraphPoint {
                            x: sx0 + sw / 2.0,
                            y: sy0 + sh,
                        };
                        let end = GraphPoint {
                            x: tx0 + tw / 2.0,
                            y: ty0,
                        };
                        if (start.x - end.x).abs() < 0.5 {
                            vec![start, end]
                        } else {
                            let mid_y = (start.y + end.y) / 2.0;
                            vec![
                                start,
                                GraphPoint {
                                    x: start.x,
                                    y: mid_y,
                                },
                                GraphPoint { x: end.x, y: mid_y },
                                end,
                            ]
                        }
                    }
                    GraphDirection::Lr => {
                        let start = GraphPoint {
                            x: sx0 + sw,
                            y: sy0 + sh / 2.0,
                        };
                        let end = GraphPoint {
                            x: tx0,
                            y: ty0 + th / 2.0,
                        };
                        if (start.y - end.y).abs() < 0.5 {
                            vec![start, end]
                        } else {
                            let mid_x = (start.x + end.x) / 2.0;
                            vec![
                                start,
                                GraphPoint {
                                    x: mid_x,
                                    y: start.y,
                                },
                                GraphPoint { x: mid_x, y: end.y },
                                end,
                            ]
                        }
                    }
                }
            };
            Some(LayoutEdge {
                id: m.id.clone(),
                source: m.source.clone(),
                target: m.target.clone(),
                points,
                dashed: m.dashed,
                back_edge: m.is_back,
            })
        })
        .collect()
}

/// 计算布局;超限/环占优/空图返回降级原因(《M12》§4)。
pub fn compute_layout(content: &GraphContent) -> GraphLayoutOutcome {
    if content.nodes.len() > MAX_GRAPH_NODES || content.edges.len() > MAX_GRAPH_EDGES {
        return GraphLayoutOutcome {
            layout: None,
            fallback_reason: Some("budget_exceeded".to_string()),
        };
    }
    if content.nodes.is_empty() {
        return GraphLayoutOutcome {
            layout: None,
            fallback_reason: Some("empty_graph".to_string()),
        };
    }
    let back = back_edge_ids(content);
    if !content.edges.is_empty()
        && back.len() as f64 > content.edges.len() as f64 * BACK_EDGE_DOMINANCE
    {
        return GraphLayoutOutcome {
            layout: None,
            fallback_reason: Some("cycle_dominated".to_string()),
        };
    }

    let n = content.nodes.len();
    let index_of: HashMap<&str, usize> = content
        .nodes
        .iter()
        .enumerate()
        .map(|(i, v)| (v.id.as_str(), i))
        .collect();

    // DAG 骨架邻接(回边、自环、悬空端点排除)。
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    let mut edge_metas: Vec<EdgeMeta> = Vec::with_capacity(content.edges.len());
    for e in &content.edges {
        let is_back = back.contains(&e.id);
        edge_metas.push(EdgeMeta {
            id: e.id.clone(),
            source: e.source.clone(),
            target: e.target.clone(),
            dashed: e.certainty != GraphCertainty::Stated,
            is_back,
        });
        if e.source == e.target || is_back {
            continue;
        }
        if let (Some(&s), Some(&t)) = (
            index_of.get(e.source.as_str()),
            index_of.get(e.target.as_str()),
        ) {
            adj[s].push(t);
            indeg[t] += 1;
        }
    }

    // 分层:拓扑序上最长路径。
    let mut rank = vec![0usize; n];
    let mut done = vec![false; n];
    let mut topo = Vec::with_capacity(n);
    loop {
        let picked = done
            .iter()
            .zip(indeg.iter())
            .position(|(d, deg)| !*d && *deg == 0);
        let Some(i) = picked else { break };
        done[i] = true;
        topo.push(i);
        for &j in &adj[i] {
            indeg[j] -= 1;
        }
    }
    for &i in &topo {
        for &j in &adj[i] {
            rank[j] = rank[j].max(rank[i] + 1);
        }
    }
    // 纯环节点(topo 未覆盖)保持 rank 0,由回边表达环关系。
    let max_rank = rank.iter().copied().max().unwrap_or(0);

    // 层内排序:输入顺序初始化 + 两轮重心扫描(前向/后向邻接均值,稳定并列)。
    let mut ranks: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for i in 0..n {
        ranks[rank[i]].push(i);
    }
    let skeleton_edges: Vec<(usize, usize)> = adj
        .iter()
        .enumerate()
        .flat_map(|(s, ts)| ts.iter().map(move |&t| (s, t)))
        .collect();
    for _ in 0..2 {
        for r in 1..ranks.len() {
            let prev_positions: HashMap<usize, f64> = ranks[r - 1]
                .iter()
                .enumerate()
                .map(|(pos, &node)| (node, pos as f64))
                .collect();
            let mut keyed: Vec<(f64, usize)> = ranks[r]
                .iter()
                .enumerate()
                .map(|(cur, &node)| {
                    let ups: Vec<f64> = skeleton_edges
                        .iter()
                        .filter(|(_, t)| *t == node)
                        .filter_map(|(s, _)| prev_positions.get(s).copied())
                        .collect();
                    let bary = if ups.is_empty() {
                        cur as f64
                    } else {
                        ups.iter().sum::<f64>() / ups.len() as f64
                    };
                    (bary, node)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)));
            ranks[r] = keyed.into_iter().map(|(_, node)| node).collect();
        }
        for r in (0..ranks.len().saturating_sub(1)).rev() {
            let next_positions: HashMap<usize, f64> = ranks[r + 1]
                .iter()
                .enumerate()
                .map(|(pos, &node)| (node, pos as f64))
                .collect();
            let mut keyed: Vec<(f64, usize)> = ranks[r]
                .iter()
                .enumerate()
                .map(|(cur, &node)| {
                    let downs: Vec<f64> = skeleton_edges
                        .iter()
                        .filter(|(s, _)| *s == node)
                        .filter_map(|(_, t)| next_positions.get(t).copied())
                        .collect();
                    let bary = if downs.is_empty() {
                        cur as f64
                    } else {
                        downs.iter().sum::<f64>() / downs.len() as f64
                    };
                    (bary, node)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)));
            ranks[r] = keyed.into_iter().map(|(_, node)| node).collect();
        }
    }

    // 坐标分配:层内按序累加宽度并整体居中;层间距固定;TB 纵向、LR 横向。
    let direction = content.layout.direction;
    let sizes: Vec<(f64, f64)> = content
        .nodes
        .iter()
        .map(|node| estimate_node_size(&node.label))
        .collect();
    // cross[i]:节点 i 的层内主坐标(先按层内顺序累加)。
    let mut cross = vec![0.0f64; n];
    let mut layer_extent = vec![0.0f64; ranks.len()];
    for (r, layer) in ranks.iter().enumerate() {
        let mut cursor = CANVAS_MARGIN;
        for &node in layer {
            cross[node] = cursor;
            cursor += sizes[node].0 + NODE_GAP;
        }
        layer_extent[r] = cursor - NODE_GAP + CANVAS_MARGIN;
    }
    let max_extent = layer_extent.iter().cloned().fold(0.0_f64, f64::max);
    for (r, layer) in ranks.iter().enumerate() {
        let shift = (max_extent - layer_extent[r]) / 2.0;
        for &node in layer {
            cross[node] += shift;
        }
    }

    let mut geom: NodeGeom = HashMap::with_capacity(n);
    let mut layout_nodes: Vec<LayoutNode> = Vec::with_capacity(n);
    for (i, node) in content.nodes.iter().enumerate() {
        let (w, h) = sizes[i];
        let rank_axis = CANVAS_MARGIN + TITLE_SPACE + rank[i] as f64 * RANK_GAP;
        let (x, y) = match direction {
            GraphDirection::Tb => (cross[i], rank_axis),
            GraphDirection::Lr => (rank_axis, cross[i]),
        };
        geom.insert(node.id.clone(), (x, y, w, h));
        layout_nodes.push(LayoutNode {
            id: node.id.clone(),
            x,
            y,
            width: w,
            height: h,
        });
    }

    let layout_edges = route_edges(&geom, &edge_metas, direction);

    // 画布范围(含标题区与边距)。
    let mut width = 0.0_f64;
    let mut height = 0.0_f64;
    for ln in &layout_nodes {
        width = width.max(ln.x + ln.width);
        height = height.max(ln.y + ln.height);
    }
    let (w, h) = match direction {
        GraphDirection::Tb => (width.max(max_extent), height),
        GraphDirection::Lr => (width, height.max(max_extent)),
    };

    let mut result = LayoutResult {
        algorithm: "layered".to_string(),
        direction,
        nodes: layout_nodes,
        edges: layout_edges,
        back_edge_ids: {
            let mut ids: Vec<String> = back.into_iter().collect();
            ids.sort();
            ids
        },
        width: (w + CANVAS_MARGIN).max(320.0),
        height: (h + CANVAS_MARGIN).max(240.0),
    };
    // 用户手工位置覆盖(语义不变,《M12》§4)。
    apply_user_positions(&mut result, &content.layout.user_positions, &edge_metas);
    GraphLayoutOutcome {
        layout: Some(result),
        fallback_reason: None,
    }
}

/// 用持久化的用户位置覆盖自动坐标,并按新几何重算边路由(语义不变,《M12》§4)。
pub fn apply_user_positions(
    result: &mut LayoutResult,
    user_positions: &HashMap<String, GraphPoint>,
    edge_metas: &[EdgeMeta],
) {
    if user_positions.is_empty() {
        return;
    }
    for ln in &mut result.nodes {
        if let Some(p) = user_positions.get(&ln.id) {
            ln.x = p.x;
            ln.y = p.y;
        }
    }
    let geom: NodeGeom = result
        .nodes
        .iter()
        .map(|ln| (ln.id.clone(), (ln.x, ln.y, ln.width, ln.height)))
        .collect();
    result.edges = route_edges(&geom, edge_metas, result.direction);
    // 画布范围跟随外移节点,保证导出不裁切。
    let mut width = 0.0_f64;
    let mut height = 0.0_f64;
    for ln in &result.nodes {
        width = width.max(ln.x + ln.width);
        height = height.max(ln.y + ln.height);
    }
    if width + CANVAS_MARGIN > result.width {
        result.width = width + CANVAS_MARGIN;
    }
    if height + CANVAS_MARGIN > result.height {
        result.height = height + CANVAS_MARGIN;
    }
}

/// 由内容边重建路由元数据(apply_user_positions 的公开入口用)。
pub fn edge_metas_of(content: &GraphContent) -> Vec<EdgeMeta> {
    let back: HashSet<String> = back_edge_ids(content);
    content
        .edges
        .iter()
        .map(|e| EdgeMeta {
            id: e.id.clone(),
            source: e.source.clone(),
            target: e.target.clone(),
            dashed: e.certainty != GraphCertainty::Stated,
            is_back: back.contains(&e.id),
        })
        .collect()
}
