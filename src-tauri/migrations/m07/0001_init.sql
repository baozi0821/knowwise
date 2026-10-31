-- M07 上下文快照与会话(《01》§4.4/§4.6/§7/§10、《M07》§2–§8)。
-- ContextSnapshot 由 M07 独占冻结:创建后禁止 UPDATE(触发器 + Repository 双重约束);
-- ContextSource/SnapshotBlock 同样只允许 INSERT 与按清除策略 DELETE。
-- 全文快照绑定 parseGenerationId + blockSetHash;局部快照保存显式块集合;
-- 笔记与 AI 草稿以文本快照进入,后续编辑不反写历史任务。

CREATE TABLE context_snapshot (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    -- 全文快照必填且代次完整;局部快照为冻结时当前代次(可空)。
    parse_generation_id TEXT REFERENCES parse_generation(id),
    scope_kind TEXT NOT NULL CHECK (scope_kind IN
        ('selection','current_position','section','pages','full_document','sources')),
    scope_json TEXT NOT NULL CHECK(json_valid(scope_json)),
    coverage_json TEXT NOT NULL CHECK(json_valid(coverage_json)),
    -- 全文快照:代次封存的块集合哈希;局部快照为空(块 ID 进 canonical_hash)。
    block_set_hash TEXT,
    canonical_hash TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_context_snapshot_document ON context_snapshot(document_id, created_at);
CREATE INDEX idx_context_snapshot_version ON context_snapshot(document_version_id);

-- 不可变约束(《01》§7 基线 DDL + §10 Repository 双重约束)。
CREATE TRIGGER context_snapshot_forbid_update
BEFORE UPDATE ON context_snapshot
BEGIN
    SELECT RAISE(ABORT, 'context snapshot is immutable');
END;

-- 来源清单:按 position 保序;去重由服务层完成,层级不得合并提升。
CREATE TABLE context_source (
    id TEXT PRIMARY KEY,
    snapshot_id TEXT NOT NULL REFERENCES context_snapshot(id),
    position INTEGER NOT NULL,
    source_type TEXT NOT NULL CHECK (source_type IN
        ('content_block','note','message','artifact_version','selection')),
    source_id TEXT NOT NULL,
    -- 来源版本(笔记 revision 等);原文块可空。
    source_version TEXT,
    source_role TEXT NOT NULL CHECK (source_role IN
        ('original','user_note','ai_draft')),
    display_name TEXT NOT NULL DEFAULT '',
    -- 当时正文副本:笔记/AI 草稿必须复制;原文块同为不可变文本。
    text_snapshot TEXT NOT NULL,
    locator_snapshot_json TEXT CHECK(locator_snapshot_json IS NULL
        OR json_valid(locator_snapshot_json)),
    content_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (snapshot_id, position)
);
CREATE INDEX idx_context_source_snapshot ON context_source(snapshot_id, position);
CREATE INDEX idx_context_source_target ON context_source(source_type, source_id);

CREATE TRIGGER context_source_forbid_update
BEFORE UPDATE ON context_source
BEGIN
    SELECT RAISE(ABORT, 'context source is immutable');
END;

-- 局部范围显式冻结块集合;全文完整快照不写行(代次 + blockSetHash 固定)。
CREATE TABLE snapshot_block (
    snapshot_id TEXT NOT NULL REFERENCES context_snapshot(id),
    block_id TEXT NOT NULL REFERENCES content_block(id),
    position INTEGER NOT NULL,
    PRIMARY KEY (snapshot_id, position)
) WITHOUT ROWID;
CREATE UNIQUE INDEX idx_snapshot_block_unique ON snapshot_block(snapshot_id, block_id);

CREATE TRIGGER snapshot_block_forbid_update
BEFORE UPDATE ON snapshot_block
BEGIN
    SELECT RAISE(ABORT, 'snapshot block is immutable');
END;

-- 会话:按文档组织;切换文档后仍属原文档(《M07》§6)。
-- active_snapshot_id 只影响下一次追问建议,不改变历史消息引用。
CREATE TABLE conversation (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    title TEXT NOT NULL DEFAULT '',
    active_snapshot_id TEXT REFERENCES context_snapshot(id),
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_conversation_document ON conversation(document_id, updated_at);

-- 消息:用户消息与 Task 同事务创建;助手消息按 Task 幂等完成(《01》§4.6)。
-- 不保存模型原始流和未校验 JSON:contentJson 只存通过校验的
-- user_message.v1 / assistant_placeholder.v1 / answer.v1。
CREATE TABLE message (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversation(id),
    role TEXT NOT NULL CHECK (role IN ('user','assistant')),
    snapshot_id TEXT REFERENCES context_snapshot(id),
    task_id TEXT REFERENCES task(id),
    content_json TEXT NOT NULL CHECK(json_valid(content_json)),
    -- 生命周期状态;语义状态(answered/insufficient_evidence/...)在 contentJson 内。
    answer_status TEXT NOT NULL CHECK (answer_status IN
        ('pending','completed','failed','canceled')),
    model_provider_id TEXT,
    model_id TEXT,
    error_code TEXT,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_message_conversation ON message(conversation_id, created_at, id);
-- task_id 为空时允许重复;非空时按 (task_id, role) 唯一 —— 用户消息与
-- 助手占位消息共享同一 Task,各角色内幂等(《01》§4.6/§6 语义)。
CREATE UNIQUE INDEX idx_message_task_role_unique ON message(task_id, role)
    WHERE task_id IS NOT NULL;
CREATE INDEX idx_message_snapshot ON message(snapshot_id);
