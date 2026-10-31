//! M11 理解校验题服务(《M11》§4–§8)。
//!
//! 关键不变量:
//! - 题目内容本体归 M10:本模块经 `QuizArtifactSource` 端口读取 quiz.v1
//!   版本,不直接读写 artifact/artifact_version 表;端口在 M10 落地后
//!   由 M13 接线,未接线时命令返回明确错误而非假数据。
//! - 作答只呈现事实:同一题重做新增 attempt,保留全部历史;
//!   正确率只统计 validity=valid 的题,无效题即使答错也不计入用户错误。
//! - 反馈独立于作答;不静默修改 AI 原版,只有 accepted 触发重生成建议
//!   (重新生成仍由 M10 创建新草稿版本,历史版本与作答不变)。
//! - 乱序选项或重新生成是新版本:旧作答仍引用旧版本,不消失、不映射。
//! - 事件(userdata.changed)只在 COMMIT 成功后发出。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rusqlite::{params, Connection, OptionalExtension};

use crate::persistence::{db_app_error, Db};
use crate::shared::events::{DomainEvent, DomainEventType, EventSink};
use crate::shared::{AppError, Clock, CoreErrorCode, IdGenerator};

use super::models::*;
use super::validation::{parse_quiz_content, quiz_question_invalid, validate_feedback_comment};

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

fn invalid(reason: &str) -> AppError {
    err(CoreErrorCode::InvalidRequest).with_detail("reason", reason.into())
}

/// M10 未接线前的生产占位端口:明确报错,不返回假版本。
pub struct UnwiredQuizArtifactSource;

impl QuizArtifactSource for UnwiredQuizArtifactSource {
    fn load_quiz_version(
        &self,
        _artifact_version_id: &str,
    ) -> Result<Option<QuizArtifactVersion>, AppError> {
        Err(invalid("quiz_artifact_source_unwired")
            .with_detail("hint", "M10 接入后由 M13 注册 QuizArtifactSource".into()))
    }
}

// —— 事务内行加载(仅 M11 自有表)——

