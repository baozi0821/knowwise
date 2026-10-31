-- P3A Agent 写工具 + 人在环确认:扩展 agent_step。
-- 迁移只前滚;m16 已应用文件不可修改,这里重建表以扩展 CHECK 并新增确认投影。

CREATE TABLE agent_step_m18 (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES agent_run(id) ON DELETE CASCADE,
    step_no INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('model','tool')),
    tool_name TEXT,
    args_summary TEXT,
    result_summary TEXT,
    status TEXT NOT NULL CHECK(status IN ('running','awaiting_confirmation','ok','failed','canceled')),
    error_code TEXT,
    duration_ms INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    -- 确认框投影:工具名/摘要/是否需要路径;绝不含正文、密钥或完整路径。
    confirmation_json TEXT CHECK(confirmation_json IS NULL OR json_valid(confirmation_json))
);

INSERT INTO agent_step_m18 (
    id, run_id, step_no, kind, tool_name, args_summary, result_summary,
    status, error_code, duration_ms, created_at, updated_at, confirmation_json
)
SELECT
    id, run_id, step_no, kind, tool_name, args_summary, result_summary,
    status, error_code, duration_ms, created_at, updated_at, NULL
FROM agent_step;

DROP TABLE agent_step;
ALTER TABLE agent_step_m18 RENAME TO agent_step;
CREATE UNIQUE INDEX idx_agent_step_no ON agent_step(run_id, step_no);
CREATE INDEX idx_agent_step_run ON agent_step(run_id, step_no);
