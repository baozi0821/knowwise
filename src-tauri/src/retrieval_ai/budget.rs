//! M09 分层上下文 token 预算(WBS-5)。
//!
//! 目标不是"塞满更大的窗口",而是把有限预算稳定地分给三层:
//! 历史摘要、检索来源、最近原文。估算刻意保守:
//! - 非 ASCII 字符(中文等)按 1 字符 ≈ 1 token;
//! - ASCII 按 4 字符 ≈ 1 token。
//!
//! 《M09》§6 的固定块数预算仍保留为硬上限;本模块负责在这个上限内
//! 按 token 预算取舍。任何裁剪都必须产生可解释的说明(不静默丢弃):
//! 保留优先级为 历史摘要 > 检索块 > 最近原文,超预算时先丢最早原文、
//! 再压缩摘要,绝不在没有记录的情况下改变输入。

use super::models::ChatTurn;

/// 默认上下文窗口估算。M0 评估前没有按模型区分的窗口配置,
/// 这里取保守值;预算本身再留 20% 余量,避免不同提供方窗口差异导致超窗。
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 16_384;
/// 窗口预算余量(百分比):不参与 Prompt 估算,留给供应商模板与工具消息。
pub const BUDGET_HEADROOM_PERCENT: usize = 20;
/// 历史进入 Prompt 的默认 token 上限(单层硬上限)。
pub const DEFAULT_MAX_HISTORY_TOKENS: usize = 3_000;
/// 单轮历史文本上限:超长回答只保留开头部分,并向模型显式标注截断。
pub const MAX_HISTORY_TURN_CHARS: usize = 2_000;

/// 分层预算。`output` 为生成预留,不用于 Prompt 装填。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBudget {
    pub system: usize,
    pub history: usize,
    pub retrieval: usize,
    pub output: usize,
}

impl TokenBudget {
    /// 按模型上下文窗口比例分配(扣除 20% 余量):
    /// system ≈ 1/12、history ≈ 28%、output ≈ 20%,其余给检索。
    pub fn for_context_window(window_tokens: usize) -> Self {
        let usable = window_tokens.saturating_mul(100 - BUDGET_HEADROOM_PERCENT) / 100;
        let history = usable.saturating_mul(28) / 100;
        let output = usable / 5;
        // system 保留最低 512,但绝不挤占 history/output 之外的恒定份额。
        let system = (usable / 12)
            .max(512)
            .min(usable.saturating_sub(history + output));
        let retrieval = usable.saturating_sub(system + history + output);
        Self {
            system,
            history,
            retrieval,
            output,
        }
    }

    /// 冻结的默认预算(未配置具体模型窗口时使用)。
    pub fn default_budget() -> Self {
        Self::for_context_window(DEFAULT_CONTEXT_WINDOW_TOKENS)
    }

    /// 实际可用于 Prompt 的 token 上限(不含输出预留)。
    pub fn prompt_limit(&self) -> usize {
        self.system + self.history + self.retrieval
    }

    pub fn total(&self) -> usize {
        self.prompt_limit() + self.output
    }
}

/// 保守 token 估算:中文/其他非 ASCII 字符按 1 token,ASCII 按 4 字符 1 token。
/// 空串为 0;结果对同一输入稳定,不依赖分词器版本。
pub fn estimate_tokens(text: &str) -> usize {
    let mut non_ascii = 0usize;
    let mut ascii = 0usize;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
    }
    non_ascii + ascii.div_ceil(4)
}

/// 截断到 token 上限:到达边界时追加省略号并返回 `true`。
/// 只在摘要等"可再生成"的低层使用;当前问题与检索块的裁剪另行记录。
pub fn truncate_to_token_budget(text: &str, max_tokens: usize) -> (String, bool) {
    if estimate_tokens(text) <= max_tokens {
        return (text.to_string(), false);
    }
    if max_tokens == 0 {
        return (String::new(), !text.is_empty());
    }
    // 给省略号预留 1 token,保证返回值(含省略号)不超过上限。
    let content_limit = max_tokens.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    let mut ascii_run = 0usize;
    for ch in text.chars() {
        let cost = if ch.is_ascii() {
            let cost = usize::from(ascii_run.is_multiple_of(4));
            ascii_run += 1;
            cost
        } else {
            ascii_run = 0;
            1
        };
        if used + cost > content_limit {
            out.push('…');
            return (out, true);
        }
        used += cost;
        out.push(ch);
    }
    (out, false)
}

