//! M09 错误码(《02》§7:错误码只增不改语义)。
//!
//! M07+ 模块的错误码由自有模块定义,不并入共享 `CoreErrorCode`
//! (M00 注释)。此处按《02》第 7 节的基线表构造 M09 使用的行为码;
//! 重试性与用户动作与契约一致。

use crate::shared::error::{AppError, DetailValue, UserAction};
use crate::shared::new_trace_id;

fn m09_error(code: &str, retryable: bool, user_action: UserAction) -> AppError {
    AppError {
        schema_version: "error.v1",
        code: code.to_string(),
        message_key: format!("error.{}", code.to_lowercase()),
        retryable,
        user_action,
        details: None,
        trace_id: new_trace_id(),
    }
}

/// 覆盖不足:返回等待索引或缩小范围,流程在模型前终止(M09-A8)。
pub fn context_coverage_incomplete() -> AppError {
    m09_error(
        "CONTEXT_COVERAGE_INCOMPLETE",
        false,
        UserAction::WaitForIndex,
    )
}

/// 预算超限:要求缩小范围,不静默截断(M09-A8)。
pub fn context_budget_exceeded() -> AppError {
    m09_error("CONTEXT_BUDGET_EXCEEDED", false, UserAction::ChooseScope)
}

/// 授权被拒:模型调用前终止(E2E-13:拒绝后零内容请求)。
pub fn privacy_grant_required() -> AppError {
    m09_error("PRIVACY_GRANT_REQUIRED", false, UserAction::GrantPermission)
}

/// 模型不可达:保留问题、快照与草稿(《M09》§9)。
pub fn model_unavailable() -> AppError {
    m09_error("MODEL_UNAVAILABLE", true, UserAction::Retry)
}

/// 生成超时(连接/首字/生成细分由出口层写 details)。
pub fn model_timeout() -> AppError {
    m09_error("MODEL_TIMEOUT", true, UserAction::Retry)
}

/// 结构化输出校验失败:可重试,不把原始文本包成成功回答。
pub fn model_output_invalid(reason: &str) -> AppError {
    m09_error("MODEL_OUTPUT_INVALID", true, UserAction::Retry).with_detail("reason", reason.into())
}

/// 额度不足:只显示供应商实际返回状态。
pub fn model_quota_exhausted() -> AppError {
    m09_error("MODEL_QUOTA_EXHAUSTED", false, UserAction::OpenSettings)
}

/// 将出口层错误映射为稳定错误码(保留 details,不外泄供应商原文)。
pub fn map_egress_error(err: AppError) -> AppError {
    match err.code.as_str() {
        "MODEL_UNAVAILABLE"
        | "MODEL_TIMEOUT"
        | "MODEL_OUTPUT_INVALID"
        | "MODEL_QUOTA_EXHAUSTED"
        | "PRIVACY_GRANT_REQUIRED"
        | "TASK_CANCELED" => err,
        "INDEX_FTS_ONLY" | "INDEX_GENERATION_UNAVAILABLE" => context_coverage_incomplete(),
        _ => {
            let mut mapped = model_unavailable();
            mapped.details = err.details;
            mapped
        }
    }
}

#[allow(dead_code)]
fn _assert_detail_value_used(_: DetailValue) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_contract_table() {
        let e = context_budget_exceeded();
        assert_eq!(e.code, "CONTEXT_BUDGET_EXCEEDED");
        assert!(!e.retryable);
        assert_eq!(e.user_action, UserAction::ChooseScope);

        let e = model_output_invalid("answer_decode_failed");
        assert_eq!(e.code, "MODEL_OUTPUT_INVALID");
        assert!(e.retryable);
        assert_eq!(
            e.details.as_ref().unwrap()["reason"].clone(),
            "answer_decode_failed".into()
        );
    }
}
