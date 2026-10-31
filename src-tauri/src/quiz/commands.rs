//! M11 命令层(薄封装,`desktop` feature,契约见《02》§3.3)。
//!
//! `quiz_submit_answer`/`quiz_feedback` 是契约命令;`quiz_feedback_update`、
//! `quiz_list_records`、`quiz_list_feedback`、`quiz_version_summary` 是
//! 《M11》§4/§5/§7 要求的补充命令(先例:M06 分册补充命令)。

use rusqlite::OptionalExtension;
use serde::Deserialize;

use super::models::*;
use crate::shared::AppError;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizSubmitAnswerRequest {
    #[serde(flatten)]
    pub input: QuizAnswerInput,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn quiz_submit_answer(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: QuizSubmitAnswerRequest,
) -> Result<ComprehensionRecordView, AppError> {
    state
        .quiz
        .submit_answer(&request.input, &request.idempotency_key)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizFeedbackRequest {
    #[serde(flatten)]
    pub input: QuizFeedbackInput,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn quiz_feedback(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: QuizFeedbackRequest,
) -> Result<QuestionFeedbackView, AppError> {
    state
        .quiz
        .submit_feedback(&request.input, &request.idempotency_key)
}

#[tauri::command]
pub async fn quiz_feedback_update(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: QuizFeedbackUpdateInput,
) -> Result<QuestionFeedbackView, AppError> {
    state.quiz.update_feedback(&request)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizListRecordsRequest {
    pub artifact_version_id: String,
    #[serde(default)]
    pub question_id: Option<String>,
}

#[tauri::command]
pub async fn quiz_list_records(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: QuizListRecordsRequest,
) -> Result<Vec<ComprehensionRecordView>, AppError> {
    state
        .quiz
        .list_records(&request.artifact_version_id, request.question_id.as_deref())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizListFeedbackRequest {
    pub artifact_version_id: String,
    #[serde(default)]
    pub question_id: Option<String>,
}

#[tauri::command]
pub async fn quiz_list_feedback(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: QuizListFeedbackRequest,
) -> Result<Vec<QuestionFeedbackView>, AppError> {
    state
        .quiz
        .list_feedback(&request.artifact_version_id, request.question_id.as_deref())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizVersionSummaryRequest {
    pub artifact_version_id: String,
}

#[tauri::command]
pub async fn quiz_version_summary(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: QuizVersionSummaryRequest,
) -> Result<QuizVersionSummary, AppError> {
    state.quiz.version_summary(&request.artifact_version_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_deserialize_camel_case() {
        let req: QuizSubmitAnswerRequest = serde_json::from_str(
            r#"{"artifactVersionId":"av1","questionId":"q1","selectedOptionId":"a",
                "attemptNo":1,"idempotencyKey":"k1"}"#,
        )
        .unwrap();
        assert_eq!(req.input.artifact_version_id, "av1");
        assert_eq!(req.input.attempt_no, Some(1));
        assert_eq!(req.idempotency_key, "k1");

        let req: QuizSubmitAnswerRequest = serde_json::from_str(
            r#"{"artifactVersionId":"av1","questionId":"q1","selectedOptionId":"a",
                "idempotencyKey":"k2"}"#,
        )
        .unwrap();
        assert_eq!(req.input.attempt_no, None);
    }
}

// —— 2B quiz.v2 作答(确定性判分;问答为自评 rubric)——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizV2SubmitRequest {
    pub artifact_version_id: String,
    pub question_id: String,
    #[serde(default)]
    pub selected_option_ids: Vec<String>,
    #[serde(default)]
    pub blank_answers: Vec<String>,
    #[serde(default)]
    pub answer_text: Option<String>,
    pub idempotency_key: String,
}

#[tauri::command]
pub async fn quiz_submit_answer_v2(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: QuizV2SubmitRequest,
) -> Result<super::v2::QuizV2GradeResult, AppError> {
    use super::v2::{grade_v2, parse_quiz_v2, GradeInput, QuizGradingMode};
    let Some(version) = state.artifacts.find_version(&request.artifact_version_id)? else {
        return Err(AppError::new(
            crate::shared::CoreErrorCode::NotFound,
            crate::shared::new_trace_id(),
        ));
    };
    if version.schema_version != super::v2::QUIZ_V2_SCHEMA_VERSION {
        return Err(AppError::new(
            crate::shared::CoreErrorCode::InvalidRequest,
            crate::shared::new_trace_id(),
        )
        .with_detail("reason", "not_quiz_v2".into()));
    }
    let content = parse_quiz_v2(&version.content_json)?;
    let question = content
        .questions
        .iter()
        .find(|q| q.id == request.question_id)
        .ok_or_else(|| {
            AppError::new(
                crate::shared::CoreErrorCode::NotFound,
                crate::shared::new_trace_id(),
            )
        })?;
    if question.validity != super::models::QuestionValidity::Valid {
        return Err(super::validation::quiz_question_invalid("question_invalid"));
    }
    let mut result = grade_v2(
        question,
        &GradeInput {
            selected_option_ids: &request.selected_option_ids,
            blank_answers: &request.blank_answers,
            answer_text: request.answer_text.as_deref(),
        },
    );
    let now = state.core.clock.now_ms();
    let attempt_id = state.core.ids.new_entity_id();
    let selected_json = serde_json::to_string(&request.selected_option_ids)
        .map_err(|e| AppError::internal(e.to_string()))?;
    let blanks_json = serde_json::to_string(&request.blank_answers)
        .map_err(|e| AppError::internal(e.to_string()))?;
    let grading = match result.grading {
        QuizGradingMode::Auto => "auto",
        QuizGradingMode::SelfAssessed => "self",
    }
    .to_string();
    let idem = request.idempotency_key.clone();
    let record = (
        attempt_id.clone(),
        request.artifact_version_id.clone(),
        request.question_id.clone(),
        selected_json,
        blanks_json,
        request.answer_text.clone(),
        result.is_correct,
        result.score,
        result.max_score,
        grading,
        now,
        idem,
    );
    let existing_id: Option<String> = state.db.write(move |conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM quiz_v2_attempt WHERE idempotency_key=?1",
                [&record.11],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| crate::persistence::db_app_error(&e))?;
        if let Some(existing) = existing {
            return Ok(Some(existing));
        }
        conn.execute(
            "INSERT INTO quiz_v2_attempt
                (id, artifact_version_id, question_id, selected_option_ids_json,
                 blank_answers_json, answer_text, is_correct, score, max_score,
                 grading, created_at, idempotency_key)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                record.0,
                record.1,
                record.2,
                record.3,
                record.4,
                record.5,
                record.6.map(i64::from),
                record.7,
                record.8,
                record.9,
                record.10,
                record.11
            ],
        )
        .map_err(|e| crate::persistence::db_app_error(&e))?;
        Ok(None)
    })?;
    result.attempt_id = existing_id.unwrap_or(attempt_id);
    Ok(result)
}