/// 最近原文装填结果。
#[derive(Debug, Clone)]
pub struct HistoryFit {
    /// 按时间升序保留的原文轮次。
    pub turns: Vec<ChatTurn>,
    pub tokens: usize,
    pub dropped_turns: usize,
    pub truncated_turns: usize,
    pub notes: Vec<String>,
}

impl HistoryFit {
    fn finish(mut self) -> Self {
        if self.dropped_turns > 0 {
            self.notes
                .push(format!("省略了较早的 {} 轮对话", self.dropped_turns));
        }
        if self.truncated_turns > 0 {
            self.notes
                .push(format!("{} 轮过长对话已截断", self.truncated_turns));
        }
        self
    }
}

/// 只装填最近原文:从最近一轮向前装入,预算不足时丢弃更早轮次。
pub fn fit_recent_turns(
    turns: &[ChatTurn],
    token_budget: usize,
    max_turn_chars: usize,
) -> HistoryFit {
    let mut kept: Vec<ChatTurn> = Vec::new();
    let mut used = 0usize;
    let mut dropped_turns = 0usize;
    let mut truncated_turns = 0usize;
    for turn in turns.iter().rev() {
        let mut text = turn.text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        if text.chars().count() > max_turn_chars {
            text = text.chars().take(max_turn_chars).collect();
            text.push_str("\n…(历史回答已截断)");
            truncated_turns += 1;
        }
        let cost = estimate_tokens(&text);
        if cost > token_budget.saturating_sub(used) {
            dropped_turns += 1;
            continue;
        }
        used += cost;
        kept.push(ChatTurn {
            role: turn.role,
            text,
        });
    }
    kept.reverse();
    HistoryFit {
        turns: kept,
        tokens: used,
        dropped_turns,
        truncated_turns,
        notes: Vec::new(),
    }
    .finish()
}

/// 分层历史装填结果(摘要 + 最近原文 + 当前问题)。
#[derive(Debug, Clone)]
pub struct LayeredHistory {
    pub summary: Option<String>,
    pub summary_tokens: usize,
    pub turns: Vec<ChatTurn>,
    /// 摘要 + 最近原文 + 当前问题(不含检索块与系统指令)。
    pub tokens: usize,
    /// 可能因预算被截断的当前问题(默认原样)。
    pub question: String,
    pub question_truncated: bool,
    pub dropped_turns: usize,
    pub truncated_turns: usize,
    pub summary_truncated: bool,
    pub notes: Vec<String>,
}

impl LayeredHistory {
    /// 仅面向"最近原文"的裁剪说明(与既有历史字段语义一致)。
    pub fn history_note(&self) -> Option<String> {
        if self.dropped_turns == 0 && self.truncated_turns == 0 {
            return None;
        }
        let mut parts = Vec::new();
        if self.dropped_turns > 0 {
            parts.push(format!("省略了较早的 {} 轮对话", self.dropped_turns));
        }
        if self.truncated_turns > 0 {
            parts.push(format!("{} 轮过长对话已截断", self.truncated_turns));
        }
        Some(parts.join(","))
    }
}

