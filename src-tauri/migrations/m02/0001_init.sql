-- M02 文档身份、内容版本与文件位置(《01》§4.1、《M02》§9)。
-- 路径与文件名不是身份;内容变化新建版本行,不修改旧行。

CREATE TABLE document (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('pdf','markdown','docx')),
    -- 循环外键:先插入 NULL,同事务再 UPDATE(应用层维护)。
    current_version_id TEXT,
    -- 乐观并发修订号:版本确认等可变操作必须校验(《M02》§9)。
    revision INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_opened_at INTEGER
);

CREATE INDEX idx_document_last_opened ON document(last_opened_at DESC);
CREATE INDEX idx_document_updated ON document(updated_at DESC);

CREATE TABLE document_version (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    supersedes_version_id TEXT REFERENCES document_version(id),
    -- 指纹计算完成前为 NULL;完成后不可变(《01》§4.1)。
    content_hash TEXT,
    file_size INTEGER,
    fingerprint_status TEXT NOT NULL CHECK (fingerprint_status IN ('computing','ready','unreadable')),
    -- 候选版本默认 0;用户确认采用后置 1(《M02》§5)。
    confirmed INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_version_document ON document_version(document_id, created_at);
-- 就绪哈希索引(部分唯一哈希查找;同文档同内容去重由服务层裁决)。
CREATE INDEX idx_version_ready_hash ON document_version(content_hash)
    WHERE content_hash IS NOT NULL;

CREATE TABLE file_location (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    -- 最后观察到的内容版本;不假定路径当前内容仍未变化。
    observed_version_id TEXT REFERENCES document_version(id),
    path TEXT NOT NULL,
    is_primary INTEGER NOT NULL DEFAULT 1,
    last_seen_size INTEGER,
    last_seen_mtime_ns INTEGER,
    last_verified_at INTEGER,
    UNIQUE(document_id, path)
);

CREATE INDEX idx_location_document ON file_location(document_id, is_primary DESC);
-- 同一路径重复导入需要跨文档按路径定位(M02 §3/§4)。
CREATE INDEX idx_location_path ON file_location(path);
