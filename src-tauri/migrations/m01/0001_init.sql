-- M01 任务与迁移元数据(《01》§7 DDL 基线,不得削弱约束)。
-- schema_migration 表由迁移执行器在首个迁移前创建,不在此重复。

CREATE TABLE task (
    id TEXT PRIMARY KEY,
    document_id TEXT,
    task_type TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK (status IN
        ('queued','running','succeeded','failed','canceled','interrupted')),
    phase TEXT NOT NULL,
    outcome TEXT NOT NULL DEFAULT 'none',
    current_attempt_id TEXT,
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    state_version INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    error_code TEXT,
    -- 兼容扩展:TaskView.resultRef 的落库投影(可空)。
    result_ref_kind TEXT
        CHECK (result_ref_kind IS NULL OR result_ref_kind IN ('message','artifact_version')),
    result_ref_id TEXT
);

CREATE INDEX idx_task_document_status ON task(document_id, status);
CREATE INDEX idx_task_updated ON task(updated_at DESC);

CREATE TABLE task_attempt (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES task(id),
    attempt_no INTEGER NOT NULL,
    provider_id TEXT,
    model_id TEXT,
    status TEXT NOT NULL CHECK (status IN
        ('running','succeeded','failed','canceled','interrupted')),
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    error_code TEXT,
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    UNIQUE(task_id, attempt_no)
);

CREATE INDEX idx_attempt_task ON task_attempt(task_id);

CREATE TABLE task_result (
    task_id TEXT PRIMARY KEY REFERENCES task(id),
    result_json TEXT NOT NULL CHECK(json_valid(result_json)),
    result_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- 迁移备份元数据:只记录文件与校验信息,不含任何内容。
-- 执行器在首个迁移前也会创建此表(全新库先备份后应用),故幂等声明。
CREATE TABLE IF NOT EXISTS migration_backup (
    id TEXT PRIMARY KEY,
    file_name TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    checksum TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
