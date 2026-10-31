//! 诊断收集与导出(《M08》§9 / SEC-03)。
//!
//! 默认预览只含应用版本、OS、schema、能力状态与非敏感计数;不含正文、
//! 路径、提示词和密钥。路径与模型细节仅在用户显式勾选后加入,且导出
//! 绑定 previewHash——勾选状态变化后必须重新预览并再次确认(A9)。
//! 导出使用显式路径、不覆盖已有文件;生成失败不修改任何现有配置或数据库。

use std::io::Write;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::persistence::{db_app_error, Db};
use crate::shared::{AppError, Clock, CoreErrorCode};

use super::errors::{model_err, ModelErrorCode};
use super::models::{
    CountEntry, DiagnosticsExportResult, DiagnosticsPreview, ModelStatusView, SchemaModuleVersion,
};

pub struct DiagnosticsCollector {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    /// 近期预览(哈希 → 序列化内容);导出按哈希取内容,防止导出与
    /// 用户确认的预览不一致。上限 16 条,LRU 淘汰按插入序。
    recent: Mutex<Vec<(String, String)>>,
}

const RECENT_PREVIEW_CAP: usize = 16;

fn schema_versions_tx(conn: &Connection) -> Result<Vec<SchemaModuleVersion>, AppError> {
    let mut stmt = conn
        .prepare("SELECT module, version FROM schema_migration ORDER BY module, version")
        .map_err(|e| db_app_error(&e))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(SchemaModuleVersion {
                module: r.get(0)?,
                version: r.get(1)?,
            })
        })
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_app_error(&e))?;
    Ok(rows)
}

fn task_counts_tx(conn: &Connection) -> Result<(Vec<CountEntry>, Vec<CountEntry>), AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT task_type || '/' || status, COUNT(*) FROM task \
             GROUP BY task_type, status ORDER BY task_type, status",
        )
        .map_err(|e| db_app_error(&e))?;
    let counts = stmt
        .query_map([], |r| {
            Ok(CountEntry {
                key: r.get(0)?,
                count: r.get(1)?,
            })
        })
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_app_error(&e))?;
    drop(stmt);
    let mut stmt = conn
        .prepare(
            "SELECT error_code, COUNT(*) FROM task WHERE error_code IS NOT NULL \
             GROUP BY error_code ORDER BY error_code",
        )
        .map_err(|e| db_app_error(&e))?;
    let errors = stmt
        .query_map([], |r| {
            Ok(CountEntry {
                key: r.get(0)?,
                count: r.get(1)?,
            })
        })
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_app_error(&e))?;
    Ok((counts, errors))
}

impl DiagnosticsCollector {
    pub fn new(db: Arc<Db>, clock: Arc<dyn Clock>) -> Self {
        Self {
            db,
            clock,
            recent: Mutex::new(Vec::new()),
        }
    }

    /// 生成脱敏预览。`include_*` 为本次显式选择(默认 false);
    /// `data_dir` 仅在 include_paths 时进入预览。
    pub fn preview(
        &self,
        include_paths: bool,
        include_model_details: bool,
        data_dir: &std::path::Path,
        status: &ModelStatusView,
    ) -> Result<DiagnosticsPreview, AppError> {
        let (db_schema, (task_counts, task_error_counts)) = {
            let schema = self.db.read(schema_versions_tx)?;
            let counts = self.db.read(task_counts_tx)?;
            (schema, counts)
        };
        let provider_kinds: Vec<super::models::ProviderKind> = {
            let mut kinds: Vec<super::models::ProviderKind> = Vec::new();
            for p in &status.providers {
                if !kinds.contains(&p.kind) {
                    kinds.push(p.kind);
                }
            }
            kinds.sort_by_key(|k| k.as_str());
            kinds
        };
        let preview = DiagnosticsPreview {
            schema_version: "diagnostics.v1",
            preview_hash: String::new(), // 计算后回填
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            db_schema,
            model_capability: status.capability,
            model_reasons: status.reasons.clone(),
            provider_kinds,
            task_counts,
            task_error_counts,
            data_dir_path: include_paths.then(|| data_dir.display().to_string()),
            model_details: include_model_details.then(|| status.providers.clone()),
            include_paths,
            include_model_details,
            generated_at: self.clock.now_ms(),
        };
        // previewHash 绑定全部预览内容(含勾选状态)。
        let hash = preview_hash_of(&preview);
        let preview = DiagnosticsPreview {
            preview_hash: hash.clone(),
            ..preview
        };
        let serialized = serde_json::to_string_pretty(&preview)
            .map_err(|_| AppError::internal("diagnostics-serialize"))?;
        {
            let mut recent = self.recent.lock().unwrap();
            recent.retain(|(h, _)| h != &hash);
            recent.push((hash, serialized));
            while recent.len() > RECENT_PREVIEW_CAP {
                recent.remove(0);
            }
        }
        Ok(preview)
    }

