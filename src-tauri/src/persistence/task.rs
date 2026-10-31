//! 任务状态机与幂等提交(M01,见《M01》§4–§6)。
//!
//! 统一状态机(《M01》§4):
//! - queued: start_attempt → running;cancel → canceled
//! - running: phase_changed → running;complete → succeeded;fail → failed;
//!   cancel_ack → canceled;recovery → interrupted
//! - failed/canceled/interrupted: explicit_retry → queued(新 Attempt)
//! - succeeded: 终态;需要新结果创建新 Task
//!
//! 结果提交在同一事务中校验:Task 存在、状态 running、current_attempt_id
//! 匹配、cancel_requested=0、TaskResult 不存在或哈希一致;任何一步失败
//! 整体回滚(《01》§9、DATA-05)。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use tokio_util::sync::CancellationToken;

use crate::shared::dto::{
    TaskOutcome, TaskPhase, TaskResultRef, TaskResultRefKind, TaskStatus, TaskType, TaskView,
};
use crate::shared::textnorm::sha256_str;
use crate::shared::{
    AppError, Clock, CoreErrorCode, DomainEvent, DomainEventType, EventSink, IdGenerator,
};

use super::connection::{db_app_error, Db};

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub id: String,
    pub document_id: Option<String>,
    pub task_type: TaskType,
    pub idempotency_key: String,
    pub status: TaskStatus,
    pub phase: TaskPhase,
    pub outcome: TaskOutcome,
    pub current_attempt_id: Option<String>,
    pub cancel_requested: bool,
    pub state_version: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub result_ref: Option<TaskResultRef>,
    /// 列表查询补充的会话归属;其它查询为 None。
    pub conversation_id: Option<String>,
    pub conversation_title: Option<String>,
}

impl TaskRow {
    pub fn to_view(&self) -> TaskView {
        TaskView {
            id: self.id.clone(),
            document_id: self.document_id.clone(),
            task_type: self.task_type,
            status: self.status,
            phase: self.phase,
            outcome: match self.outcome {
                TaskOutcome::None => None,
                o => Some(o),
            },
            cancel_requested: self.cancel_requested,
            state_version: self.state_version,
            result_ref: self.result_ref.clone(),
            // 任务中心要显示失败原因:把稳定错误码投影成 AppError
            // (只含错误码与默认动作,不含正文/路径)。
            error: self.error_code.as_deref().map(error_view_of),
            conversation_id: self.conversation_id.clone(),
            conversation_title: self.conversation_title.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// 由稳定错误码还原一个最小错误视图(供任务中心展示原因)。
/// 只保留错误码/可重试/用户动作,不携带供应商正文。
fn error_view_of(code: &str) -> crate::shared::AppError {
    crate::shared::AppError {
        schema_version: "error.v1",
        code: code.to_string(),
        message_key: format!("error.{}", code.to_lowercase()),
        retryable: matches!(
            code,
            "MODEL_TIMEOUT"
                | "MODEL_UNAVAILABLE"
                | "MODEL_OUTPUT_INVALID"
                | "INTERNAL_ERROR"
                | "INTERNAL"
                | "ANSWER_MISSING"
                | "CONTEXT_COVERAGE_INCOMPLETE"
                | "TASK_INTERRUPTED"
        ),
        user_action: crate::shared::UserAction::Retry,
        details: None,
        trace_id: String::new(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttemptRow {
    pub id: String,
    pub task_id: String,
    pub attempt_no: i64,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub status: AttemptStatus,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
    Canceled,
    Interrupted,
}

impl AttemptStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Interrupted => "interrupted",
        }
    }
    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            "interrupted" => Self::Interrupted,
            _ => return None,
        })
    }
}

