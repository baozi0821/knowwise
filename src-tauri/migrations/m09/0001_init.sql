-- M09 检索与问答编排(《01》§4.6、《M09》§10)。
-- M09 独占 retrieval_run / retrieval_candidate;回答正文归 M07 消息,
-- Citation 归 M05;普通日志不保存问题正文,只保存查询哈希。

CREATE TABLE retrieval_run (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL UNIQUE REFERENCES task(id),
    snapshot_id TEXT NOT NULL,
    query_text_hash TEXT NOT NULL,
    -- 版本化检索配置(FTS/向量候选上限、RRF 常数、最终候选数、嵌入代次)。
    config_json TEXT NOT NULL CHECK(json_valid(config_json)),
    capability_mode TEXT NOT NULL CHECK (capability_mode IN
        ('explicit_blocks','fts_only','hybrid','unavailable')),
    -- LIKE 降级等偏离主路径的事实必须记录(《M09》§4)。
    degraded_path INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_retrieval_run_snapshot ON retrieval_run(snapshot_id);

-- 每个候选一行:召回名次、融合分数与是否进入 Prompt;
-- 未入选必须带 exclusion_reason,禁止无记录截断(《M09》§5、A9)。
CREATE TABLE retrieval_candidate (
    run_id TEXT NOT NULL REFERENCES retrieval_run(id) ON DELETE CASCADE,
    block_id TEXT NOT NULL,
    fts_rank INTEGER,
    vector_rank INTEGER,
    rrf_score REAL NOT NULL,
    included_in_prompt INTEGER NOT NULL DEFAULT 0,
    exclusion_reason TEXT,
    PRIMARY KEY (run_id, block_id)
) WITHOUT ROWID;
CREATE INDEX idx_retrieval_candidate_block ON retrieval_candidate(block_id);
