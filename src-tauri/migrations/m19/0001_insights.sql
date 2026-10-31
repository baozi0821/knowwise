-- 2A 动态卡片与文档画像。
-- Profile/Card 都是可重建缓存:docVersion + parseGeneration + 内容代次变化即失效。

CREATE TABLE document_profile (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    parse_generation_id TEXT,
    profile_version TEXT NOT NULL,
    format TEXT NOT NULL,
    language TEXT NOT NULL,
    document_type TEXT NOT NULL,
    page_count INTEGER,
    section_count INTEGER NOT NULL,
    title_tree_json TEXT NOT NULL CHECK(json_valid(title_tree_json)),
    created_at INTEGER NOT NULL,
    UNIQUE (document_version_id, profile_version)
);

CREATE INDEX idx_document_profile_document ON document_profile(document_id, created_at);

CREATE TABLE insight_card (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    profile_version TEXT NOT NULL,
    -- 0 = 文档级/欢迎态;>0 = PDF 页码。
    page_number INTEGER NOT NULL CHECK(page_number >= 0),
    -- 缓存作用域:page:N / section:<hash> / blocks:<hash>(非 PDF 无页码时不混用缓存)。
    scope_key TEXT NOT NULL,
    locale TEXT NOT NULL,
    kind TEXT NOT NULL,
    generated_by TEXT NOT NULL CHECK(generated_by IN ('ai','extractive')),
    confidence TEXT NOT NULL CHECK(confidence IN ('high','medium','low')),
    card_json TEXT NOT NULL CHECK(json_valid(card_json)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (document_version_id, profile_version, scope_key, locale, kind)
);

CREATE INDEX idx_insight_card_document ON insight_card(document_id, updated_at);