fn task_type_str(t: TaskType) -> &'static str {
    match t {
        TaskType::ParseDocument => "parse_document",
        TaskType::BuildEmbedding => "build_embedding",
        TaskType::AskQuestion => "ask_question",
        TaskType::GenerateArtifact => "generate_artifact",
        TaskType::AgentRun => "agent_run",
    }
}
fn parse_task_type(s: &str) -> Option<TaskType> {
    Some(match s {
        "parse_document" => TaskType::ParseDocument,
        "build_embedding" => TaskType::BuildEmbedding,
        "ask_question" => TaskType::AskQuestion,
        "generate_artifact" => TaskType::GenerateArtifact,
        "agent_run" => TaskType::AgentRun,
        _ => return None,
    })
}
fn status_str(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Succeeded => "succeeded",
        TaskStatus::Failed => "failed",
        TaskStatus::Canceled => "canceled",
        TaskStatus::Interrupted => "interrupted",
    }
}
fn parse_status(s: &str) -> Option<TaskStatus> {
    Some(match s {
        "queued" => TaskStatus::Queued,
        "running" => TaskStatus::Running,
        "succeeded" => TaskStatus::Succeeded,
        "failed" => TaskStatus::Failed,
        "canceled" => TaskStatus::Canceled,
        "interrupted" => TaskStatus::Interrupted,
        _ => return None,
    })
}
fn phase_str(p: TaskPhase) -> &'static str {
    match p {
        TaskPhase::Waiting => "waiting",
        TaskPhase::Parsing => "parsing",
        TaskPhase::Indexing => "indexing",
        TaskPhase::Retrieving => "retrieving",
        TaskPhase::Generating => "generating",
        TaskPhase::Validating => "validating",
        TaskPhase::Committing => "committing",
    }
}
fn parse_phase(s: &str) -> Option<TaskPhase> {
    Some(match s {
        "waiting" => TaskPhase::Waiting,
        "parsing" => TaskPhase::Parsing,
        "indexing" => TaskPhase::Indexing,
        "retrieving" => TaskPhase::Retrieving,
        "generating" => TaskPhase::Generating,
        "validating" => TaskPhase::Validating,
        "committing" => TaskPhase::Committing,
        _ => return None,
    })
}
fn outcome_str(o: TaskOutcome) -> &'static str {
    match o {
        TaskOutcome::None => "none",
        TaskOutcome::Answered => "answered",
        TaskOutcome::InsufficientEvidence => "insufficient_evidence",
        TaskOutcome::Conflict => "conflict",
        TaskOutcome::OutOfScope => "out_of_scope",
        TaskOutcome::Partial => "partial",
    }
}
fn parse_outcome(s: &str) -> Option<TaskOutcome> {
    Some(match s {
        "none" => TaskOutcome::None,
        "answered" => TaskOutcome::Answered,
        "insufficient_evidence" => TaskOutcome::InsufficientEvidence,
        "conflict" => TaskOutcome::Conflict,
        "out_of_scope" => TaskOutcome::OutOfScope,
        "partial" => TaskOutcome::Partial,
        _ => return None,
    })
}

fn row_to_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    let ref_kind: Option<String> = r.get("result_ref_kind")?;
    let ref_id: Option<String> = r.get("result_ref_id")?;
    Ok(TaskRow {
        id: r.get("id")?,
        document_id: r.get("document_id")?,
        task_type: parse_task_type(&r.get::<_, String>("task_type")?)
            .unwrap_or(TaskType::ParseDocument),
        idempotency_key: r.get("idempotency_key")?,
        status: parse_status(&r.get::<_, String>("status")?).unwrap_or(TaskStatus::Queued),
        phase: parse_phase(&r.get::<_, String>("phase")?).unwrap_or(TaskPhase::Waiting),
        outcome: parse_outcome(&r.get::<_, String>("outcome")?).unwrap_or(TaskOutcome::None),
        current_attempt_id: r.get("current_attempt_id")?,
        cancel_requested: r.get::<_, i64>("cancel_requested")? != 0,
        state_version: r.get("state_version")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        completed_at: r.get("completed_at")?,
        error_code: r.get("error_code")?,
        result_ref: match (ref_kind, ref_id) {
            (Some(k), Some(i)) => Some(TaskResultRef {
                kind: match k.as_str() {
                    "message" => TaskResultRefKind::Message,
                    _ => TaskResultRefKind::ArtifactVersion,
                },
                id: i,
            }),
            _ => None,
        },
        conversation_id: r.get::<_, Option<String>>("conversation_id").ok().flatten(),
        conversation_title: r
            .get::<_, Option<String>>("conversation_title")
            .ok()
            .flatten(),
    })
}

