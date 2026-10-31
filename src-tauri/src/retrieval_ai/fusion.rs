//! M09 RRF 融合与候选裁决(《M09》§5)。
//!
//! score(block) = sum(weight_i / (rrfK + rank_i(block)))
//! 融合后按分数排序、同块去重;未入选候选必须带 exclusion_reason。

/// 版本化默认检索配置(《M09》§5)。实际数值在 M0 评估后冻结;
/// 任何偏离默认值的结果必须随 RetrievalManifest 记录。
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievalConfig {
    pub search_config_version: u32,
    pub fts_candidate_limit: usize,
    pub vector_candidate_limit: usize,
    pub fused_candidate_limit: usize,
    pub rrf_k: u32,
    /// 数字完全匹配显式加权(默认 1,即不加权)。
    pub exact_match_weight: f64,
    /// 标题命中显式加权(默认 1)。
    pub heading_hit_weight: f64,
}

/// 冻结的 V1 默认值:FTS 64 / 向量 64 / 融合后 24 / RRF k=60。
pub const DEFAULT_CONFIG: RetrievalConfig = RetrievalConfig {
    search_config_version: 1,
    fts_candidate_limit: 64,
    vector_candidate_limit: 64,
    fused_candidate_limit: 24,
    rrf_k: 60,
    exact_match_weight: 1.0,
    heading_hit_weight: 1.0,
};

/// 单路召回输入:块 ID + 该路名次(1-based;rank 越小越相关)。
#[derive(Debug, Clone)]
pub struct RankedList {
    pub source: &'static str,
    pub entries: Vec<(String, usize)>,
    /// 该路的显式权重(FTS/向量默认 1)。
    pub weight: f64,
}

/// 融合后的候选。
#[derive(Debug, Clone)]
pub struct FusedCandidate {
    pub block_id: String,
    pub rrf_score: f64,
    pub fts_rank: Option<usize>,
    pub vector_rank: Option<usize>,
}

/// 排除原因(写入 retrieval_candidate.exclusion_reason)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExclusionReason {
    /// 融合分数未进入前 N。
    BelowFusedLimit,
    /// 超出 Prompt 块预算,按 Manifest 记录的顺序取舍。
    PromptBudgetExceeded,
    /// 块不在快照授权范围(邻块扩展越界防御)。
    OutOfSnapshot,
}

impl ExclusionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BelowFusedLimit => "below_fused_limit",
            Self::PromptBudgetExceeded => "prompt_budget_exceeded",
            Self::OutOfSnapshot => "out_of_snapshot",
        }
    }
}

/// Reciprocal Rank Fusion:多路排名 → 单一分数。
/// 未出现在某路的块不贡献该路分数;rank 从 1 计。
pub fn rrf_fuse(lists: &[RankedList], config: &RetrievalConfig) -> Vec<FusedCandidate> {
    use std::collections::HashMap;
    let k = config.rrf_k as f64;
    let mut acc: HashMap<String, (f64, Option<usize>, Option<usize>)> = HashMap::new();
    for list in lists {
        for (block_id, rank) in &list.entries {
            let e = acc.entry(block_id.clone()).or_insert((0.0, None, None));
            e.0 += list.weight / (k + (*rank as f64));
            let slot = match list.source {
                "fts" => &mut e.1,
                _ => &mut e.2,
            };
            match *slot {
                None => *slot = Some(*rank),
                Some(r) if *rank < r => *slot = Some(*rank),
                _ => {}
            }
        }
    }
    let mut out: Vec<FusedCandidate> = acc
        .into_iter()
        .map(
            |(block_id, (score, fts_rank, vector_rank))| FusedCandidate {
                block_id,
                rrf_score: score,
                fts_rank,
                vector_rank,
            },
        )
        .collect();
    // 分数排序;并列按块 ID 稳定排序保证可复现(M09-A9)。
    out.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.block_id.cmp(&b.block_id))
    });
    out
}

/// 融合候选裁决:截断到 fused_candidate_limit,并在快照白名单
/// 校验后选出实际进入 Prompt 的集合。每个候选都得到
/// `included` 或明确 `exclusion_reason`(禁止无记录截断)。
#[derive(Debug, Clone)]
pub struct CandidateDecision {
    pub candidate: FusedCandidate,
    pub included: bool,
    pub exclusion_reason: Option<ExclusionReason>,
}

