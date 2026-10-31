-- M10 产物中心与可编辑摘要(《01》§7、《M10》§2–§9)。
-- Artifact 外壳 + 不可变 AI 原版 + 可变用户版本 + 导出记录。

CREATE TABLE artifact (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    -- kind 至少 summary/quiz/graph;未来类型经注册扩展,不修改聊天流。
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    current_version_id TEXT,
    revision INTEGER NOT NULL DEFAULT 1,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_artifact_document ON artifact(document_id, updated_at);
CREATE INDEX idx_artifact_kind ON artifact(document_id, kind, updated_at);

CREATE TABLE artifact_version (
    id TEXT PRIMARY KEY,
    artifact_id TEXT NOT NULL REFERENCES artifact(id),
    parent_version_id TEXT REFERENCES artifact_version(id),
    version_no INTEGER NOT NULL,
    -- AI 原版不可变;用户版本可变;copy/restore 派生新版本。
    origin TEXT NOT NULL CHECK(origin IN ('ai_draft','user_edit','copy','restore')),
    state TEXT NOT NULL CHECK(state IN ('mutable','sealed')),
    schema_version TEXT NOT NULL,
    content_json TEXT NOT NULL CHECK(json_valid(content_json)),
    -- M07 快照 ID:生成时冻结;此处只存引用(M07 落地后接线)。
    snapshot_id TEXT,
    -- UNIQUE(task_id):同一 Task 只产出一份最终产物(M10 §10)。
    task_id TEXT,
    coverage_json TEXT NOT NULL CHECK(json_valid(coverage_json)),
    revision INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    sealed_at INTEGER,
    UNIQUE(artifact_id, version_no),
    UNIQUE(task_id)
);
CREATE INDEX idx_artifact_version_artifact ON artifact_version(artifact_id, version_no);

-- 数据库级不可变约束(《01》§7),与 ArtifactService 校验双重防护:
-- 1) origin=ai_draft 的版本任何 UPDATE 都被拒绝;
-- 2) sealed 版本任何 UPDATE 都被拒绝;
-- 3) state 只允许 mutable→sealed(封版,仅 user_edit)。
-- 违反时 ABORT,服务层映射为 ARTIFACT_VERSION_IMMUTABLE。
CREATE TRIGGER artifact_version_forbid_update
BEFORE UPDATE ON artifact_version
WHEN OLD.origin = 'ai_draft'
     OR OLD.state = 'sealed'
     OR (NEW.state != 'sealed' AND NEW.state != OLD.state)
BEGIN
    SELECT RAISE(ABORT, 'artifact version is immutable');
END;

-- 导出记录:只保存状态与格式,不保存正文和完整目标路径(《M10》§9)。
CREATE TABLE export_record (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    artifact_id TEXT NOT NULL REFERENCES artifact(id),
    artifact_version_id TEXT NOT NULL REFERENCES artifact_version(id),
    format TEXT NOT NULL,
    -- 目标类别(file/path 由用户显式选择),不存文件名与完整路径。
    destination_kind TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('succeeded','failed')),
    error_code TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_export_record_artifact ON export_record(artifact_version_id, created_at);

-- 删除产物:版本与导出记录级联清理;不触碰原文、笔记或其他产物。
-- (export_record 已建,触发器可安全引用。)
CREATE TRIGGER artifact_delete_cleanup
AFTER DELETE ON artifact
BEGIN
    DELETE FROM artifact_version WHERE artifact_id = OLD.id;
    DELETE FROM export_record WHERE artifact_id = OLD.id;
END;