const TASK_COLS: &str =
    "id, document_id, task_type, idempotency_key, status, phase, outcome, current_attempt_id, \
     cancel_requested, state_version, created_at, updated_at, completed_at, error_code, \
     result_ref_kind, result_ref_id";

fn load_task(conn: &Connection, id: &str) -> Result<Option<TaskRow>, AppError> {
    let sql = format!("SELECT {TASK_COLS} FROM task WHERE id = ?1");
    conn.query_row(&sql, [id], row_to_task)
        .optional()
        .map_err(|e| db_app_error(&e))
}

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

/// 创建任务请求。
pub struct CreateTask {
    pub document_id: Option<String>,
    pub task_type: TaskType,
    pub idempotency_key: String,
    pub phase: TaskPhase,
}

pub enum CommitOutcome {
    Committed(Box<TaskRow>),
    /// 同一 Attempt 重复提交同一结果:返回已有状态,零额外写入。
    AlreadyCommitted(Box<TaskRow>),
}

impl std::fmt::Debug for CommitOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Committed(t) => write!(f, "Committed({:?})", t.id),
            Self::AlreadyCommitted(t) => write!(f, "AlreadyCommitted({:?})", t.id),
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct RecoveryReport {
    pub interrupted_task_ids: Vec<String>,
    pub parse_retry_candidates: Vec<String>,
    pub orphan_attempt_ids: Vec<String>,
}

pub struct TaskService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    events: Arc<dyn EventSink>,
    /// 每个活动任务一个取消令牌;终态后移除。
    cancel_tokens: Mutex<HashMap<String, CancellationToken>>,
}