pub fn decide_candidates(
    fused: Vec<FusedCandidate>,
    snapshot_block_ids: &dyn Fn(&str) -> bool,
    prompt_block_budget: usize,
    config: &RetrievalConfig,
) -> Vec<CandidateDecision> {
    let mut decisions: Vec<CandidateDecision> = Vec::with_capacity(fused.len());
    let mut included_count = 0usize;
    for (idx, candidate) in fused.into_iter().enumerate() {
        if idx >= config.fused_candidate_limit {
            decisions.push(CandidateDecision {
                candidate,
                included: false,
                exclusion_reason: Some(ExclusionReason::BelowFusedLimit),
            });
            continue;
        }
        if !snapshot_block_ids(&candidate.block_id) {
            decisions.push(CandidateDecision {
                candidate,
                included: false,
                exclusion_reason: Some(ExclusionReason::OutOfSnapshot),
            });
            continue;
        }
        if included_count < prompt_block_budget {
            included_count += 1;
            decisions.push(CandidateDecision {
                candidate,
                included: true,
                exclusion_reason: None,
            });
        } else {
            decisions.push(CandidateDecision {
                candidate,
                included: false,
                exclusion_reason: Some(ExclusionReason::PromptBudgetExceeded),
            });
        }
    }
    decisions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(source: &'static str, entries: Vec<(&str, usize)>) -> RankedList {
        RankedList {
            source,
            entries: entries
                .into_iter()
                .map(|(id, r)| (id.to_string(), r))
                .collect(),
            weight: 1.0,
        }
    }

    #[test]
    fn rrf_combined_rank_beats_single_list_rank1() {
        // b 在两路都第 2,应超过只在 FTS 第 1 的 a。
        let fused = rrf_fuse(
            &[
                list("fts", vec![("a", 1), ("b", 2)]),
                list("vec", vec![("c", 1), ("b", 2)]),
            ],
            &DEFAULT_CONFIG,
        );
        assert_eq!(fused[0].block_id, "b");
        let b = &fused[0];
        assert_eq!(b.fts_rank, Some(2));
        assert_eq!(b.vector_rank, Some(2));
        let expected = 1.0 / (60.0 + 2.0) * 2.0;
        assert!((b.rrf_score - expected).abs() < 1e-12);
    }

    #[test]
    fn same_block_appears_once() {
        let fused = rrf_fuse(
            &[list("fts", vec![("a", 1)]), list("vec", vec![("a", 1)])],
            &DEFAULT_CONFIG,
        );
        assert_eq!(fused.len(), 1);
    }

    #[test]
    fn tie_breaks_stably_by_block_id() {
        let fused = rrf_fuse(&[list("fts", vec![("x", 1), ("y", 1)])], &DEFAULT_CONFIG);
        let ids: Vec<&str> = fused.iter().map(|c| c.block_id.as_str()).collect();
        assert_eq!(ids, vec!["x", "y"]);
    }

    #[test]
    fn every_candidate_has_inclusion_or_reason() {
        let fused: Vec<FusedCandidate> = (0..30)
            .map(|i| FusedCandidate {
                block_id: format!("b{i}"),
                rrf_score: 1.0 / (i + 2) as f64,
                fts_rank: Some(i + 1),
                vector_rank: None,
            })
            .collect();
        let decisions = decide_candidates(fused, &|_| true, 10, &DEFAULT_CONFIG);
        assert_eq!(decisions.len(), 30);
        assert_eq!(decisions.iter().filter(|d| d.included).count(), 10);
        // 融合上限 24:第 25 名起 below_fused_limit。
        assert_eq!(
            decisions[24].exclusion_reason,
            Some(ExclusionReason::BelowFusedLimit)
        );
        // 上限内但超出 Prompt 预算:prompt_budget_exceeded。
        assert_eq!(
            decisions[10].exclusion_reason,
            Some(ExclusionReason::PromptBudgetExceeded)
        );
        assert!(decisions
            .iter()
            .all(|d| d.included == d.exclusion_reason.is_none()));
    }

    #[test]
    fn out_of_snapshot_candidates_are_excluded_not_dropped() {
        let fused = vec![FusedCandidate {
            block_id: "a".into(),
            rrf_score: 1.0,
            fts_rank: Some(1),
            vector_rank: None,
        }];
        let decisions = decide_candidates(fused, &|id| id != "a", 5, &DEFAULT_CONFIG);
        assert_eq!(
            decisions[0].exclusion_reason,
            Some(ExclusionReason::OutOfSnapshot)
        );
    }
}
