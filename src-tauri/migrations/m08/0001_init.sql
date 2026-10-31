-- M08 模型接入、隐私与设置(《01》§4.8/§11、《M08》§5/§8)。
-- 本模块只拥有 privacy_grant 与 data_clear_audit;密钥仅在系统凭据存储,
-- 非敏感模型设置存原子 JSON 文件,不进数据库。

-- 隐私授权:按 Document × processingLocation × providerId × purpose 管理;
-- 本地与远程分别授权,生成与嵌入分别授权(《M08》§5)。
-- 历史保留:撤回只更新状态,不删除行;"单一当前语义"由应用层保证——
-- 同一 (document, location, provider, purpose) 至多一行 status='granted'。
CREATE TABLE privacy_grant (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    processing_location TEXT NOT NULL CHECK (processing_location IN ('local','remote')),
    provider_id TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('generate','embed')),
    status TEXT NOT NULL DEFAULT 'granted' CHECK (status IN ('granted','revoked')),
    granted_at INTEGER NOT NULL,
    revoked_at INTEGER,
    -- 授权时向用户展示的隐私说明版本;版本变化需重新确认。
    policy_version TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_grant_lookup
    ON privacy_grant(document_id, processing_location, provider_id, purpose, status);
CREATE INDEX idx_grant_document ON privacy_grant(document_id, granted_at);

-- 派生数据清除审计:只保存类别、数量与结果,不保存被清除正文(《01》§4.8)。
CREATE TABLE data_clear_audit (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    categories_json TEXT NOT NULL CHECK(json_valid(categories_json)),
    -- 清除不是 Task 时的外部关联(预留;MVP 直接执行,无任务)。
    task_id TEXT,
    status TEXT NOT NULL CHECK (status IN ('completed','failed')),
    -- 执行结果摘要(各类别删除数等非内容计数),幂等重放返回同一结果。
    result_json TEXT CHECK(json_valid(result_json)),
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    completed_at INTEGER
);
CREATE INDEX idx_clear_audit_document ON data_clear_audit(document_id, created_at);