impl TaskService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            events,
            cancel_tokens: Mutex::new(HashMap::new()),
        }
    }

    /// 取消令牌:执行器(解析循环、检索、模型流)监听它实现传播。
    pub fn cancel_token(&self, task_id: &str) -> CancellationToken {
        let mut map = self.cancel_tokens.lock().unwrap();
        map.entry(task_id.to_string()).or_default().clone()
    }

    fn drop_token(&self, task_id: &str) {
        self.cancel_tokens.lock().unwrap().remove(task_id);
    }

    fn emit_task(&self, task: &TaskRow) {
        let mut event = DomainEvent::new(
            DomainEventType::TaskChanged,
            task.id.clone(),
            task.state_version,
            self.clock.now_ms(),
            serde_json::to_value(task.to_view()).unwrap_or(serde_json::Value::Null),
        )
        .with_document(task.document_id.clone(), None)
        .with_task(Some(task.id.clone()));
        event.aggregate_version = task.state_version;
        self.events.emit(event);
    }

    /// 创建逻辑任务。同 idempotencyKey 重复请求返回已有 Task(不重复创建)。
    pub fn create_task(&self, req: CreateTask) -> Result<TaskRow, AppError> {
        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let key = req.idempotency_key.clone();
        let existing = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            if let Some(existing) = tx
                .query_row(
                    &format!("SELECT {TASK_COLS} FROM task WHERE idempotency_key = ?1"),
                    [&key],
                    row_to_task,
                )
                .optional()
                .map_err(|e| db_app_error(&e))?
            {
                return Ok(existing);
            }
            tx.execute(
                "INSERT INTO task(id, document_id, task_type, idempotency_key, status, phase, \
                 outcome, cancel_requested, state_version, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,'queued',?5,'none',0,1,?6,?6)",
                params![
                    id,
                    req.document_id,
                    task_type_str(req.task_type),
                    key,
                    phase_str(req.phase),
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            let created = load_task(&tx, &id).ok().flatten().unwrap();
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(created)
        })?;
        // 事件只在 COMMIT 成功后发出(DATA-09)。
        self.emit_task(&existing);
        Ok(existing)
    }

    /// 显式重试:failed/canceled/interrupted → queued。succeeded 拒绝。
    pub fn retry_task(&self, task_id: &str) -> Result<TaskRow, AppError> {
        let now = self.clock.now_ms();
        let task_id = task_id.to_string();
        let task = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let task = load_task(&tx, &task_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if !matches!(
                task.status,
                TaskStatus::Failed | TaskStatus::Canceled | TaskStatus::Interrupted
            ) {
                return Err(err(CoreErrorCode::TaskNotRunning));
            }
            tx.execute(
                "UPDATE task SET status='queued', phase='waiting', outcome='none', \
                 cancel_requested=0, state_version=state_version+1, updated_at=?2, \
                 completed_at=NULL, error_code=NULL WHERE id=?1",
                params![task_id, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let updated = load_task(&tx, &task_id)?.unwrap();
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(updated)
        })?;
        self.emit_task(&task);
        Ok(task)
    }

    /// 从 queued 进入 running 并创建新 Attempt。
    pub fn begin_attempt(
        &self,
        task_id: &str,
        provider_id: Option<String>,
        model_id: Option<String>,
    ) -> Result<(TaskRow, AttemptRow), AppError> {
        let now = self.clock.now_ms();
        let attempt_id = self.ids.new_entity_id();
        let task_id = task_id.to_string();
        let out = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let task = load_task(&tx, &task_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if task.status != TaskStatus::Queued {
                return Err(err(CoreErrorCode::TaskNotRunning));
            }
            let next_no: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(attempt_no),0)+1 FROM task_attempt WHERE task_id=?1",
                    [&task_id],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "INSERT INTO task_attempt(id, task_id, attempt_no, provider_id, model_id, \
                 status, started_at) VALUES (?1,?2,?3,?4,?5,'running',?6)",
                params![attempt_id, task_id, next_no, provider_id, model_id, now],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "UPDATE task SET status='running', current_attempt_id=?2, \
                 state_version=state_version+1, updated_at=?3 WHERE id=?1",
                params![task_id, attempt_id, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let task = load_task(&tx, &task_id)?.unwrap();
            let attempt = load_attempt(&tx, &attempt_id)?.unwrap();
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok((task, attempt))
        })?;
        self.emit_task(&out.0);
        Ok(out)
    }

    /// 阶段推进(仅 running 且当前 Attempt)。
    pub fn set_phase(
        &self,
        task_id: &str,
        attempt_id: &str,
        phase: TaskPhase,
    ) -> Result<TaskRow, AppError> {
        let now = self.clock.now_ms();
        let task_id = task_id.to_string();
        let attempt_id = attempt_id.to_string();
        let phase_s = phase_str(phase);
        let task = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let task = guard_running_attempt(&tx, &task_id, &attempt_id)?;
            let _ = task;
            tx.execute(
                "UPDATE task SET phase=?2, state_version=state_version+1, updated_at=?3 WHERE id=?1",
                params![task_id, phase_s, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let updated = load_task(&tx, &task_id)?.unwrap();
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(updated)
        })?;
        self.emit_task(&task);
        Ok(task)
    }

    /// 请求取消:queued 直接 canceled;running 置 cancel_requested 并触发令牌。
    /// 取消请求本身不是终态,执行方确认(cancel_ack)后才显示“已取消”。
    pub fn request_cancel(
        &self,
        task_id: &str,
        expected_state_version: i64,
    ) -> Result<TaskRow, AppError> {
        let now = self.clock.now_ms();
        let task_id = task_id.to_string();

        let task = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let task = load_task(&tx, &task_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if task.state_version != expected_state_version {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch));
            }
            match task.status {
                TaskStatus::Queued => {
                    tx.execute(
                        "UPDATE task SET status='canceled', cancel_requested=1, \
                         state_version=state_version+1, updated_at=?2, completed_at=?2 WHERE id=?1",
                        params![task_id, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                }
                TaskStatus::Running => {
                    tx.execute(
                        "UPDATE task SET cancel_requested=1, state_version=state_version+1, \
                         updated_at=?2 WHERE id=?1",
                        params![task_id, now],
                    )
                    .map_err(|e| db_app_error(&e))?;
                }
                _ => return Err(err(CoreErrorCode::TaskNotRunning)),
            }
            let updated = load_task(&tx, &task_id)?.unwrap();
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(updated)
        })?;

        if let Some(token) = self.cancel_tokens.lock().unwrap().get(task.id.as_str()) {
            token.cancel();
        }
        if task.status == TaskStatus::Canceled {
            self.drop_token(&task.id);
        }
        self.emit_task(&task);
        Ok(task)
    }

    /// 执行方确认取消:running + cancel_requested → canceled(终态)。
    pub fn cancel_ack(&self, task_id: &str, attempt_id: &str) -> Result<TaskRow, AppError> {
        let now = self.clock.now_ms();
        let task = self.finish_attempt(
            task_id,
            attempt_id,
            TaskStatus::Canceled,
            AttemptStatus::Canceled,
            None,
            now,
        )?;
        self.drop_token(task_id);
        self.emit_task(&task);
        Ok(task)
    }

    /// 执行失败:running → failed。
    pub fn fail_task(
        &self,
        task_id: &str,
        attempt_id: &str,
        error_code: &str,
    ) -> Result<TaskRow, AppError> {
        let now = self.clock.now_ms();
        let task = self.finish_attempt(
            task_id,
            attempt_id,
            TaskStatus::Failed,
            AttemptStatus::Failed,
            Some(error_code),
            now,
        )?;
        self.drop_token(task_id);
        self.emit_task(&task);
        Ok(task)
    }

    /// 生成编排的统一失败收口:取消错误必须落为 canceled(而不是 failed),
    /// 其他错误继续走 failed。调用方保留原始错误返回给 IPC。
    pub fn settle_generation_failure(
        &self,
        task_id: &str,
        attempt_id: &str,
        error_code: &str,
    ) -> Result<TaskRow, AppError> {
        if error_code == "TASK_CANCELED" {
            self.cancel_ack(task_id, attempt_id)
        } else {
            self.fail_task(task_id, attempt_id, error_code)
        }
    }

    fn finish_attempt(
        &self,
        task_id: &str,
        attempt_id: &str,
        task_status: TaskStatus,
        attempt_status: AttemptStatus,
        error_code: Option<&str>,
        now: i64,
    ) -> Result<TaskRow, AppError> {
        let task_id = task_id.to_string();
        let attempt_id = attempt_id.to_string();
        let status_s = status_str(task_status);
        let attempt_s = attempt_status.as_str();
        let error_code = error_code.map(|s| s.to_string());
        self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let task = guard_running_attempt(&tx, &task_id, &attempt_id)?;
            if task_status == TaskStatus::Canceled && !task.cancel_requested {
                // 无取消请求却确认取消:状态机违规。
                return Err(err(CoreErrorCode::TaskNotRunning));
            }
            tx.execute(
                "UPDATE task_attempt SET status=?2, ended_at=?3, error_code=?4 WHERE id=?1",
                params![attempt_id, attempt_s, now, error_code],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "UPDATE task SET status=?2, state_version=state_version+1, updated_at=?3, \
                 completed_at=?3, error_code=?4 WHERE id=?1",
                params![task_id, status_s, now, error_code],
            )
            .map_err(|e| db_app_error(&e))?;
            let updated = load_task(&tx, &task_id)?.unwrap();
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(updated)
        })
    }

    /// 幂等结果提交(M01 §5)。业务写入闭包与状态校验同事务;
    /// 闭包只能使用传入的 `&Connection`(同一事务),不得开嵌套事务。
    pub fn commit_result<F>(
        &self,
        task_id: &str,
        attempt_id: &str,
        result_json: serde_json::Value,
        outcome: TaskOutcome,
        result_ref: Option<TaskResultRef>,
        business_write: F,
    ) -> Result<CommitOutcome, AppError>
    where
        F: FnOnce(&Connection, &TaskRow) -> Result<(), AppError> + Send + 'static,
    {
        let now = self.clock.now_ms();
        let task_id = task_id.to_string();
        let attempt_id = attempt_id.to_string();
        let result_hash = sha256_str(&result_json.to_string());
        let result_body = result_json.to_string();
        let outcome_s = outcome_str(outcome);
        let (ref_kind_s, ref_id_s) = match &result_ref {
            Some(r) => (
                Some(match r.kind {
                    TaskResultRefKind::Message => "message",
                    TaskResultRefKind::ArtifactVersion => "artifact_version",
                }),
                Some(r.id.clone()),
            ),
            None => (None, None),
        };

        let committed = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let task = load_task(&tx, &task_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;

            // 重复提交同一结果:返回已有结果,零额外写入。
            let existing: Option<(String, i64)> = tx
                .query_row(
                    "SELECT result_hash, created_at FROM task_result WHERE task_id=?1",
                    [&task_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if let Some((hash, _)) = existing {
                if hash == result_hash {
                    let current = load_task(&tx, &task_id)?.unwrap();
                    tx.commit().map_err(|e| db_app_error(&e))?;
                    return Ok((CommitOutcome::AlreadyCommitted(Box::new(current)), false));
                }
                // 不同哈希:拒绝,不覆盖。
                return Err(err(CoreErrorCode::TaskNotRunning));
            }

            if task.status != TaskStatus::Running {
                return Err(err(CoreErrorCode::TaskNotRunning));
            }
            if task.current_attempt_id.as_deref() != Some(attempt_id.as_str()) {
                // 旧 Attempt 迟到结果:零写入。
                return Err(err(CoreErrorCode::TaskNotRunning));
            }
            if task.cancel_requested {
                return Err(err(CoreErrorCode::TaskCanceled));
            }

            // 业务写入(同一事务)。
            business_write(&tx, &task)?;

            tx.execute(
                "INSERT INTO task_result(task_id, result_json, result_hash, created_at) \
                 VALUES (?1,?2,?3,?4)",
                params![task_id, result_body, result_hash, now],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "UPDATE task SET status='succeeded', outcome=?2, completed_at=?3, \
                 state_version=state_version+1, updated_at=?3, result_ref_kind=?4, \
                 result_ref_id=?5 WHERE id=?1",
                params![task_id, outcome_s, now, ref_kind_s, ref_id_s],
            )
            .map_err(|e| db_app_error(&e))?;
            tx.execute(
                "UPDATE task_attempt SET status='succeeded', ended_at=?2 WHERE id=?1",
                params![attempt_id, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let updated = load_task(&tx, &task_id)?.unwrap();
            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok((CommitOutcome::Committed(Box::new(updated)), true))
        })?;

        let (outcome, _emitted) = committed;
        if let Some(task) = match &outcome {
            CommitOutcome::Committed(t) => Some(t.as_ref()),
            CommitOutcome::AlreadyCommitted(t) => Some(t.as_ref()),
        } {
            self.drop_token(&task.id);
            self.emit_task(task);
        }
        Ok(outcome)
    }

    /// 启动恢复(《M01》§7):活动任务 → interrupted;已取消任务保持取消;
    /// AI 任务不自动补发;解析任务可按设置列为重试候选。
    pub fn recover_interrupted_tasks(
        &self,
        parse_auto_retry_enabled: bool,
    ) -> Result<RecoveryReport, AppError> {
        let now = self.clock.now_ms();
        let report = self.db.write(move |conn| {
            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;

            // 1) running/queued 任务 → interrupted;对应 running attempt → interrupted。
            let mut stmt = tx
                .prepare(&format!(
                    "SELECT {TASK_COLS} FROM task WHERE status IN ('running','queued')"
                ))
                .map_err(|e| db_app_error(&e))?;
            let active: Vec<TaskRow> = stmt
                .query_map([], row_to_task)
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);

            let mut interrupted = Vec::new();
            let mut parse_candidates = Vec::new();
            for t in &active {
                tx.execute(
                    "UPDATE task SET status='interrupted', state_version=state_version+1, \
                     updated_at=?2, error_code='TASK_INTERRUPTED' WHERE id=?1",
                    params![t.id, now],
                )
                .map_err(|e| db_app_error(&e))?;
                tx.execute(
                    "UPDATE task_attempt SET status='interrupted', ended_at=?2 \
                     WHERE task_id=?1 AND status='running'",
                    params![t.id, now],
                )
                .map_err(|e| db_app_error(&e))?;
                interrupted.push(t.id.clone());
                if t.task_type == TaskType::ParseDocument && parse_auto_retry_enabled {
                    parse_candidates.push(t.id.clone());
                }
            }

            // 2) succeeded 但无 TaskResult:不完整,降级 interrupted(M01 §8)。
            let mut stmt = tx
                .prepare(&format!(
                    "SELECT {TASK_COLS} FROM task t WHERE status='succeeded' \
                     AND NOT EXISTS (SELECT 1 FROM task_result r WHERE r.task_id = t.id)"
                ))
                .map_err(|e| db_app_error(&e))?;
            let incomplete: Vec<TaskRow> = stmt
                .query_map([], row_to_task)
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            for t in &incomplete {
                tx.execute(
                    "UPDATE task SET status='interrupted', state_version=state_version+1, \
                     updated_at=?2, error_code='TASK_INTERRUPTED' WHERE id=?1",
                    params![t.id, now],
                )
                .map_err(|e| db_app_error(&e))?;
                interrupted.push(t.id.clone());
            }

            // 3) 无主 running attempt(任务非 running):报告为数据异常并修复。
            let mut stmt = tx
                .prepare(
                    "SELECT a.id FROM task_attempt a JOIN task t ON t.id = a.task_id \
                     WHERE a.status='running' AND t.status != 'running'",
                )
                .map_err(|e| db_app_error(&e))?;
            let orphan: Vec<String> = stmt
                .query_map([], |r| r.get(0))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            for aid in &orphan {
                tx.execute(
                    "UPDATE task_attempt SET status='interrupted', ended_at=?2 WHERE id=?1",
                    params![aid, now],
                )
                .map_err(|e| db_app_error(&e))?;
            }

            tx.commit().map_err(|e| db_app_error(&e))?;
            Ok(RecoveryReport {
                interrupted_task_ids: interrupted,
                parse_retry_candidates: parse_candidates,
                orphan_attempt_ids: orphan,
            })
        })?;
        Ok(report)
    }

    pub fn get_task(&self, task_id: &str) -> Result<Option<TaskRow>, AppError> {
        let id = task_id.to_string();
        self.db.read(move |conn| load_task(conn, &id))
    }

    /// 删除任务(已完成、失败、已取消或已中断的任务方可删除;运行中的任务拒绝删除)。
    pub fn delete_task(&self, task_id: &str) -> Result<crate::shared::dto::DeleteResult, AppError> {
        let task_id_str = task_id.to_string();
        self.db.write(move |conn| {
            let existing = load_task(conn, &task_id_str)?;
            let task = match existing {
                Some(t) => t,
                None => {
                    return Err(err(CoreErrorCode::NotFound)
                        .with_detail("taskId", task_id_str.clone().into()))
                }
            };
            if task.status == TaskStatus::Running || task.status == TaskStatus::Queued {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "task_is_running".into())
                    .with_detail("taskId", task_id_str.clone().into()));
            }

            let tx = conn.transaction().map_err(|e| db_app_error(&e))?;
            let _ = tx.execute("UPDATE message SET task_id = NULL WHERE task_id = ?1", [&task_id_str]);
            let _ = tx.execute("UPDATE artifact SET task_id = NULL WHERE task_id = ?1", [&task_id_str]);
            let _ = tx.execute("UPDATE parse_generation SET task_id = NULL WHERE task_id = ?1", [&task_id_str]);
            let _ = tx.execute("UPDATE data_clear_audit SET task_id = NULL WHERE task_id = ?1", [&task_id_str]);
            let _ = tx.execute(
                "DELETE FROM agent_step WHERE run_id IN (SELECT id FROM agent_run WHERE task_id=?1)",
                [&task_id_str],
            );
            let _ = tx.execute("DELETE FROM agent_run WHERE task_id=?1", [&task_id_str]);
            let _ = tx.execute(
                "DELETE FROM retrieval_candidate WHERE run_id IN (SELECT id FROM retrieval_run WHERE task_id=?1)",
                [&task_id_str],
            );
            let _ = tx.execute("DELETE FROM retrieval_run WHERE task_id=?1", [&task_id_str]);
            tx.execute("DELETE FROM task_result WHERE task_id=?1", [&task_id_str])
                .map_err(|e| db_app_error(&e))?;
            tx.execute("DELETE FROM task_attempt WHERE task_id=?1", [&task_id_str])
                .map_err(|e| db_app_error(&e))?;
            tx.execute("DELETE FROM task WHERE id=?1", [&task_id_str])
                .map_err(|e| db_app_error(&e))?;
            tx.commit().map_err(|e| db_app_error(&e))?;

            Ok(crate::shared::dto::DeleteResult {
                deleted: true,
                id: task_id_str,
            })
        })?;

        self.drop_token(task_id);

        Ok(crate::shared::dto::DeleteResult {
            deleted: true,
            id: task_id.to_string(),
        })
    }

    /// 任务列表(状态恢复与任务中心)。
    pub fn list_tasks(
        &self,
        document_id: Option<String>,
        active_only: bool,
        limit: i64,
        cursor: Option<String>,
    ) -> Result<crate::shared::paging::Page<TaskView>, AppError> {
        use rusqlite::types::Value as SqlValue;
        self.db.read(move |conn| {
            // 一次查询补会话归属:问答/研究任务通过消息关联会话;
            // 产物/解析任务没有会话消息,保持 None(前端归入“未关联对话”)。
            let mut sql = format!(
                "SELECT {TASK_COLS},                  (SELECT m.conversation_id FROM message m WHERE m.task_id = task.id                   ORDER BY m.created_at ASC, m.id ASC LIMIT 1) AS conversation_id,                  (SELECT c.title FROM message m JOIN conversation c ON c.id = m.conversation_id                   WHERE m.task_id = task.id ORDER BY m.created_at ASC, m.id ASC LIMIT 1)                   AS conversation_title                  FROM task WHERE 1=1"
            );
            let mut binds: Vec<SqlValue> = Vec::new();
            if let Some(doc) = &document_id {
                sql.push_str(&format!(" AND document_id = ?{}", binds.len() + 1));
                binds.push(SqlValue::Text(doc.clone()));
            }
            if active_only {
                sql.push_str(" AND status IN ('queued','running')");
            }
            if let Some(cur) = &cursor {
                // 游标基于 (updated_at, id),保持分页稳定。
                sql.push_str(&format!(
                    " AND (updated_at, id) < (?{}, ?{})",
                    binds.len() + 1,
                    binds.len() + 2
                ));
                let (ts, id): (i64, String) = crate::shared::paging::decode_cursor(cur)
                    .ok_or_else(|| err(CoreErrorCode::InvalidRequest))?;
                binds.push(SqlValue::Integer(ts));
                binds.push(SqlValue::Text(id));
            }
            sql.push_str(&format!(
                " ORDER BY updated_at DESC, id DESC LIMIT ?{}",
                binds.len() + 1
            ));
            binds.push(SqlValue::Integer(limit + 1));

            let mut stmt = conn.prepare(&sql).map_err(|e| db_app_error(&e))?;
            let rows: Vec<TaskRow> = stmt
                .query_map(rusqlite::params_from_iter(binds.iter()), row_to_task)
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            let page = crate::shared::paging::Page::from_probe(rows, limit, |t| {
                crate::shared::paging::encode_cursor(&(t.updated_at, t.id.clone()))
            });
            Ok(crate::shared::paging::Page {
                items: page.items.iter().map(|t| t.to_view()).collect(),
                next_cursor: page.next_cursor,
                total: None,
            })
        })
    }
}

