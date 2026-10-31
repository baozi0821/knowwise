//! M08 模块错误码(《02》§7 基线的 PRIVACY_*/SECRET_*/MODEL_* 部分)。
//!
//! 依《02》§7"M07+ 由自有模块定义":这些码不进入 `shared::CoreErrorCode`,
//! 但字符串、retryable 与 userAction 语义与《02》§7 基线完全一致,
//! 错误码只增不改语义。AppError 字段公开,此处直接构造统一信封。

use crate::shared::error::{AppError, UserAction};

/// M08 拥有的稳定错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelErrorCode {
    /// 未授权(生成/嵌入前须取得处理位置授权)。
    PrivacyGrantRequired,
    /// 出口拒绝:许可无效/过期/重放、载荷哈希不符、来源越界、任务已取消。
    PrivacyEgressDenied,
    /// 清除计划过期(含预览后数据漂移,须重新预览)。
    PrivacyPlanExpired,
    /// 系统凭据存储不可用;禁止退回明文文件。
    SecretStoreUnavailable,
    /// 提供方不可达/未配置/模型不存在/鉴权被拒。
    ModelUnavailable,
    /// 请求超时。
    ModelTimeout,
    /// 模型输出 JSON 不合法或未通过 Schema 基础校验。
    ModelOutputInvalid,
    /// 供应商额度不足或已限流。
    ModelQuotaExhausted,
    /// 诊断导出目标已存在;不覆盖已有文件。
    ExportTargetExists,
}

impl ModelErrorCode {
    pub fn code(&self) -> &'static str {
        match self {
            Self::PrivacyGrantRequired => "PRIVACY_GRANT_REQUIRED",
            Self::PrivacyEgressDenied => "PRIVACY_EGRESS_DENIED",
            Self::PrivacyPlanExpired => "PRIVACY_PLAN_EXPIRED",
            Self::SecretStoreUnavailable => "SECRET_STORE_UNAVAILABLE",
            Self::ModelUnavailable => "MODEL_UNAVAILABLE",
            Self::ModelTimeout => "MODEL_TIMEOUT",
            Self::ModelOutputInvalid => "MODEL_OUTPUT_INVALID",
            Self::ModelQuotaExhausted => "MODEL_QUOTA_EXHAUSTED",
            Self::ExportTargetExists => "EXPORT_TARGET_EXISTS",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::PrivacyPlanExpired | Self::ModelTimeout | Self::ModelOutputInvalid
        )
    }

    pub fn user_action(&self) -> UserAction {
        match self {
            Self::PrivacyGrantRequired | Self::PrivacyEgressDenied => UserAction::GrantPermission,
            Self::PrivacyPlanExpired => UserAction::Retry,
            Self::SecretStoreUnavailable => UserAction::None,
            Self::ModelUnavailable => UserAction::OpenSettings,
            Self::ModelTimeout => UserAction::Retry,
            Self::ModelOutputInvalid => UserAction::Retry,
            Self::ModelQuotaExhausted => UserAction::OpenSettings,
            Self::ExportTargetExists => UserAction::ChooseOtherFile,
        }
    }

    /// 构造统一 AppError(traceId 由调用方生成)。
    pub fn to_app_error(self, trace_id: impl Into<String>) -> AppError {
        AppError {
            schema_version: "error.v1",
            code: self.code().to_string(),
            message_key: format!("error.{}", self.code().to_lowercase()),
            retryable: self.retryable(),
            user_action: self.user_action(),
            details: None,
            trace_id: trace_id.into(),
        }
    }
}

pub fn model_err(code: ModelErrorCode) -> AppError {
    code.to_app_error(crate::shared::new_trace_id())
}

/// 适配器层错误 → 稳定错误码映射(《M08》§7:超时/断连/额度/输出不合法
/// 分别映射,不合并成"生成失败";reason 为非敏感静态描述,不含供应商原文)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// 连接失败/DNS/端口不可达。
    Unreachable,
    /// 请求超时(含探测超时)。
    Timeout,
    /// 模型不存在或端点不支持该模型。
    ModelNotFound,
    /// 鉴权被拒(密钥缺失/无效)。
    AuthRejected,
    /// 额度不足或限流。
    QuotaExhausted,
    /// 供应商侧错误(状态码,仅 5xx 等)。
    ServerError(u16),
    /// 输出 JSON 不合法/维度不符。
    InvalidOutput,
    /// 响应体超过上限。
    ResponseTooLarge,
    /// 系统凭据存储不可用(远程需要密钥时)。
    SecretUnavailable,
    /// 已取消。
    Cancelled,
}