    /// 导出:按哈希取"用户确认过的"预览内容写入显式路径;不覆盖已有文件。
    pub fn export(
        &self,
        preview_hash: &str,
        path: &str,
    ) -> Result<DiagnosticsExportResult, AppError> {
        if preview_hash.is_empty() || path.is_empty() {
            return Err(AppError::new(
                CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "empty_export_request".into()));
        }
        let serialized = {
            let recent = self.recent.lock().unwrap();
            recent
                .iter()
                .find(|(h, _)| h == preview_hash)
                .map(|(_, body)| body.clone())
        };
        let Some(serialized) = serialized else {
            return Err(AppError::new(
                CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "preview_unknown_or_refreshed".into()));
        };
        let target = std::path::Path::new(path);
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                return Err(AppError::new(
                    CoreErrorCode::InvalidRequest,
                    crate::shared::new_trace_id(),
                )
                .with_detail("reason", "export_parent_missing".into()));
            }
        }
        // 显式路径,不覆盖已有文件。
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(model_err(ModelErrorCode::ExportTargetExists));
            }
            Err(e) => {
                return Err(AppError::internal(format!("diagnostics-export-io: {e}")));
            }
        };
        let bytes = serialized.as_bytes();
        file.write_all(bytes)
            .map_err(|e| AppError::internal(format!("diagnostics-export-io: {e}")))?;
        Ok(DiagnosticsExportResult {
            schema_version: "diagnostics_export.v1",
            path: path.to_string(),
            bytes: bytes.len() as i64,
        })
    }
}

/// 预览哈希:对预览内容(preview_hash 字段置空后)做键排序规范序列化。
fn preview_hash_of(preview: &DiagnosticsPreview) -> String {
    let mut value = serde_json::to_value(preview).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "previewHash".into(),
            serde_json::Value::String(String::new()),
        );
    }
    // generated_at 参与(导出绑定的是"用户看到的那份预览")。
    let canonical = super::clear::canonical_json(&value);
    crate::shared::textnorm::sha256_hex(canonical.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::TestClock;

    fn test_db(dir: &std::path::Path) -> Arc<Db> {
        let db = Db::open(&dir.join("diag.sqlite3"), 1).unwrap();
        let runner = crate::persistence::MigrationRunner::new(dir.join("backups"));
        let sets = crate::persistence::registration::migrations();
        let db = Arc::new(db);
        let db2 = Arc::clone(&db);
        let db_path = dir.join("diag.sqlite3");
        db2.write(move |conn| {
            runner.run(conn, &sets, &db_path, 1_000).unwrap();
            Ok(())
        })
        .unwrap();
        db
    }

    fn status() -> ModelStatusView {
        ModelStatusView {
            schema_version: "model_status.v1",
            capability: crate::model_privacy::models::ModelCapability::Unavailable,
            reasons: vec![],
            providers: vec![],
            generate_reference: None,
            fast_reference: None,
            quality_reference: None,
            translation_reference: None,
            embed_reference: None,
            quality_capability: None,
            secrets: vec![],
            settings_revision: 1,
        }
    }

    #[test]
    fn preview_redacts_by_default_and_export_binds_hash() {
        let dir = std::env::temp_dir().join(format!("kw-m08-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = test_db(&dir);
        let clock = Arc::new(TestClock::new(1_000));
        let collector = DiagnosticsCollector::new(db, clock);
        let secret_dir_marker = dir.display().to_string();

        let preview = collector.preview(false, false, &dir, &status()).unwrap();
        assert!(preview.data_dir_path.is_none());
        assert!(preview.model_details.is_none());
        let body = serde_json::to_string(&preview).unwrap();
        assert!(!body.contains(&secret_dir_marker), "默认预览不得包含路径");

        // 显式勾选路径后包含,且哈希不同。
        let with_paths = collector.preview(true, false, &dir, &status()).unwrap();
        assert!(with_paths.data_dir_path.is_some());
        assert_ne!(preview.preview_hash, with_paths.preview_hash);

        // 导出按哈希绑定内容;不存在/过期哈希拒绝。
        let target = dir.join("diag-export.json");
        let out = collector
            .export(&with_paths.preview_hash, target.to_str().unwrap())
            .unwrap();
        assert!(target.exists());
        assert_eq!(out.path, target.to_str().unwrap());
        let err = collector
            .export(&with_paths.preview_hash, target.to_str().unwrap())
            .unwrap_err();
        assert_eq!(err.code, "EXPORT_TARGET_EXISTS");
        let err = collector
            .export("bogus-hash", dir.join("x.json").to_str().unwrap())
            .unwrap_err();
        assert_eq!(
            err.details.unwrap()["reason"],
            "preview_unknown_or_refreshed".into()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_binding_covers_selection_flags() {
        // 同一份数据、不同勾选 → 不同哈希(防止导出与确认内容不一致)。
        let dir = std::env::temp_dir().join(format!("kw-m08-diag2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = test_db(&dir);
        let clock = Arc::new(TestClock::new(1_000));
        let collector = DiagnosticsCollector::new(db, clock);
        let a = collector.preview(false, false, &dir, &status()).unwrap();
        let b = collector.preview(false, false, &dir, &status()).unwrap();
        assert_eq!(a.preview_hash, b.preview_hash, "同选项同数据哈希稳定");
        let c = collector.preview(false, true, &dir, &status()).unwrap();
        assert_ne!(a.preview_hash, c.preview_hash);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