fn load_record_by_key_tx(
    conn: &Connection,
    key: &str,
) -> Result<Option<ComprehensionRecordRow>, AppError> {
    conn.query_row(
        &format!("SELECT {RECORD_COLS} FROM comprehension_record WHERE idempotency_key=?1"),
        [key],
        ComprehensionRecordRow::from_row,
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_record_by_id_tx(
    conn: &Connection,
    id: &str,
) -> Result<Option<ComprehensionRecordRow>, AppError> {
    conn.query_row(
        &format!("SELECT {RECORD_COLS} FROM comprehension_record WHERE id=?1"),
        [id],
        ComprehensionRecordRow::from_row,
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_feedback_by_key_tx(
    conn: &Connection,
    key: &str,
) -> Result<Option<QuestionFeedbackRow>, AppError> {
    conn.query_row(
        &format!("SELECT {FEEDBACK_COLS} FROM quiz_question_feedback WHERE idempotency_key=?1"),
        [key],
        QuestionFeedbackRow::from_row,
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_feedback_tx(conn: &Connection, id: &str) -> Result<Option<QuestionFeedbackRow>, AppError> {
    conn.query_row(
        &format!("SELECT {FEEDBACK_COLS} FROM quiz_question_feedback WHERE id=?1"),
        [id],
        QuestionFeedbackRow::from_row,
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn next_attempt_no_tx(
    conn: &Connection,
    version_id: &str,
    question_id: &str,
) -> Result<i64, AppError> {
    // MAX(attempt_no) 在无历史行时返回 NULL:先按可空列读取再展平。
    let max: Option<Option<i64>> = conn
        .query_row(
            "SELECT MAX(attempt_no) FROM comprehension_record \
             WHERE artifact_version_id=?1 AND question_id=?2",
            params![version_id, question_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    Ok(max.flatten().unwrap_or(0) + 1)
}

/// 答题时刻该题最近一次 accepted 反馈类型(呈现上下文,可空)。
fn latest_accepted_feedback_type_tx(
    conn: &Connection,
    version_id: &str,
    question_id: &str,
) -> Result<Option<QuizFeedbackType>, AppError> {
    let ft: Option<String> = conn
        .query_row(
            "SELECT feedback_type FROM quiz_question_feedback \
             WHERE artifact_version_id=?1 AND question_id=?2 AND status='accepted' \
             ORDER BY updated_at DESC, id DESC LIMIT 1",
            params![version_id, question_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    Ok(ft.as_deref().and_then(QuizFeedbackType::parse_db))
}

fn records_for_version_tx(
    conn: &Connection,
    version_id: &str,
) -> Result<Vec<ComprehensionRecordRow>, AppError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {RECORD_COLS} FROM comprehension_record \
             WHERE artifact_version_id=?1 ORDER BY question_id, attempt_no"
        ))
        .map_err(|e| db_app_error(&e))?;
    let rows = stmt
        .query_map([version_id], ComprehensionRecordRow::from_row)
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_app_error(&e))?;
    Ok(rows)
}

fn feedback_for_version_tx(
    conn: &Connection,
    version_id: &str,
) -> Result<Vec<QuestionFeedbackRow>, AppError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {FEEDBACK_COLS} FROM quiz_question_feedback \
             WHERE artifact_version_id=?1 ORDER BY created_at, id"
        ))
        .map_err(|e| db_app_error(&e))?;
    let rows = stmt
        .query_map([version_id], QuestionFeedbackRow::from_row)
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_app_error(&e))?;
    Ok(rows)
}

// —— 服务 ——

pub struct QuizService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    events: Arc<dyn EventSink>,
    artifact_source: RwLock<Arc<dyn QuizArtifactSource>>,
}

impl QuizService {
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
            // M10 落地前生产路径明确报错;M13 接线真实 ArtifactService 投影。
            artifact_source: RwLock::new(Arc::new(UnwiredQuizArtifactSource)),
        }
    }

    /// M13 在 M10 落地后注册真实版本读取端口。
    pub fn register_artifact_source(&self, source: Arc<dyn QuizArtifactSource>) {
        *self.artifact_source.write().unwrap() = source;
    }

    fn source(&self) -> Arc<dyn QuizArtifactSource> {
        self.artifact_source.read().unwrap().clone()
    }

    /// 读取 quiz.v1 版本与解析后的内容。
    fn load_content(
        &self,
        artifact_version_id: &str,
    ) -> Result<(QuizArtifactVersion, QuizContentV1), AppError> {
        let version = self
            .source()
            .load_quiz_version(artifact_version_id)?
            .ok_or_else(|| err(CoreErrorCode::NotFound))?;
        if version.schema_version != QUIZ_SCHEMA_VERSION {
            return Err(invalid("artifact_not_quiz_schema")
                .with_detail("expected", QUIZ_SCHEMA_VERSION.into())
                .with_detail("actual", version.schema_version.clone().into()));
        }
        let content = parse_quiz_content(&version.content_json).map_err(|e| {
            quiz_question_invalid("saved_content_invalid")
                .with_detail("cause", e.code.clone().into())
        })?;
        Ok((version, content))
    }

    fn emit_userdata(
        &self,
        entity_type: &str,
        entity_id: &str,
        action: &str,
        revision: i64,
        document_id: &str,
    ) {
        let event = DomainEvent::new(
            DomainEventType::UserdataChanged,
            entity_id.to_string(),
            revision,
            self.clock.now_ms(),
            serde_json::json!({
                "entityType": entity_type,
                "entityId": entity_id,
                "action": action,
                "revision": revision,
            }),
        )
        .with_document(Some(document_id.to_string()), None);
        self.events.emit(event);
    }

    // —— 作答(《M11》§5)——

    /// `quiz_submit_answer`:作答创建 ComprehensionRecord;幂等。
    /// 无效题也可作答(呈现事实),但正确率统计只取 validity=valid。
    pub fn submit_answer(
        &self,
        input: &QuizAnswerInput,
        idempotency_key: &str,
    ) -> Result<ComprehensionRecordView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        // 预检幂等(读路径):重复送达直接返回既有记录,
        // 即使产物版本此后已被删除,重放仍成立。
        let key = idempotency_key.to_string();
        let pre_key = key.clone();
        if let Some(row) = self
            .db
            .read(move |conn| load_record_by_key_tx(conn, &pre_key))?
        {
            return Ok(row.to_view());
        }
        // AI 原版与封存版本不可变,读取结果在提交窗口内稳定。
        // 产物版本读取经端口,不入写事务(避免跨服务连接互锁)。
        let (_, content) = self.load_content(&input.artifact_version_id)?;
        let question = content
            .questions
            .iter()
            .find(|q| q.id == input.question_id)
            .ok_or_else(|| invalid("question_not_in_version"))?;
        if !question
            .options
            .iter()
            .any(|o| o.id == input.selected_option_id)
        {
            return Err(invalid("selected_option_unknown"));
        }
        let question_validity = question.validity;
        let correct = question.correct_option_id.clone();

        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let input2 = input.clone();
        let view = self.db.write(move |conn| {
            // 事务内复核幂等,先于一切校验(与预检窗口竞争兜底)。
            if let Some(row) = load_record_by_key_tx(conn, &key)? {
                return Ok(row.to_view());
            }
            let next = next_attempt_no_tx(conn, &input2.artifact_version_id, &input2.question_id)?;
            if let Some(expected) = input2.attempt_no {
                if expected != next {
                    return Err(invalid("attempt_no_conflict")
                        .with_detail("expectedAttemptNo", next.into())
                        .with_detail("actualAttemptNo", expected.into()));
                }
            }
            let feedback_type = latest_accepted_feedback_type_tx(
                conn,
                &input2.artifact_version_id,
                &input2.question_id,
            )?;
            let is_correct = input2.selected_option_id == correct;
            let inserted = conn
                .execute(
                    "INSERT INTO comprehension_record(id, artifact_version_id, question_id, \
                     attempt_no, selected_option_id, is_correct, validity_status, feedback_type, \
                     idempotency_key, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![
                        id,
                        input2.artifact_version_id,
                        input2.question_id,
                        next,
                        input2.selected_option_id,
                        is_correct as i64,
                        question_validity.as_str(),
                        feedback_type.map(|f| f.as_str()),
                        key,
                        now
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
            if inserted == 0 {
                return Err(err(CoreErrorCode::Internal));
            }
            let row =
                load_record_by_id_tx(conn, &id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            Ok(row.to_view())
        })?;
        // documentId 来自版本归属;作答事件只带实体标识,不带正文。
        let document_id = self.document_id_of(&input.artifact_version_id)?;
        self.emit_userdata(
            "comprehension_record",
            &view.id,
            "created",
            view.attempt_no,
            &document_id,
        );
        Ok(view)
    }

    /// 版本归属文档(事件信封用):经端口再读一次元数据投影。
    fn document_id_of(&self, artifact_version_id: &str) -> Result<String, AppError> {
        Ok(self
            .source()
            .load_quiz_version(artifact_version_id)?
            .ok_or_else(|| err(CoreErrorCode::NotFound))?
            .document_id)
    }

    /// `quiz_list_records`:按版本(可按题)返回全部尝试,attemptNo 升序。
    pub fn list_records(
        &self,
        artifact_version_id: &str,
        question_id: Option<&str>,
    ) -> Result<Vec<ComprehensionRecordView>, AppError> {
        let vid = artifact_version_id.to_string();
        let qid = question_id.map(|s| s.to_string());
        self.db.read(move |conn| {
            let mut rows = records_for_version_tx(conn, &vid)?;
            if let Some(q) = &qid {
                rows.retain(|r| &r.question_id == q);
            }
            Ok(rows.iter().map(|r| r.to_view()).collect())
        })
    }

    // —— 无效题反馈(《M11》§4)——

    /// `quiz_feedback`:未作答题也可提交;写入 open 状态,不改 AI 原版。
    pub fn submit_feedback(
        &self,
        input: &QuizFeedbackInput,
        idempotency_key: &str,
    ) -> Result<QuestionFeedbackView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        validate_feedback_comment(&input.comment)?;
        let key = idempotency_key.to_string();
        let pre_key = key.clone();
        if let Some(row) = self
            .db
            .read(move |conn| load_feedback_by_key_tx(conn, &pre_key))?
        {
            return Ok(row.to_view());
        }
        // question_id 必须存在于对应版本内容(《M11》§4 字段规则)。
        let (_, content) = self.load_content(&input.artifact_version_id)?;
        if !content.questions.iter().any(|q| q.id == input.question_id) {
            return Err(invalid("question_not_in_version"));
        }

        let now = self.clock.now_ms();
        let id = self.ids.new_entity_id();
        let input2 = input.clone();
        let view = self.db.write(move |conn| {
            if let Some(row) = load_feedback_by_key_tx(conn, &key)? {
                return Ok(row.to_view());
            }
            conn.execute(
                "INSERT INTO quiz_question_feedback(id, artifact_version_id, question_id, \
                 feedback_type, comment, status, revision, idempotency_key, created_at, \
                 updated_at) VALUES (?1,?2,?3,?4,?5,'open',1,?6,?7,?7)",
                params![
                    id,
                    input2.artifact_version_id,
                    input2.question_id,
                    input2.feedback_type.as_str(),
                    input2.comment,
                    key,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            let row =
                load_feedback_by_key_tx(conn, &key)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            Ok(row.to_view())
        })?;
        let document_id = self.document_id_of(&input.artifact_version_id)?;
        self.emit_userdata(
            "quiz_feedback",
            &view.id,
            "created",
            view.revision,
            &document_id,
        );
        Ok(view)
    }

    /// `quiz_feedback_update`:状态流转;只有 accepted 触发重生成建议,
    /// 历史题目版本与作答记录不变。
    pub fn update_feedback(
        &self,
        input: &QuizFeedbackUpdateInput,
    ) -> Result<QuestionFeedbackView, AppError> {
        let now = self.clock.now_ms();
        let fid = input.feedback_id.clone();
        let status = input.status.as_str().to_string();
        let expected = input.expected_revision;
        let view = self.db.write(move |conn| {
            let row = load_feedback_tx(conn, &fid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.revision != expected {
                return Err(err(CoreErrorCode::ConflictRevisionMismatch));
            }
            conn.execute(
                "UPDATE quiz_question_feedback SET status=?2, revision=revision+1, updated_at=?3 \
                 WHERE id=?1 AND revision=?4",
                params![fid, status, now, expected],
            )
            .map_err(|e| db_app_error(&e))?;
            load_feedback_tx(conn, &fid)?
                .ok_or_else(|| err(CoreErrorCode::Internal))
                .map(|r| r.to_view())
        })?;
        let document_id = self.document_id_of(&view.artifact_version_id)?;
        self.emit_userdata(
            "quiz_feedback",
            &view.id,
            "updated",
            view.revision,
            &document_id,
        );
        Ok(view)
    }

    /// `quiz_list_feedback`:按版本(可按题)返回反馈,创建时间倒序。
    pub fn list_feedback(
        &self,
        artifact_version_id: &str,
        question_id: Option<&str>,
    ) -> Result<Vec<QuestionFeedbackView>, AppError> {
        let vid = artifact_version_id.to_string();
        let qid = question_id.map(|s| s.to_string());
        self.db.read(move |conn| {
            let mut rows = feedback_for_version_tx(conn, &vid)?;
            if let Some(q) = &qid {
                rows.retain(|r| &r.question_id == q);
            }
            let mut views: Vec<QuestionFeedbackView> = rows.iter().map(|r| r.to_view()).collect();
            views.reverse(); // created_at 升序读取,展示倒序
            Ok(views)
        })
    }

    // —— 版本聚合(《M11》§7 读模型)——

    /// `quiz_version_summary`:题目中心读模型。未作答的有效题不揭示
    /// 答案与解析;无效/争议题携带完整依据以显示"此题无效"。
    pub fn version_summary(
        &self,
        artifact_version_id: &str,
    ) -> Result<QuizVersionSummary, AppError> {
        let (version, content) = self.load_content(artifact_version_id)?;
        let vid = version.id.clone();
        let (records, feedback) = self.db.read(move |conn| {
            Ok((
                records_for_version_tx(conn, &vid)?,
                feedback_for_version_tx(conn, &vid)?,
            ))
        })?;

        let mut records_by_question: HashMap<String, Vec<ComprehensionRecordView>> = HashMap::new();
        for r in records {
            records_by_question
                .entry(r.question_id.clone())
                .or_default()
                .push(r.to_view());
        }
        let mut accepted_by_question: HashMap<String, QuestionFeedbackView> = HashMap::new();
        let mut open_counts: HashMap<String, i64> = HashMap::new();
        for f in feedback {
            if f.status == QuizFeedbackStatus::Accepted {
                // 多条 accepted 取最新(updated_at 最大;同刻按 id)。
                let entry = accepted_by_question.get(&f.question_id);
                let replace = match entry {
                    Some(prev) => (f.updated_at, f.id.clone()) > (prev.updated_at, prev.id.clone()),
                    None => true,
                };
                if replace {
                    accepted_by_question.insert(f.question_id.clone(), f.to_view());
                }
            } else if f.status == QuizFeedbackStatus::Open {
                *open_counts.entry(f.question_id.clone()).or_insert(0) += 1;
            }
        }

        let mut questions = Vec::with_capacity(content.questions.len());
        let mut stats = QuizStats {
            total_questions: content.questions.len() as i64,
            valid_questions: 0,
            invalid_questions: 0,
            answered_questions: 0,
            answered_valid_questions: 0,
            correct_valid_questions: 0,
            accuracy: None,
        };
        for q in &content.questions {
            let attempts = records_by_question.remove(&q.id).unwrap_or_default();
            let latest = attempts.last().cloned();
            let answered = !attempts.is_empty();
            // 揭示条件:已作答,或题目无效/有争议(需展示完整依据)。
            let revealed = answered || q.validity != QuestionValidity::Valid;
            if q.validity == QuestionValidity::Valid {
                stats.valid_questions += 1;
                if answered {
                    stats.answered_valid_questions += 1;
                    if latest.as_ref().is_some_and(|a| a.is_correct) {
                        stats.correct_valid_questions += 1;
                    }
                }
            } else if q.validity == QuestionValidity::Invalid {
                stats.invalid_questions += 1;
            }
            // unverified/disputed 既不计有效也不计无效:不参与计分统计。
            if answered {
                stats.answered_questions += 1;
            }
            questions.push(QuizQuestionSummary {
                question_id: q.id.clone(),
                prompt: q.prompt.clone(),
                options: q.options.clone(),
                difficulty: q.difficulty,
                validity: q.validity,
                invalid_reason: q.invalid_reason,
                explanation: revealed.then(|| QuizQuestionExplanation {
                    segments: q.explanation_segments.clone(),
                    citations: q.citations.clone(),
                }),
                correct_option_id: revealed.then(|| q.correct_option_id.clone()),
                attempts,
                latest_attempt: latest,
                accepted_feedback: accepted_by_question.remove(&q.id),
                open_feedback_count: open_counts.get(&q.id).copied().unwrap_or(0),
            });
        }
        if stats.answered_valid_questions > 0 {
            stats.accuracy =
                Some(stats.correct_valid_questions as f64 / stats.answered_valid_questions as f64);
        }
        Ok(QuizVersionSummary {
            artifact_version_id: version.id,
            artifact_id: version.artifact_id,
            document_id: version.document_id,
            schema_version: version.schema_version,
            questions,
            stats,
        })
    }

    // —— 数据清理(《M11》§8;M10 删除预览与 M08 清除的输入)——

    /// 版本集合的作答记录数(删除 Artifact 的影响预览)。
    pub fn count_records_for_versions(&self, version_ids: &[String]) -> Result<i64, AppError> {
        if version_ids.is_empty() {
            return Ok(0);
        }
        let ids = version_ids.to_vec();
        self.db
            .read(move |conn| count_in_versions_tx(conn, "comprehension_record", &ids))
    }

    /// 删除版本集合的作答记录(用户明确删除后调用;Task 与错误审计
    /// 保留非内容事实,由 M01/M10 负责)。
    pub fn clear_records_for_versions(&self, version_ids: &[String]) -> Result<i64, AppError> {
        if version_ids.is_empty() {
            return Ok(0);
        }
        let ids = version_ids.to_vec();
        self.db
            .write(move |conn| delete_in_versions_tx(conn, "comprehension_record", &ids))
    }

    /// 删除版本集合的题目反馈。
    pub fn clear_feedback_for_versions(&self, version_ids: &[String]) -> Result<i64, AppError> {
        if version_ids.is_empty() {
            return Ok(0);
        }
        let ids = version_ids.to_vec();
        self.db
            .write(move |conn| delete_in_versions_tx(conn, "quiz_question_feedback", &ids))
    }
}

/// 版本集合按 IN 列表的受控计数/删除辅助(参数化绑定,不拼接 ID 值)。
fn count_in_versions_tx(
    conn: &Connection,
    table: &str,
    version_ids: &[String],
) -> Result<i64, AppError> {
    let placeholders = version_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE artifact_version_id IN ({placeholders})"),
        rusqlite::params_from_iter(version_ids.iter()),
        |r| r.get(0),
    )
    .map_err(|e| db_app_error(&e))
}

fn delete_in_versions_tx(
    conn: &Connection,
    table: &str,
    version_ids: &[String],
) -> Result<i64, AppError> {
    let placeholders = version_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    conn.execute(
        &format!("DELETE FROM {table} WHERE artifact_version_id IN ({placeholders})"),
        rusqlite::params_from_iter(version_ids.iter()),
    )
    .map_err(|e| db_app_error(&e))
    .map(|n| n as i64)
}
