-- M11 理解校验题(《01》§4.7/§6、《M11》§4–§5)。
-- 题目内容本体保存在 M10 的 artifact_version.content_json(quiz.v1),
-- 本迁移只保存作答记录与无效题反馈两张表。
-- artifact_version 表由 M10 拥有;落地前不做物理外键,
-- 版本存在性由 QuizArtifactSource 端口校验(M13 在 M10 接入时接线)。

-- 作答记录:同一题重做新增 attempt,保留全部历史;不覆盖旧记录。
-- 无效题即使答错也不计入用户错误(统计只取 validity_status='valid',
-- 见服务层 QuizStats);记录本身只呈现事实,不推断掌握度。
CREATE TABLE comprehension_record (
    id TEXT PRIMARY KEY,
    artifact_version_id TEXT NOT NULL,
    question_id TEXT NOT NULL,
    attempt_no INTEGER NOT NULL CHECK (attempt_no >= 1),
    selected_option_id TEXT NOT NULL,
    is_correct INTEGER NOT NULL CHECK (is_correct IN (0, 1)),
    -- 答题时题目有效状态的快照;unverified/valid/invalid/disputed。
    validity_status TEXT NOT NULL CHECK (validity_status IN
        ('unverified', 'valid', 'invalid', 'disputed')),
    -- 答题时该题已 accepted 的反馈类型(可空);不依赖是否已作答的反馈
    -- 存 quiz_question_feedback,此处仅为呈现上下文。
    feedback_type TEXT CHECK (feedback_type IN
        ('multiple_answers', 'no_evidence', 'wrong_explanation', 'ambiguous')),
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    UNIQUE (artifact_version_id, question_id, attempt_no)
);
CREATE INDEX idx_comprehension_version_question
    ON comprehension_record(artifact_version_id, question_id, attempt_no);
CREATE INDEX idx_comprehension_created ON comprehension_record(created_at);

-- 无效题/质量反馈:独立于作答记录;不静默改写 AI 原版,
-- 只有 accepted 状态可触发重生成建议(重新生成仍由 M10 创建新草稿版本)。
CREATE TABLE quiz_question_feedback (
    id TEXT PRIMARY KEY,
    artifact_version_id TEXT NOT NULL,
    -- 必须存在于对应版本内容(服务层经端口校验)。
    question_id TEXT NOT NULL,
    feedback_type TEXT NOT NULL CHECK (feedback_type IN
        ('multiple_answers', 'no_evidence', 'wrong_explanation', 'ambiguous')),
    -- 可选纯文本说明,限长;不执行 Markdown/脚本。
    comment TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'accepted', 'dismissed')),
    revision INTEGER NOT NULL DEFAULT 1,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_quiz_feedback_version_question
    ON quiz_question_feedback(artifact_version_id, question_id, created_at);
CREATE INDEX idx_quiz_feedback_status ON quiz_question_feedback(status);
