-- M08 v2:允许 Vision 用途(《WBS-1》图片多模态)。
-- SQLite 不能直接修改 CHECK 约束;按前滚原则重建 privacy_grant 并保留全部历史。
-- 只新增 'vision' 取值,既有 generate/embed 数据与索引语义不变。

CREATE TABLE privacy_grant_new (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    processing_location TEXT NOT NULL CHECK (processing_location IN ('local','remote')),
    provider_id TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('generate','embed','vision')),
    status TEXT NOT NULL DEFAULT 'granted' CHECK (status IN ('granted','revoked')),
    granted_at INTEGER NOT NULL,
    revoked_at INTEGER,
    policy_version TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

INSERT INTO privacy_grant_new(
    id, document_id, processing_location, provider_id, purpose, status,
    granted_at, revoked_at, policy_version, idempotency_key, created_at
)
SELECT
    id, document_id, processing_location, provider_id, purpose, status,
    granted_at, revoked_at, policy_version, idempotency_key, created_at
FROM privacy_grant;

DROP TABLE privacy_grant;
ALTER TABLE privacy_grant_new RENAME TO privacy_grant;

CREATE INDEX idx_grant_lookup
    ON privacy_grant(document_id, processing_location, provider_id, purpose, status);
CREATE INDEX idx_grant_document ON privacy_grant(document_id, granted_at);
