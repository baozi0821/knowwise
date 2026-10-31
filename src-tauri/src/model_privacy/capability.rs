//! 模型能力表(2F 长上下文):上下文窗口、最大输出与能力标记。
//!
//! 背景:此前预算固定 16k,与实际模型窗口无关,长文档只能靠截断。
//! 这里把“模型真实窗口”变成可查询、可修正的数据:
//! - 自动探测:已知模型名前缀 → 内置保守值;探测失败回退默认窗口;
//! - 手动修正:设置写入 `source='manual'`,自动探测不再覆盖;
//! - 预算:调用方按 `context_window` 计算 `TokenBudget`,不再用常数。
//!
//! 表里只存模型标识与数值,不含正文、密钥或路径。

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};

use crate::persistence::{db_app_error, Db};
use crate::shared::{AppError, Clock, CoreErrorCode, SystemClock};

/// 探测不到时的保守默认窗口(与旧预算一致,不静默放大)。
pub const DEFAULT_CONTEXT_WINDOW: i64 = 16_384;
/// 默认最大输出。
pub const DEFAULT_MAX_OUTPUT: i64 = 4_096;
/// 允许的手动修正范围(防御性;防止误填 0 或超大值)。
pub const MIN_CONTEXT_WINDOW: i64 = 2_048;
pub const MAX_CONTEXT_WINDOW: i64 = 2_000_000;
pub const MIN_MAX_OUTPUT: i64 = 256;
pub const MAX_MAX_OUTPUT: i64 = 128_000;

/// 能力来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySource {
    Auto,
    Manual,
}

impl CapabilitySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    pub fn parse_db(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// 一条模型能力记录(视图与预算共用)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapability {
    pub provider_id: String,
    pub model_id: String,
    pub context_window: i64,
    pub max_output: i64,
    pub streaming: bool,
    pub reasoning: bool,
    pub tool_calling: bool,
    pub vision: bool,
    pub json_schema: bool,
    pub source: CapabilitySource,
}

impl ModelCapability {
    /// 未知模型的内置默认(保守):不假设大窗口,不假设工具能力。
    pub fn fallback(provider_id: &str, model_id: &str) -> Self {
        let lower = model_id.to_ascii_lowercase();
        let (context_window, max_output, reasoning) = if lower.contains("deepseek") {
            // DeepSeek 系列常见 64k 窗口;输出保守取 8k。
            (64_000, 8_000, true)
        } else if lower.contains("gpt-4o") || lower.contains("gpt-4.1") {
            (128_000, 16_000, false)
        } else if lower.contains("claude") {
            (200_000, 8_000, false)
        } else if lower.contains("qwen")
            || lower.contains("glm")
            || lower.contains("llama")
            || lower.contains("mistral")
        {
            (32_768, 4_096, false)
        } else {
            (DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_OUTPUT, false)
        };
        Self {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            context_window,
            max_output,
            streaming: true,
            reasoning,
            // 工具调用必须显式确认(能力声明在提供方侧);默认不假设。
            tool_calling: false,
            vision: false,
            json_schema: false,
            source: CapabilitySource::Auto,
        }
    }

    /// 预算可用窗口:扣除输出预留后仍为正。
    pub fn prompt_window(&self) -> usize {
        (self.context_window - self.max_output).max(MIN_CONTEXT_WINDOW) as usize
    }
}

/// 手动修正输入(设置 UI)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityOverride {
    pub provider_id: String,
    pub model_id: String,
    pub context_window: i64,
    pub max_output: i64,
    pub tool_calling: bool,
    pub vision: bool,
    pub json_schema: bool,
}

pub struct ModelCapabilityService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
}

