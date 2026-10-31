-- 2B quiz.v2 作答记录:选择/多选/判断/填空确定性判分;问答为自评 rubric。
-- 与 quiz.v1 作答记录并存,不修改旧表。

CREATE TABLE quiz_v2_attempt (
    id TEXT PRIMARY KEY,
    artifact_version_id TEXT NOT NULL,
    question_id TEXT NOT NULL,
    selected_option_ids_json TEXT NOT NULL CHECK(json_valid(selected_option_ids_json)),
    blank_answers_json TEXT NOT NULL CHECK(json_valid(blank_answers_json)),
    answer_text TEXT,
    is_correct INTEGER CHECK(is_correct IS NULL OR is_correct IN (0,1)),
    score REAL,
    max_score REAL NOT NULL,
    grading TEXT NOT NULL CHECK(grading IN ('auto','self')),
    created_at INTEGER NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE
);

CREATE INDEX idx_quiz_v2_attempt_version ON quiz_v2_attempt(artifact_version_id, question_id);
