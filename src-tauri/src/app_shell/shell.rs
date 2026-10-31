//! M13 壳层启动状态与恢复页数据(《M13》§2)。
//!
//! 启动阶段发生数据库损坏、迁移失败或配置不可解析时,setup 不再
//! 让进程崩溃退出,而是托管 `ShellStartup::failed`,前端通过
//! `shell_get_status` 感知并进入可操作恢复页;业务命令在恢复模式下
//! 不会被前端调用。本模块在默认特性(无 Tauri)下也编译,启动状态机
//! 可直接单测。

use ts_rs::TS;

use crate::shared::error::DetailValue;
use crate::shared::settings::{SettingsStore, SettingsV1};
use crate::shared::{new_trace_id, AppError};

/// WebView 事件通道:所有 DomainEvent 经 TauriEventSink 广播到该通道,
/// 前端事件桥只订阅这一个通道(《M13》§5)。与前端 `events/bridge.ts`
/// 的 DOMAIN_EVENT_CHANNEL 字面量保持一致。
pub const DOMAIN_EVENT_CHANNEL: &str = "domain-event";

/// 壳层状态。`failed` 时 `error` 必填;`ready` 时业务 AppState 已托管。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "src/shared/generated/")]
pub enum ShellState {
    Booting,
    Ready,
    Failed,
}

/// `shell_get_status` 返回视图。非敏感:只含状态与稳定错误信封。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct ShellStatusView {
    pub schema_version: &'static str,
    pub state: ShellState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AppError>,
}

impl ShellStatusView {
    pub const SCHEMA_VERSION: &'static str = "shell.status.v1";
}

/// Tauri 托管状态:setup 完成后总是存在(Ready 或 Failed)。
#[derive(Debug, Clone)]
pub struct ShellStartup {
    pub status: ShellStatusView,
}

impl ShellStartup {
    pub fn ready() -> Self {
        Self {
            status: ShellStatusView {
                schema_version: ShellStatusView::SCHEMA_VERSION,
                state: ShellState::Ready,
                error: None,
            },
        }
    }

    pub fn failed(error: AppError) -> Self {
        Self {
            status: ShellStatusView {
                schema_version: ShellStatusView::SCHEMA_VERSION,
                state: ShellState::Failed,
                error: Some(error),
            },
        }
    }
}

/// 设置文件不可解析/不可读(《M13》§2:配置不可解析 → 恢复页,不覆盖原文件)。
/// 错误码只增不改语义(《02》§7);该码由 M13 定义,不并入 M00 的 CoreErrorCode。
pub const SETTINGS_INVALID_CODE: &str = "SETTINGS_INVALID";

pub fn settings_invalid_error(reason: &str) -> AppError {
    AppError {
        schema_version: "error.v1",
        code: SETTINGS_INVALID_CODE.to_string(),
        message_key: "error.settings_invalid".to_string(),
        retryable: false,
        user_action: crate::shared::UserAction::None,
        details: Some(
            [
                ("reason".to_string(), DetailValue::Str(reason.to_string())),
                ("recoverableCopy".to_string(), DetailValue::Bool(true)),
            ]
            .into_iter()
            .collect(),
        ),
        trace_id: new_trace_id(),
    }
}

/// 加载设置并映射为 AppError(未知 schemaVersion 与 IO 错误都进入恢复页,
/// 绝不静默回退默认值覆盖用户配置)。
pub fn load_settings_or_fail(store: &SettingsStore) -> Result<SettingsV1, AppError> {
    match store.load() {
        Ok(s) => Ok(s),
        Err(crate::shared::settings::SettingsError::UnknownSchemaVersion(v)) => {
            Err(settings_invalid_error("unknown_schema_version")
                .with_detail("schemaVersion", DetailValue::Str(v)))
        }
        Err(crate::shared::settings::SettingsError::File(_)) => {
            Err(settings_invalid_error("file_unreadable"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_status_roundtrip_states() {
        let ready = ShellStartup::ready();
        assert_eq!(ready.status.state, ShellState::Ready);
        assert!(ready.status.error.is_none());
        assert_eq!(ready.status.schema_version, "shell.status.v1");

        let err = AppError::new(crate::shared::CoreErrorCode::DbCorrupt, "t1");
        let failed = ShellStartup::failed(err.clone());
        assert_eq!(failed.status.state, ShellState::Failed);
        assert_eq!(failed.status.error.as_ref().unwrap().code, "DB_CORRUPT");
    }

    #[test]
    fn failed_status_serializes_stable_envelope() {
        let view = ShellStartup::failed(settings_invalid_error("unknown_schema_version"))
            .status
            .clone();
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["schemaVersion"], "shell.status.v1");
        assert_eq!(json["state"], "failed");
        assert_eq!(json["error"]["code"], "SETTINGS_INVALID");
        assert_eq!(json["error"]["userAction"], "none");
        assert_eq!(json["error"]["retryable"], false);
    }

    #[test]
    fn domain_event_channel_is_stable() {
        // 前端 events/bridge.ts 引用同一字面量;通道名是跨端契约。
        assert_eq!(DOMAIN_EVENT_CHANNEL, "domain-event");
    }

    #[test]
    fn settings_unknown_schema_maps_to_recovery_not_default() {
        let dir = std::env::temp_dir().join(format!("kw-m13-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"schemaVersion":"settings.v0","maxPdfMib":1}"#,
        )
        .unwrap();
        let store = SettingsStore::open(&dir);
        let err = load_settings_or_fail(&store).unwrap_err();
        assert_eq!(err.code, "SETTINGS_INVALID");
        assert_eq!(err.user_action, crate::shared::UserAction::None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