impl ModelCapabilityService {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            clock: Arc::new(SystemClock),
        }
    }

    /// 读取能力:表里有记录用记录,否则返回内置默认(不落库)。
    pub fn get(&self, provider_id: &str, model_id: &str) -> Result<ModelCapability, AppError> {
        let pid = provider_id.to_string();
        let mid = model_id.to_string();
        let row = self.db.read(move |conn| {
            conn.query_row(
                "SELECT context_window, max_output, streaming, reasoning, tool_calling, \
                 vision, json_schema, source FROM model_capability \
                 WHERE provider_id=?1 AND model_id=?2",
                params![pid, mid],
                |r| {
                    Ok(ModelCapability {
                        provider_id: String::new(),
                        model_id: String::new(),
                        context_window: r.get(0)?,
                        max_output: r.get(1)?,
                        streaming: r.get::<_, i64>(2)? != 0,
                        reasoning: r.get::<_, i64>(3)? != 0,
                        tool_calling: r.get::<_, i64>(4)? != 0,
                        vision: r.get::<_, i64>(5)? != 0,
                        json_schema: r.get::<_, i64>(6)? != 0,
                        source: CapabilitySource::parse_db(&r.get::<_, String>(7)?)
                            .unwrap_or(CapabilitySource::Auto),
                    })
                },
            )
            .optional()
            .map_err(|e| db_app_error(&e))
        })?;
        match row {
            Some(mut capability) => {
                capability.provider_id = provider_id.to_string();
                capability.model_id = model_id.to_string();
                Ok(capability)
            }
            None => Ok(ModelCapability::fallback(provider_id, model_id)),
        }
    }

    /// 记录自动探测结果:已有 manual 记录时不覆盖(用户修正优先)。
    pub fn record_auto(&self, capability: &ModelCapability) -> Result<(), AppError> {
        let row = capability.clone();
        let now = self.clock.now_ms();
        self.db.write(move |conn| {
            let existing: Option<String> = conn
                .query_row(
                    "SELECT source FROM model_capability WHERE provider_id=?1 AND model_id=?2",
                    params![row.provider_id, row.model_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if existing.as_deref() == Some("manual") {
                return Ok(());
            }
            conn.execute(
                "INSERT INTO model_capability(provider_id, model_id, context_window, max_output, \
                 streaming, reasoning, tool_calling, vision, json_schema, source, updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'auto',?10) \
                 ON CONFLICT(provider_id, model_id) DO UPDATE SET \
                 context_window=excluded.context_window, max_output=excluded.max_output, \
                 streaming=excluded.streaming, reasoning=excluded.reasoning, \
                 tool_calling=excluded.tool_calling, vision=excluded.vision, \
                 json_schema=excluded.json_schema, source='auto', updated_at=excluded.updated_at",
                params![
                    row.provider_id,
                    row.model_id,
                    row.context_window,
                    row.max_output,
                    i64::from(row.streaming),
                    i64::from(row.reasoning),
                    i64::from(row.tool_calling),
                    i64::from(row.vision),
                    i64::from(row.json_schema),
                    now,
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }

    /// 手动修正:写 manual 记录;非法范围直接拒绝(不静默夹取)。
    pub fn set_manual(&self, input: CapabilityOverride) -> Result<ModelCapability, AppError> {
        if input.provider_id.trim().is_empty() || input.model_id.trim().is_empty() {
            return Err(err("provider_or_model_empty"));
        }
        if !(MIN_CONTEXT_WINDOW..=MAX_CONTEXT_WINDOW).contains(&input.context_window) {
            return Err(err("context_window_out_of_range"));
        }
        if !(MIN_MAX_OUTPUT..=MAX_MAX_OUTPUT).contains(&input.max_output) {
            return Err(err("max_output_out_of_range"));
        }
        if input.max_output >= input.context_window {
            return Err(err("max_output_not_less_than_window"));
        }
        let row = input.clone();
        let now = self.clock.now_ms();
        self.db.write(move |conn| {
            conn.execute(
                "INSERT INTO model_capability(provider_id, model_id, context_window, max_output, \
                 streaming, reasoning, tool_calling, vision, json_schema, source, updated_at) \
                 VALUES (?1,?2,?3,?4,1,0,?5,?6,?7,'manual',?8) \
                 ON CONFLICT(provider_id, model_id) DO UPDATE SET \
                 context_window=excluded.context_window, max_output=excluded.max_output, \
                 tool_calling=excluded.tool_calling, vision=excluded.vision, \
                 json_schema=excluded.json_schema, source='manual', updated_at=excluded.updated_at",
                params![
                    row.provider_id,
                    row.model_id,
                    row.context_window,
                    row.max_output,
                    i64::from(row.tool_calling),
                    i64::from(row.vision),
                    i64::from(row.json_schema),
                    now,
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })?;
        self.get(&input.provider_id, &input.model_id)
    }
}

fn err(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
        .with_detail("reason", reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{Db, MigrationRunner};
    use std::path::PathBuf;

    fn harness(tag: &str) -> (Arc<Db>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kw-cap-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("t.sqlite3");
        let db = Arc::new(Db::open(&db_path, 2).unwrap());
        let mut all = crate::persistence::registration::migrations();
        // m08 迁移引用 document(外键),需先应用 M02。
        all.extend(crate::document::registration::migrations());
        all.extend(crate::model_privacy::registration::migrations());
        let runner = MigrationRunner::new(dir.join("backups"));
        let db2 = Arc::clone(&db);
        db2.write(move |conn| {
            runner.run(conn, &all, &db_path, 1_000_000).unwrap();
            Ok(())
        })
        .unwrap();
        (db, dir)
    }

    #[test]
    fn fallback_guesses_known_families_conservatively() {
        let deepseek = ModelCapability::fallback("p", "cn:deepseek-v4.1-flash");
        assert_eq!(deepseek.context_window, 64_000);
        assert!(deepseek.reasoning);
        assert!(!deepseek.tool_calling, "工具能力不得默认假设");
        let unknown = ModelCapability::fallback("p", "some-local-model");
        assert_eq!(unknown.context_window, DEFAULT_CONTEXT_WINDOW);
        assert!(unknown.prompt_window() > 0);
    }

    #[test]
    fn manual_override_wins_and_survives_auto_record() {
        let (db, dir) = harness("manual");
        let svc = ModelCapabilityService::new(db);
        let saved = svc
            .set_manual(CapabilityOverride {
                provider_id: "p".into(),
                model_id: "m".into(),
                context_window: 200_000,
                max_output: 8_000,
                tool_calling: true,
                vision: true,
                json_schema: true,
            })
            .unwrap();
        assert_eq!(saved.context_window, 200_000);
        assert_eq!(saved.source, CapabilitySource::Manual);

        // 自动探测不得覆盖用户修正。
        let auto = ModelCapability::fallback("p", "m");
        svc.record_auto(&auto).unwrap();
        let after = svc.get("p", "m").unwrap();
        assert_eq!(after.context_window, 200_000);
        assert_eq!(after.source, CapabilitySource::Manual);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_override_is_rejected_not_clamped() {
        let (db, dir) = harness("invalid");
        let svc = ModelCapabilityService::new(db);
        let base = CapabilityOverride {
            provider_id: "p".into(),
            model_id: "m".into(),
            context_window: 100_000,
            max_output: 4_000,
            tool_calling: false,
            vision: false,
            json_schema: false,
        };
        let too_small = CapabilityOverride {
            context_window: 100,
            ..base.clone()
        };
        assert_eq!(
            svc.set_manual(too_small).unwrap_err().code,
            "INVALID_REQUEST"
        );
        let output_ge_window = CapabilityOverride {
            max_output: 100_000,
            ..base
        };
        assert_eq!(
            svc.set_manual(output_ge_window).unwrap_err().code,
            "INVALID_REQUEST"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn auto_record_is_upsert_and_visible() {
        let (db, dir) = harness("auto");
        let svc = ModelCapabilityService::new(db);
        let mut auto = ModelCapability::fallback("p", "cn:deepseek-v4.1-flash");
        auto.context_window = 128_000;
        svc.record_auto(&auto).unwrap();
        let got = svc.get("p", "cn:deepseek-v4.1-flash").unwrap();
        assert_eq!(got.context_window, 128_000);
        assert_eq!(got.source, CapabilitySource::Auto);
        let _ = std::fs::remove_dir_all(dir);
    }
}
