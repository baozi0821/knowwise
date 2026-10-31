-- P2 Agent 只读循环(WBS-2/WBS-3):运行与步骤审计。
-- 迁移只前滚;本文件创建后不得修改字节。

CREATE TABLE agent_run (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL UNIQUE,
    document_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    conversation_id TEXT,
    assistant_message_id TEXT,
    status TEXT NOT NULL CHECK(status IN ('running','succeeded','failed','canceled')),
    stopped_reason TEXT,
    -- 完成后固化 agent.v1(最终答复 + 步骤快照),同幂等键重放不再调用模型。
    final_json TEXT,
    error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_agent_run_task ON agent_run(task_id);
CREATE INDEX idx_agent_run_conversation ON agent_run(conversation_id, created_at);

CREATE TABLE agent_step (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES agent_run(id) ON DELETE CASCADE,
    step_no INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('model','tool')),
    tool_name TEXT,
    -- 只保存脱敏摘要(参数规模/计数/状态),不保存正文全文、密钥或路径。
    args_summary TEXT,
    result_summary TEXT,
    status TEXT NOT NULL CHECK(status IN ('running','ok','failed','canceled')),
    error_code TEXT,
    duration_ms INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_agent_step_no ON agent_step(run_id, step_no);
CREATE INDEX idx_agent_step_run ON agent_step(run_id, step_no);