/// 把摘要、最近原文与当前问题装入历史预算。
///
/// 顺序与保留优先级一致:当前问题必须保留(极端情况下截断并记录),
/// 其次是历史摘要,最后才是最近原文。因此预算不足时先丢最早原文,
/// 只有原文全部丢完仍不足才压缩摘要。
pub fn fit_history_and_question(
    summary: Option<&str>,
    turns: &[ChatTurn],
    question: &str,
    token_budget: usize,
) -> LayeredHistory {
    let mut notes = Vec::new();

    let (question_text, question_truncated) = {
        let cost = estimate_tokens(question);
        if cost > token_budget {
            let (text, truncated) = truncate_to_token_budget(question, token_budget);
            if truncated {
                notes.push("当前问题过长,已按上下文预算截断".to_string());
            }
            (text, truncated)
        } else {
            (question.to_string(), false)
        }
    };
    let question_tokens = estimate_tokens(&question_text);
    let mut remaining = token_budget.saturating_sub(question_tokens);

    let (summary_text, summary_tokens, summary_truncated) = match summary {
        Some(raw) if !raw.trim().is_empty() => {
            let raw = raw.trim();
            let cost = estimate_tokens(raw);
            if cost <= remaining {
                (Some(raw.to_string()), cost, false)
            } else if remaining > 0 {
                let (text, truncated) = truncate_to_token_budget(raw, remaining);
                notes.push("历史摘要超出预算,已截断".to_string());
                (Some(text), remaining.min(cost), truncated)
            } else {
                notes.push("历史摘要超出预算,已省略".to_string());
                (None, 0, true)
            }
        }
        _ => (None, 0, false),
    };
    remaining = remaining.saturating_sub(summary_tokens);

    let fit = fit_recent_turns(turns, remaining, MAX_HISTORY_TURN_CHARS);
    notes.extend(fit.notes.iter().cloned());
    let used_turns = fit.tokens;

    LayeredHistory {
        summary: summary_text,
        summary_tokens,
        turns: fit.turns,
        tokens: question_tokens + summary_tokens + used_turns,
        question: question_text,
        question_truncated,
        dropped_turns: fit.dropped_turns,
        truncated_turns: fit.truncated_turns,
        summary_truncated,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval_ai::models::ChatTurnRole;

    fn turn(text: &str) -> ChatTurn {
        ChatTurn {
            role: ChatTurnRole::User,
            text: text.to_string(),
        }
    }

    #[test]
    fn token_estimate_handles_chinese_english_empty_and_long_text() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("中文四个字"), 5);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens("abcd中文"), 3);
        // 超长文本不 panic,结果随长度单调增长。
        let long = "字".repeat(100_000);
        assert_eq!(estimate_tokens(&long), 100_000);
    }

    #[test]
    fn context_window_budget_keeps_headroom_and_sums_correctly() {
        let b = TokenBudget::for_context_window(16_384);
        assert!(b.total() <= 16_384);
        assert!(b.total() <= 16_384 * (100 - BUDGET_HEADROOM_PERCENT) / 100);
        assert_eq!(b.prompt_limit(), b.system + b.history + b.retrieval);
        assert!(b.retrieval > b.history && b.history > b.output);

        // 小窗口也不能因最低 system 份额而超出窗口预算。
        let small = TokenBudget::for_context_window(1_024);
        assert!(small.total() <= 1_024);
        assert_eq!(
            small.prompt_limit(),
            small.system + small.history + small.retrieval
        );
    }

    #[test]
    fn truncation_reports_and_stays_within_budget() {
        let text = "中文".repeat(100);
        let (out, truncated) = truncate_to_token_budget(&text, 20);
        assert!(truncated);
        assert!(out.ends_with('…'));
        assert!(estimate_tokens(&out) <= 20);
        let (same, truncated) = truncate_to_token_budget("短文本", 100);
        assert!(!truncated);
        assert_eq!(same, "短文本");
    }

    #[test]
    fn summary_has_priority_over_recent_turns() {
        let turns: Vec<ChatTurn> = (0..6).map(|i| turn(&"旧内容".repeat(40 + i))).collect();
        let fitted = fit_history_and_question(Some("摘要保留关键事实"), &turns, "当前问题", 80);
        assert_eq!(fitted.summary.as_deref(), Some("摘要保留关键事实"));
        assert!(fitted.dropped_turns > 0, "预算不足时先丢最近原文");
        assert!(!fitted.summary_truncated, "摘要不应被先裁剪");
        assert!(fitted.tokens <= 80);
        assert!(fitted.history_note().is_some());
    }

    #[test]
    fn summary_is_truncated_only_after_turns_are_dropped() {
        let summary = "摘要".repeat(100);
        let turns = vec![turn(&"回答".repeat(100))];
        let fitted = fit_history_and_question(Some(&summary), &turns, "问题", 60);
        assert!(fitted.summary_truncated);
        assert_eq!(fitted.turns.len(), 0);
        assert!(fitted.notes.iter().any(|n| n.contains("摘要")));
        assert!(fitted.tokens <= 60);
    }

    #[test]
    fn oversized_question_is_truncated_and_recorded() {
        let question = "问".repeat(200);
        let fitted = fit_history_and_question(None, &[], &question, 30);
        assert!(fitted.question_truncated);
        assert!(estimate_tokens(&fitted.question) <= 30);
        assert!(fitted.notes.iter().any(|n| n.contains("问题")));
    }
}