fn load_attempt(conn: &Connection, id: &str) -> Result<Option<AttemptRow>, AppError> {
    conn.query_row(
        "SELECT id, task_id, attempt_no, provider_id, model_id, status, started_at, ended_at, \
         error_code FROM task_attempt WHERE id=?1",
        [id],
        |r| {
            Ok(AttemptRow {
                id: r.get(0)?,
                task_id: r.get(1)?,
                attempt_no: r.get(2)?,
                provider_id: r.get(3)?,
                model_id: r.get(4)?,
                status: AttemptStatus::from_str(&r.get::<_, String>(5)?)
                    .unwrap_or(AttemptStatus::Interrupted),
                started_at: r.get(6)?,
                ended_at: r.get(7)?,
                error_code: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

/// 校验:任务处于 running 且指定 Attempt 是当前 Attempt。
fn guard_running_attempt(
    conn: &Connection,
    task_id: &str,
    attempt_id: &str,
) -> Result<TaskRow, AppError> {
    let task = load_task(conn, task_id)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
    if task.status != TaskStatus::Running {
        return Err(err(CoreErrorCode::TaskNotRunning));
    }
    if task.current_attempt_id.as_deref() != Some(attempt_id) {
        return Err(err(CoreErrorCode::TaskNotRunning));
    }
    Ok(task)
}
