-- M04 语义解析与索引(《01》§4.2/§8、《M04》§2–§9)。
-- M04 独占 ContentBlock 与索引写入;解析块属于代次,按需块属于版本。

CREATE TABLE parse_generation (
    id TEXT PRIMARY KEY,
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    -- ParserConfigHash:解析器/适配器/规范化/分段/清洗版本 + 格式(服务端计算)。
    parser_config_hash TEXT NOT NULL,
    parser_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN
        ('queued','parsing','indexing','complete','partial','failed','canceled')),
    block_set_hash TEXT,
    is_current INTEGER NOT NULL DEFAULT 1,
    task_id TEXT,
    -- 批次序号按 Attempt 计:新 Attempt 归零,同代次重试依赖确定性块 ID 去重。
    current_attempt_id TEXT,
    last_batch_seq INTEGER NOT NULL DEFAULT 0,
    error_code TEXT,
    started_at INTEGER NOT NULL,
    completed_at INTEGER
);

-- 每版本最多一个当前代次(部分唯一)。
CREATE UNIQUE INDEX idx_pg_current ON parse_generation(document_version_id) WHERE is_current = 1;
CREATE INDEX idx_pg_version ON parse_generation(document_version_id, started_at);

CREATE TABLE content_block (
    id TEXT PRIMARY KEY,
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    -- 解析块必填代次;按需块为 NULL 且 indexable=0。
    parse_generation_id TEXT REFERENCES parse_generation(id),
    origin TEXT NOT NULL CHECK (origin IN ('parser','on_demand')),
    block_key TEXT NOT NULL,
    ordinal INTEGER,
    parent_block_id TEXT REFERENCES content_block(id),
    kind TEXT NOT NULL CHECK (kind IN
        ('heading','paragraph','list_item','quote','code','table','table_row','table_cell','selection')),
    -- 原文不可变;normalizedText 仅是可重建辅助值。
    text TEXT NOT NULL,
    normalized_text TEXT NOT NULL,
    text_hash TEXT NOT NULL,
    normalized_text_hash TEXT NOT NULL,
    heading_path_json TEXT NOT NULL CHECK(json_valid(heading_path_json)),
    locator_json TEXT NOT NULL CHECK(json_valid(locator_json)),
    completeness TEXT NOT NULL DEFAULT 'complete' CHECK (completeness IN
        ('complete','partial','restricted')),
    indexable INTEGER NOT NULL DEFAULT 1,
    normalization_version TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- 已完成代次的块不可追加/重排(应用层 + 完整性校验),序号部分唯一。
CREATE UNIQUE INDEX idx_block_gen_ordinal ON content_block(parse_generation_id, ordinal)
    WHERE parse_generation_id IS NOT NULL AND ordinal IS NOT NULL;
CREATE UNIQUE INDEX idx_block_gen_key ON content_block(parse_generation_id, block_key)
    WHERE parse_generation_id IS NOT NULL;
CREATE UNIQUE INDEX idx_block_ondemand ON content_block(document_version_id, normalized_text_hash, locator_json)
    WHERE origin = 'on_demand';
CREATE INDEX idx_block_version ON content_block(document_version_id);
CREATE INDEX idx_block_generation ON content_block(parse_generation_id);

-- ParseRegion 是覆盖真相(《M04》§8)。
CREATE TABLE parse_region (
    generation_id TEXT NOT NULL REFERENCES parse_generation(id),
    region_key TEXT NOT NULL,
    region_kind TEXT NOT NULL CHECK (region_kind IN ('page','body','heading','object')),
    status TEXT NOT NULL CHECK (status IN
        ('pending','partial','complete','failed','no_text_layer')),
    block_count INTEGER NOT NULL DEFAULT 0,
    order_confidence TEXT NOT NULL DEFAULT 'high' CHECK (order_confidence IN ('high','low')),
    error_code TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (generation_id, region_key)
) WITHOUT ROWID;

-- 受限对象:不能把缺失当成不存在(《M04》§8)。
CREATE TABLE unsupported_region (
    generation_id TEXT NOT NULL REFERENCES parse_generation(id),
    region_key TEXT NOT NULL,
    object_type TEXT NOT NULL,
    locator_json TEXT NOT NULL CHECK(json_valid(locator_json)),
    reason_code TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (generation_id, region_key, object_type)
) WITHOUT ROWID;

-- FTS5 trigram 基线(《01》§8.1);FTS 行是可重建缓存,不保护。
CREATE VIRTUAL TABLE content_block_fts USING fts5(
    text,
    block_id UNINDEXED,
    parse_generation_id UNINDEXED,
    tokenize = 'trigram'
);

-- 向量代次:绑定 ParseGeneration + 提供方/模型/维度/配置(《01》§8.2)。
CREATE TABLE embedding_generation (
    id TEXT PRIMARY KEY,
    parse_generation_id TEXT NOT NULL REFERENCES parse_generation(id),
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    distance_function TEXT NOT NULL DEFAULT 'cosine',
    config_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('building','complete','partial','failed','stale')),
    error_code TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    UNIQUE(parse_generation_id, provider_id, model_id, dimensions, config_hash)
);

CREATE INDEX idx_embgen_parse_gen ON embedding_generation(parse_generation_id, created_at);

-- 向量普通表:MVP 在进程内计算余弦距离;sqlite-vec vec_idx_<rowid> 虚表
-- 由 M08 接入后按代次创建,查询不跨代次(《01》§8.2)。
CREATE TABLE vector_embedding (
    generation_id TEXT NOT NULL REFERENCES embedding_generation(id),
    block_id TEXT NOT NULL REFERENCES content_block(id),
    dimensions INTEGER NOT NULL,
    vector_blob BLOB NOT NULL CHECK(length(vector_blob) = dimensions * 4),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (generation_id, block_id)
) WITHOUT ROWID;
