use std::sync::Arc;

use rusqlite::params;

use crate::persistence::Db;
use crate::shared::{AppError, Clock, CoreErrorCode, IdGenerator};

use super::models::{RunMetricsInput, RunMetricsView};

/// 运行指标写入与读取。写入失败不影响回答本身(指标不是事实来源),
/// 调用方只记录告警。
pub struct RunMetricsService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl RunMetricsService {
    pub fn new(db: Arc<Db>, clock: Arc<dyn Clock>, ids: Arc<dyn IdGenerator>) -> Self {
        Self { db, clock, ids }
    }

    pub fn record(&self, input: RunMetricsInput) -> Result<(), AppError> {
        if input.mode != "fast" && input.mode != "quality" {
            return Err(AppError::new(CoreErrorCode::InvalidRequest, "metrics")
                .with_detail("reason", "invalid_mode".into()));
        }
        let id = self.ids.new_entity_id();
        let created_at = self.clock.now_ms();
        self.db.write(move |conn| {
            conn.execute(
                "INSERT INTO run_metrics (
                    id, task_id, message_id, mode, status, first_token_ms,
                    first_visible_answer_ms, reasoning_first_event_ms,
                    max_event_gap_ms, first_answer_ms, total_ms, fallback_used,
                    citation_valid_rate, created_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    id,
                    input.task_id,
                    input.message_id,
                    input.mode,
                    input.status,
                    input.first_token_ms,
                    input.first_visible_answer_ms,
                    input.reasoning_first_event_ms,
                    input.max_event_gap_ms,
                    input.first_answer_ms,
                    input.total_ms,
                    i64::from(input.fallback_used),
                    input.citation_valid_rate,
                    created_at,
                ],
            )
            .map_err(|e| crate::persistence::db_app_error(&e))?;
            Ok(())
        })
    }

    pub fn recent(&self, limit: i64) -> Result<Vec<RunMetricsView>, AppError> {
        let limit = limit.clamp(1, 200);
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, task_id, message_id, mode, status, first_token_ms,
                            first_visible_answer_ms, reasoning_first_event_ms,
                            max_event_gap_ms, first_answer_ms, total_ms, fallback_used,
                            citation_valid_rate, created_at
                     FROM run_metrics ORDER BY created_at DESC LIMIT ?1",
                )
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            let rows = stmt
                .query_map([limit], |row| {
                    Ok(RunMetricsView {
                        id: row.get(0)?,
                        task_id: row.get(1)?,
                        message_id: row.get(2)?,
                        mode: row.get(3)?,
                        status: row.get(4)?,
                        first_token_ms: row.get(5)?,
                        first_visible_answer_ms: row.get(6)?,
                        reasoning_first_event_ms: row.get(7)?,
                        max_event_gap_ms: row.get(8)?,
                        first_answer_ms: row.get(9)?,
                        total_ms: row.get(10)?,
                        fallback_used: row.get::<_, i64>(11)? != 0,
                        citation_valid_rate: row.get(12)?,
                        created_at: row.get(13)?,
                    })
                })
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| crate::persistence::db_app_error(&e))
        })
    }

    /// 隐私清除/任务删除时按任务清理(指标不含正文,但避免残留关联)。
    pub fn clear_for_task(&self, task_id: &str) -> Result<(), AppError> {
        let task_id = task_id.to_string();
        self.db.write(move |conn| {
            conn.execute("DELETE FROM run_metrics WHERE task_id=?1", [task_id])
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{SystemClock, SystemIdGenerator};

    fn service(dir: &std::path::Path) -> RunMetricsService {
        let db = Arc::new(Db::open(&dir.join("metrics.sqlite3"), 2).unwrap());
        db.write(|conn| {
            conn.execute_batch(include_str!("../../migrations/m22/0001_run_metrics.sql"))
                .map_err(|e| crate::persistence::db_app_error(&e))
        })
        .unwrap();
        RunMetricsService::new(db, Arc::new(SystemClock), Arc::new(SystemIdGenerator))
    }

    #[test]
    fn records_and_reads_metrics_without_content_fields() {
        let dir = std::env::temp_dir().join(format!("kw-metrics-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let service = service(&dir);
        service
            .record(RunMetricsInput {
                task_id: "t1".into(),
                message_id: Some("m1".into()),
                mode: "fast".into(),
                status: "verified".into(),
                first_token_ms: Some(300),
                first_visible_answer_ms: Some(420),
                reasoning_first_event_ms: None,
                max_event_gap_ms: 900,
                first_answer_ms: Some(420),
                total_ms: Some(2100),
                fallback_used: false,
                citation_valid_rate: Some(1.0),
            })
            .unwrap();
        let rows = service.recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mode, "fast");
        assert_eq!(rows[0].first_visible_answer_ms, Some(420));
        assert!(!rows[0].fallback_used);
        service.clear_for_task("t1").unwrap();
        assert!(service.recent(10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_unknown_mode() {
        let dir = std::env::temp_dir().join(format!("kw-metrics-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let service = service(&dir);
        let result = service.record(RunMetricsInput {
            task_id: "t1".into(),
            message_id: None,
            mode: "other".into(),
            status: "failed".into(),
            first_token_ms: None,
            first_visible_answer_ms: None,
            reasoning_first_event_ms: None,
            max_event_gap_ms: 0,
            first_answer_ms: None,
            total_ms: None,
            fallback_used: true,
            citation_valid_rate: None,
        });
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