impl ProviderError {
    pub fn to_app_error(&self) -> AppError {
        // 连接失败与提供方 5xx 是"这次没成功"的瞬态外部故障:保留
        // MODEL_UNAVAILABLE 码,但标为可重试并给出重试动作;打开设置
        // 修不好上游 502。模型不存在/鉴权被拒才需要用户改设置。
        if matches!(self, Self::Unreachable | Self::ServerError(_)) {
            let mut error = model_err(ModelErrorCode::ModelUnavailable);
            error.retryable = true;
            error.user_action = UserAction::Retry;
            return match self {
                Self::Unreachable => error.with_detail("reason", "connection_failed".into()),
                Self::ServerError(code) => error
                    .with_detail("reason", "server_error".into())
                    .with_detail("status", (*code as i64).into()),
                _ => unreachable!(),
            };
        }
        let e = model_err(match self {
            Self::Unreachable | Self::ModelNotFound | Self::AuthRejected | Self::ServerError(_) => {
                ModelErrorCode::ModelUnavailable
            }
            Self::Timeout => ModelErrorCode::ModelTimeout,
            Self::InvalidOutput => ModelErrorCode::ModelOutputInvalid,
            Self::ResponseTooLarge => ModelErrorCode::ModelOutputInvalid,
            Self::SecretUnavailable => ModelErrorCode::SecretStoreUnavailable,
            Self::QuotaExhausted => ModelErrorCode::ModelQuotaExhausted,
            Self::Cancelled => {
                return AppError::new(
                    crate::shared::CoreErrorCode::TaskCanceled,
                    crate::shared::new_trace_id(),
                )
            }
        });
        let reason = match self {
            Self::Unreachable => "connection_failed",
            Self::Timeout => "timeout",
            Self::ModelNotFound => "model_not_found",
            Self::AuthRejected => "auth_rejected",
            Self::QuotaExhausted => "quota_exhausted",
            Self::ServerError(code) => {
                return e
                    .with_detail("reason", "server_error".into())
                    .with_detail("status", (*code as i64).into())
            }
            Self::InvalidOutput => "output_invalid",
            Self::ResponseTooLarge => "response_too_large",
            Self::SecretUnavailable => "secret_store_unavailable",
            Self::Cancelled => "canceled",
        };
        e.with_detail("reason", reason.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_contract_baseline() {
        assert_eq!(
            ModelErrorCode::PrivacyGrantRequired.code(),
            "PRIVACY_GRANT_REQUIRED"
        );
        assert_eq!(
            ModelErrorCode::PrivacyEgressDenied.code(),
            "PRIVACY_EGRESS_DENIED"
        );
        assert_eq!(
            ModelErrorCode::PrivacyPlanExpired.code(),
            "PRIVACY_PLAN_EXPIRED"
        );
        assert_eq!(
            ModelErrorCode::SecretStoreUnavailable.code(),
            "SECRET_STORE_UNAVAILABLE"
        );
        assert_eq!(ModelErrorCode::ModelUnavailable.code(), "MODEL_UNAVAILABLE");
        assert_eq!(ModelErrorCode::ModelTimeout.code(), "MODEL_TIMEOUT");
        assert_eq!(
            ModelErrorCode::ModelOutputInvalid.code(),
            "MODEL_OUTPUT_INVALID"
        );
        assert_eq!(
            ModelErrorCode::ModelQuotaExhausted.code(),
            "MODEL_QUOTA_EXHAUSTED"
        );
    }

    #[test]
    fn app_error_shape_matches_contract() {
        let e = model_err(ModelErrorCode::PrivacyEgressDenied)
            .with_detail("reason", "permit_expired".into());
        assert_eq!(e.schema_version, "error.v1");
        assert_eq!(e.user_action, UserAction::GrantPermission);
        assert!(!e.retryable);
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["details"]["reason"], "permit_expired");
    }

    #[test]
    fn provider_errors_map_to_distinct_codes() {
        assert_eq!(ProviderError::Timeout.to_app_error().code, "MODEL_TIMEOUT");
        assert_eq!(
            ProviderError::QuotaExhausted.to_app_error().code,
            "MODEL_QUOTA_EXHAUSTED"
        );
        assert_eq!(
            ProviderError::InvalidOutput.to_app_error().code,
            "MODEL_OUTPUT_INVALID"
        );
        assert_eq!(
            ProviderError::Unreachable.to_app_error().code,
            "MODEL_UNAVAILABLE"
        );
        assert_eq!(
            ProviderError::Cancelled.to_app_error().code,
            "TASK_CANCELED"
        );
        // 细分 reason 进入 details,不合并成单一"生成失败"。
        let e = ProviderError::AuthRejected.to_app_error();
        assert_eq!(e.details.unwrap()["reason"], "auth_rejected".into());
    }

    #[test]
    fn transient_provider_failures_are_retryable_with_retry_action() {
        // 上游 5xx 与连接失败:保留 MODEL_UNAVAILABLE,但必须可重试,
        // 且用户动作是"重试"而不是"打开设置"。
        let e = ProviderError::ServerError(502).to_app_error();
        assert_eq!(e.code, "MODEL_UNAVAILABLE");
        assert!(e.retryable);
        assert_eq!(e.user_action, UserAction::Retry);
        let details = e.details.expect("details");
        assert_eq!(details["reason"], "server_error".into());
        assert_eq!(details["status"], 502i64.into());

        let e = ProviderError::Unreachable.to_app_error();
        assert_eq!(e.code, "MODEL_UNAVAILABLE");
        assert!(e.retryable);
        assert_eq!(e.user_action, UserAction::Retry);
        assert_eq!(e.details.unwrap()["reason"], "connection_failed".into());

        // 配置类失败仍需要用户检查设置,不标可重试。
        let e = ProviderError::AuthRejected.to_app_error();
        assert!(!e.retryable);
        assert_eq!(e.user_action, UserAction::OpenSettings);
        let e = ProviderError::ModelNotFound.to_app_error();
        assert!(!e.retryable);
        assert_eq!(e.user_action, UserAction::OpenSettings);
    }
}
