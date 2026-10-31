//! M12 结构校验与规范化(《M12》§3)。
//!
//! 校验分两档:
//! - **错误**(拒绝保存):ID 不唯一、端点缺失、自环、未标记环、标签超长、
//!   超限(拒绝或提示拆图,不静默删除)、零节点。
//! - **规范化**(结构有效时产出):`supported` 无引用 ⇒ 降级 `unverified`
//!   (删除证据后不得保留"已支持"状态,《M12》§8);大纲按语义顺序重生成。
//! - **警告**(可见但不拒绝):孤立/不可达节点。
//!
//! "无未标记环"的实现口径:DFS 回边(闭合有向环的边)必须带显式标记——
//! 非空 label、`certainty != stated` 或 `contradicts` 类型;未标记的回边
//! 拒绝保存(《M12》§3)。布局阶段的环处理见 layout.rs。

use std::collections::{HashMap, HashSet};

use super::models::*;

/// 上限常量(《M12》§3 表)。
pub const MAX_GRAPH_NODES: usize = 60;
pub const MAX_GRAPH_EDGES: usize = 100;
pub const MAX_LABEL_CHARS: usize = 200;

/// 语义节点顺序:去回边后的 DAG 拓扑序,同层按输入顺序稳定排列;
/// 纯环剩余节点按输入顺序追加。大纲与证据编号共用(《M12》§3/§6)。
pub fn semantic_node_order(content: &GraphContent) -> Vec<usize> {
    let n = content.nodes.len();
    let index_of: HashMap<&str, usize> = content
        .nodes
        .iter()
        .enumerate()
        .map(|(i, v)| (v.id.as_str(), i))
        .collect();
    let back = back_edge_ids(content);
    // DAG 骨架邻接表(回边与自环不参与序)。
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for e in &content.edges {
        if e.source == e.target || back.contains(&e.id) {
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
    // Kahn 拓扑:每轮取输入顺序最小的零入度节点(n ≤ 60,O(n²) 足够)。
    let mut order = Vec::with_capacity(n);
    let mut done = vec![false; n];
    loop {
        let picked = done
            .iter()
            .zip(indeg.iter())
            .position(|(d, deg)| !*d && *deg == 0);
        let Some(i) = picked else { break };
        done[i] = true;
        order.push(i);
        for &j in &adj[i] {
            if !done[j] {
                indeg[j] -= 1;
            }
        }
    }
    for (i, finished) in done.iter().enumerate() {
        if !finished {
            order.push(i);
        }
    }
    order
}

/// DFS 回边检测:返回闭合有向环的边 ID 集合(自环由校验单独拒绝)。
pub fn back_edge_ids(content: &GraphContent) -> HashSet<String> {
    let n = content.nodes.len();
    let index_of: HashMap<&str, usize> = content
        .nodes
        .iter()
        .enumerate()
        .map(|(i, v)| (v.id.as_str(), i))
        .collect();
    let mut adj: Vec<Vec<(usize, &str)>> = vec![Vec::new(); n];
    for e in &content.edges {
        if e.source == e.target {
            continue; // 自环另行报错
        }
        if let (Some(&s), Some(&t)) = (
            index_of.get(e.source.as_str()),
            index_of.get(e.target.as_str()),
        ) {
            adj[s].push((t, e.id.as_str()));
        }
    }
    let mut color = vec![0u8; n]; // 0 未访问 1 在栈 2 完成
    let mut back: HashSet<String> = HashSet::new();
    for start in 0..n {
        if color[start] != 0 {
            continue;
        }
        // 迭代 DFS:栈帧 (节点, 邻接游标)。
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = 1;
        while let Some(&mut (node, ref mut cursor)) = stack.last_mut() {
            if *cursor >= adj[node].len() {
                color[node] = 2;
                stack.pop();
                continue;
            }
            let (next, edge_id) = adj[node][*cursor];
            *cursor += 1;
            match color[next] {
                1 => {
                    back.insert(edge_id.to_string());
                }
                0 => {
                    color[next] = 1;
                    stack.push((next, 0));
                }
                _ => {}
            }
        }
    }
    back
}

/// 回边是否"已标记":非空 label / certainty != stated / contradicts。
fn cycle_edge_marked(edge: &GraphEdge) -> bool {
    edge.certainty != GraphCertainty::Stated
        || edge.edge_kind_marks_cycle()
        || edge.label.as_deref().is_some_and(|l| !l.trim().is_empty())
}

impl GraphEdge {
    /// contradicts 边本身即是显式关系标记(论证冲突环)。
    pub fn edge_kind_marks_cycle(&self) -> bool {
        self.edge_type == GraphEdgeKind::Contradicts
    }
}

fn issue(code: &str, pointer: &str, message: impl Into<String>) -> GraphIssue {
    GraphIssue {
        code: code.to_string(),
        pointer: pointer.to_string(),
        message: message.into(),
    }
}

/// graph.v1 Artifact Codec:kind/schema/结构校验的模块内实现,
/// 由 M13 在组装根经 `impl crate::artifacts::models::ArtifactCodec`
/// 逐字段转发注册(与 M11 quiz.v1 同构);未注册 kind 的内容不能写入产物。
pub struct GraphV1Codec;

impl GraphV1Codec {
    pub fn kind(&self) -> &str {
        "graph"
    }

    pub fn schema_version(&self) -> &str {
        GRAPH_SCHEMA_VERSION
    }

    /// 写入前校验:graph.v1 结构完整(端点/自环/未标记环/上限/引用形状)。
    /// 证据档位降级由规范化承担;此处只拒绝结构性损坏。
    pub fn validate(&self, content: &serde_json::Value) -> bool {
        let Ok(parsed) = serde_json::from_value::<GraphContent>(content.clone()) else {
            return false;
        };
        validate_graph(&parsed).valid
    }
}

/// 引用项校验:ID 全图唯一、blockId/quote 非空。
/// 独立泛型函数(闭包无法跨调用点统一借用生命周期)。
fn citation_issue<'a>(
    citation_ids: &mut HashSet<&'a str>,
    owner_ptr: &str,
    cit: &'a GraphCitation,
) -> Option<GraphIssue> {
    if cit.id.trim().is_empty() {
        return Some(issue(
            "citation_id_empty",
            &format!("{owner_ptr}/id"),
            "引用 ID 不能为空",
        ));
    }
    if !citation_ids.insert(cit.id.as_str()) {
        return Some(issue(
            "citation_id_duplicate",
            &format!("{owner_ptr}/id"),
            format!("引用 ID 重复:{}", cit.id),
        ));
    }
    if cit.block_id.trim().is_empty() {
        return Some(issue(
            "citation_block_missing",
            &format!("{owner_ptr}/blockId"),
            "引用必须指向存在的原文块",
        ));
    }
    if cit.quoted_text.trim().is_empty() {
        return Some(issue(
            "citation_quote_empty",
            &format!("{owner_ptr}/quotedText"),
            "引用原文不能为空",
        ));
    }
    None
}

/// 校验 + 规范化入口(纯函数,无数据库)。
pub fn validate_graph(content: &GraphContent) -> GraphValidationReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if content.schema_version != GRAPH_SCHEMA_VERSION {
        errors.push(issue(
            "schema_version_unsupported",
            "/schemaVersion",
            format!("schemaVersion 必须为 {GRAPH_SCHEMA_VERSION}"),
        ));
    }
    if content.title.trim().is_empty() {
        errors.push(issue("title_empty", "/title", "图标题不能为空"));
    }
    if content.nodes.is_empty() {
        errors.push(issue("nodes_empty", "/nodes", "图至少需要一个节点"));
    }
    if content.nodes.len() > MAX_GRAPH_NODES {
        errors.push(issue(
            "node_limit_exceeded",
            "/nodes",
            format!(
                "节点数 {} 超过上限 {},请缩小范围或拆分为多个图",
                content.nodes.len(),
                MAX_GRAPH_NODES
            ),
        ));
    }
    if content.edges.len() > MAX_GRAPH_EDGES {
        errors.push(issue(
            "edge_limit_exceeded",
            "/edges",
            format!(
                "边数 {} 超过上限 {},请缩小范围或拆分为多个图",
                content.edges.len(),
                MAX_GRAPH_EDGES
            ),
        ));
    }

    // 节点:ID 唯一、label 非空且长度受限。
    let mut node_ids: HashSet<&str> = HashSet::new();
    for (i, node) in content.nodes.iter().enumerate() {
        if node.id.trim().is_empty() {
            errors.push(issue(
                "node_id_empty",
                &format!("/nodes/{i}/id"),
                "节点 ID 不能为空",
            ));
        } else if !node_ids.insert(node.id.as_str()) {
            errors.push(issue(
                "node_id_duplicate",
                &format!("/nodes/{i}/id"),
                format!("节点 ID 重复:{}", node.id),
            ));
        }
        if node.label.trim().is_empty() {
            errors.push(issue(
                "node_label_empty",
                &format!("/nodes/{i}/label"),
                "节点标签不能为空",
            ));
        }
        if node.label.chars().count() > MAX_LABEL_CHARS {
            errors.push(issue(
                "label_too_long",
                &format!("/nodes/{i}/label"),
                format!("节点标签超过 {MAX_LABEL_CHARS} 字符"),
            ));
        }
    }

    // 边:ID 唯一、端点存在、无自环。
    let mut edge_ids: HashSet<&str> = HashSet::new();
    for (i, edge) in content.edges.iter().enumerate() {
        if edge.id.trim().is_empty() {
            errors.push(issue(
                "edge_id_empty",
                &format!("/edges/{i}/id"),
                "边 ID 不能为空",
            ));
        } else if !edge_ids.insert(edge.id.as_str()) {
            errors.push(issue(
                "edge_id_duplicate",
                &format!("/edges/{i}/id"),
                format!("边 ID 重复:{}", edge.id),
            ));
        }
        if !node_ids.contains(edge.source.as_str()) {
            errors.push(issue(
                "edge_endpoint_missing",
                &format!("/edges/{i}/source"),
                format!("边 {} 的 source 节点不存在", edge.id),
            ));
        }
        if !node_ids.contains(edge.target.as_str()) {
            errors.push(issue(
                "edge_endpoint_missing",
                &format!("/edges/{i}/target"),
                format!("边 {} 的 target 节点不存在", edge.id),
            ));
        }
        if edge.source == edge.target && !edge.source.is_empty() {
            errors.push(issue(
                "self_loop",
                &format!("/edges/{i}"),
                format!("边 {} 为自环,不允许", edge.id),
            ));
        }
        if let Some(label) = &edge.label {
            if label.chars().count() > MAX_LABEL_CHARS {
                errors.push(issue(
                    "label_too_long",
                    &format!("/edges/{i}/label"),
                    format!("边标签超过 {MAX_LABEL_CHARS} 字符"),
                ));
            }
        }
    }

    // 引用:ID 全图唯一、blockId/quote 非空。
    let mut citation_ids: HashSet<&str> = HashSet::new();
    for (i, node) in content.nodes.iter().enumerate() {
        for (j, cit) in node.citations.iter().enumerate() {
            if let Some(e) =
                citation_issue(&mut citation_ids, &format!("/nodes/{i}/citations/{j}"), cit)
            {
                errors.push(e);
            }
        }
    }
    for (i, edge) in content.edges.iter().enumerate() {
        for (j, cit) in edge.citations.iter().enumerate() {
            if let Some(e) =
                citation_issue(&mut citation_ids, &format!("/edges/{i}/citations/{j}"), cit)
            {
                errors.push(e);
            }
        }
    }

    // 未标记环:回边必须有显式标记(§3,口径见模块注释)。
    let back = back_edge_ids(content);
    for edge in &content.edges {
        if back.contains(&edge.id) && !cycle_edge_marked(edge) {
            errors.push(issue(
                "unmarked_cycle",
                &format!("/edges/{}", edge.id),
                format!(
                    "边 {} 闭合环但未标记:环上边需要非空标签、certainty=inferred/uncertain 或 contradicts 类型",
                    edge.id
                ),
            ));
        }
    }

    // 不可达节点:警告(进入大纲,不拒绝)。
    let mut touched: HashSet<&str> = HashSet::new();
    for e in &content.edges {
        touched.insert(e.source.as_str());
        touched.insert(e.target.as_str());
    }
    for (i, node) in content.nodes.iter().enumerate() {
        if !touched.contains(node.id.as_str()) {
            warnings.push(issue(
                "unreachable_node",
                &format!("/nodes/{i}"),
                format!("节点 {} 未与任何边相连,将仅出现在大纲中", node.id),
            ));
        }
    }

    if !errors.is_empty() {
        return GraphValidationReport {
            valid: false,
            errors,
            warnings,
            normalized: None,
        };
    }

    // 规范化:证据降级 + 大纲重生成(结构已有效)。
    let mut normalized = content.clone();
    let mut downgraded: Vec<String> = Vec::new();
    for node in &mut normalized.nodes {
        if node.evidence_status == GraphEvidenceStatus::Supported && node.citations.is_empty() {
            node.evidence_status = GraphEvidenceStatus::Unverified;
            downgraded.push(node.id.clone());
        }
    }
    for edge in &mut normalized.edges {
        if edge.evidence_status == GraphEvidenceStatus::Supported && edge.citations.is_empty() {
            edge.evidence_status = GraphEvidenceStatus::Unverified;
            downgraded.push(edge.id.clone());
        }
    }
    if !downgraded.is_empty() {
        warnings.push(issue(
            "evidence_downgraded",
            "/nodes",
            format!(
                "以下节点/边声明 supported 但无引用,已降级为未核验:{}",
                downgraded.join(", ")
            ),
        ));
    }
    normalized.outline = semantic_node_order(&normalized)
        .iter()
        .map(|&i| normalized.nodes[i].label.clone())
        .collect();

    GraphValidationReport {
        valid: true,
        errors,
        warnings,
        normalized: Some(normalized),
    }
}
