//! P0 1E:可选的思考记录持久化。
//!
//! 默认不写;只有用户在设置中开启“保存思考记录”才在运行结束后写入一条。
//! 该数据纳入隐私清除(文档级)与会话/文档删除级联。
//! 记录内容不进入普通日志、诊断包或遥测。

use std::sync::Arc;

use rusqlite::OptionalExtension;

use crate::model_privacy::clear::DataClearContributor;
use crate::model_privacy::models::{ClearCategory, ClearCategoryExecution, ClearCategoryPreview};
use crate::persistence::{db_app_error, Db};
use crate::shared::{AppError, Clock, IdGenerator};

/// 单条思考记录的读取投影。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredReasoning {
    pub message_id: String,
    pub task_id: String,
    pub text: String,
    pub char_count: i64,
    pub created_at: i64,
}

pub struct ReasoningService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl ReasoningService {
    pub fn new(db: Arc<Db>, clock: Arc<dyn Clock>, ids: Arc<dyn IdGenerator>) -> Self {
        Self { db, clock, ids }
    }

    /// 保存一条运行结束后的思考记录(同 message 覆盖)。空文本不写。
    pub fn save(
        &self,
        document_id: &str,
        task_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), AppError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let id = self.ids.new_entity_id();
        let created_at = self.clock.now_ms();
        let document_id = document_id.to_string();
        let task_id = task_id.to_string();
        let message_id = message_id.to_string();
        let text = text.to_string();
        let char_count = text.chars().count() as i64;
        self.db.write(move |conn| {
            conn.execute(
                "INSERT INTO agent_reasoning
                    (id, document_id, task_id, message_id, provider_source, text, char_count, created_at)
                 VALUES (?1,?2,?3,?4,'model_reasoning',?5,?6,?7)
                 ON CONFLICT(message_id) DO UPDATE SET
                    text=excluded.text, char_count=excluded.char_count, created_at=excluded.created_at",
                rusqlite::params![id, document_id, task_id, message_id, text, char_count, created_at],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }

    pub fn get(&self, message_id: &str) -> Result<Option<StoredReasoning>, AppError> {
        let message_id = message_id.to_string();
        self.db.read(move |conn| {
            conn.query_row(
                "SELECT message_id, task_id, text, char_count, created_at
                 FROM agent_reasoning WHERE message_id=?1",
                [message_id],
                |row| {
                    Ok(StoredReasoning {
                        message_id: row.get(0)?,
                        task_id: row.get(1)?,
                        text: row.get(2)?,
                        char_count: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|e| db_app_error(&e))
        })
    }

    pub fn clear_for_document(&self, document_id: &str) -> Result<i64, AppError> {
        let document_id = document_id.to_string();
        self.db.write(move |conn| {
            let deleted = conn
                .execute(
                    "DELETE FROM agent_reasoning WHERE document_id=?1",
                    [document_id],
                )
                .map_err(|e| db_app_error(&e))?;
            Ok(deleted as i64)
        })
    }

    pub fn count_for_document(&self, document_id: &str) -> Result<i64, AppError> {
        let document_id = document_id.to_string();
        self.db.read(move |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM agent_reasoning WHERE document_id=?1",
                [document_id],
                |row| row.get(0),
            )
            .map_err(|e| db_app_error(&e))
        })
    }
}

/// M08 隐私清除 Contributor:只处理 AgentReasoning 类别。
pub struct AgentReasoningClearContributor {
    service: Arc<ReasoningService>,
}

impl AgentReasoningClearContributor {
    pub fn new(service: Arc<ReasoningService>) -> Self {
        Self { service }
    }
}

impl DataClearContributor for AgentReasoningClearContributor {
    fn contributor_id(&self) -> &'static str {
        "m13_agent_reasoning"
    }

    fn supported_categories(&self) -> &'static [ClearCategory] {
        &[ClearCategory::AgentReasoning]
    }

    fn preview(
        &self,
        document_id: &str,
        categories: &[ClearCategory],
    ) -> Result<Vec<ClearCategoryPreview>, AppError> {
        if !categories.contains(&ClearCategory::AgentReasoning) {
            return Ok(Vec::new());
        }
        let count = self.service.count_for_document(document_id)?;
        Ok(vec![ClearCategoryPreview {
            category: ClearCategory::AgentReasoning,
            available: true,
            unavailable_reason: None,
            count: Some(count),
            affected_citations: Some(0),
            protected_count: None,
        }])
    }

    fn execute(
        &self,
        document_id: &str,
        categories: &[ClearCategory],
    ) -> Result<Vec<ClearCategoryExecution>, AppError> {
        if !categories.contains(&ClearCategory::AgentReasoning) {
            return Ok(Vec::new());
        }
        let deleted = self.service.clear_for_document(document_id)?;
        Ok(vec![ClearCategoryExecution {
            category: ClearCategory::AgentReasoning,
            deleted,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{SystemClock, SystemIdGenerator};

    fn service(dir: &std::path::Path) -> ReasoningService {
        let db = Arc::new(Db::open(&dir.join("reasoning.sqlite3"), 2).unwrap());
        db.write(|conn| {
            conn.execute_batch(include_str!(
                "../../migrations/m23/0001_agent_reasoning.sql"
            ))
            .map_err(|e| db_app_error(&e))
        })
        .unwrap();
        ReasoningService::new(db, Arc::new(SystemClock), Arc::new(SystemIdGenerator))
    }

    #[test]
    fn saves_reads_and_clears_by_document() {
        let dir = std::env::temp_dir().join(format!("kw-reasoning-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let service = service(&dir);
        service
            .save("doc-1", "task-1", "msg-1", "provider reasoning")
            .unwrap();
        let row = service.get("msg-1").unwrap().unwrap();
        assert_eq!(row.text, "provider reasoning");
        assert_eq!(service.count_for_document("doc-1").unwrap(), 1);
        assert_eq!(service.clear_for_document("doc-1").unwrap(), 1);
        assert!(service.get("msg-1").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
