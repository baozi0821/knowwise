-- M13 运行指标(P0 1C):每次流式运行一行。
-- 只记录 ID/状态/耗时与比例,绝不记录正文、推理、密钥或路径。

CREATE TABLE run_metrics (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    message_id TEXT,
    mode TEXT NOT NULL CHECK (mode IN ('fast','quality')),
    status TEXT NOT NULL CHECK (status IN ('verified','unverified','failed','canceled')),
    first_token_ms INTEGER,
    first_visible_answer_ms INTEGER,
    reasoning_first_event_ms INTEGER,
    max_event_gap_ms INTEGER NOT NULL DEFAULT 0,
    first_answer_ms INTEGER,
    total_ms INTEGER,
    fallback_used INTEGER NOT NULL DEFAULT 0 CHECK (fallback_used IN (0,1)),
    citation_valid_rate REAL,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_run_metrics_task ON run_metrics(task_id);
CREATE INDEX idx_run_metrics_created ON run_metrics(created_at);
