--! foreign_keys_off
-- 2D 页级 OCR:content_block 增加 origin='ocr' 与 ocr_confidence,
-- 并建立页状态表。content_block 含 CHECK,SQLite 只能重建;
-- 执行器按首行标记在事务外关闭外键,提交后强制 foreign_key_check。

CREATE TABLE content_block_new (
    id TEXT PRIMARY KEY,
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    parse_generation_id TEXT REFERENCES parse_generation(id),
    origin TEXT NOT NULL CHECK (origin IN ('parser','on_demand','ocr')),
    block_key TEXT NOT NULL,
    ordinal INTEGER,
    parent_block_id TEXT REFERENCES content_block_new(id),
    kind TEXT NOT NULL CHECK (kind IN
        ('heading','paragraph','list_item','quote','code','table','table_row','table_cell','selection')),
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
    -- OCR 行/段落置信度(0..1);非 OCR 块为 NULL。
    ocr_confidence REAL CHECK (ocr_confidence IS NULL OR (ocr_confidence >= 0 AND ocr_confidence <= 1)),
    created_at INTEGER NOT NULL
);

INSERT INTO content_block_new(
    id, document_version_id, parse_generation_id, origin, block_key, ordinal,
    parent_block_id, kind, text, normalized_text, text_hash, normalized_text_hash,
    heading_path_json, locator_json, completeness, indexable, normalization_version,
    ocr_confidence, created_at)
SELECT
    id, document_version_id, parse_generation_id, origin, block_key, ordinal,
    parent_block_id, kind, text, normalized_text, text_hash, normalized_text_hash,
    heading_path_json, locator_json, completeness, indexable, normalization_version,
    NULL, created_at
FROM content_block;

DROP TABLE content_block;
ALTER TABLE content_block_new RENAME TO content_block;

CREATE UNIQUE INDEX idx_block_gen_ordinal ON content_block(parse_generation_id, ordinal)
    WHERE parse_generation_id IS NOT NULL AND ordinal IS NOT NULL;
CREATE UNIQUE INDEX idx_block_gen_key ON content_block(parse_generation_id, block_key)
    WHERE parse_generation_id IS NOT NULL;
CREATE UNIQUE INDEX idx_block_ondemand ON content_block(document_version_id, normalized_text_hash, locator_json)
    WHERE origin = 'on_demand';
CREATE INDEX idx_block_version ON content_block(document_version_id);
CREATE INDEX idx_block_generation ON content_block(parse_generation_id);

-- 页级 OCR 状态:一个文档版本一页一行;重新识别产生新解析代次并重置。
CREATE TABLE page_ocr (
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    page_number INTEGER NOT NULL CHECK (page_number > 0),
    parse_generation_id TEXT REFERENCES parse_generation(id),
    status TEXT NOT NULL CHECK (status IN
        ('pending','rendering','ocr_running','completed','low_confidence','failed','skipped')),
    -- 质量检测指标(字符数/替换字符/覆盖率等);只保留统计,不含页图。
    quality_json TEXT CHECK (quality_json IS NULL OR json_valid(quality_json)),
    reasons_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(reasons_json)),
    render_hash TEXT,
    engine_version TEXT,
    confidence REAL CHECK (confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
    block_count INTEGER NOT NULL DEFAULT 0 CHECK (block_count >= 0),
    error_code TEXT,
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    PRIMARY KEY (document_version_id, page_number)
) WITHOUT ROWID;

CREATE INDEX idx_page_ocr_generation ON page_ocr(parse_generation_id, page_number);
