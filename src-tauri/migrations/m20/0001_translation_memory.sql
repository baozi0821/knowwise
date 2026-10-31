-- 2C 翻译记忆:源 hash + 目标语言 + 模型 + 术语表版本 → 译文。
-- 只缓存译文文本,不缓存页图/密钥/路径;文档删除与隐私清除可级联。

CREATE TABLE translation_memory (
    id TEXT PRIMARY KEY,
    source_hash TEXT NOT NULL,
    target_language TEXT NOT NULL,
    model_id TEXT NOT NULL,
    glossary_version TEXT NOT NULL,
    translated_text TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (source_hash, target_language, model_id, glossary_version)
);

CREATE INDEX idx_translation_memory_lookup
    ON translation_memory(source_hash, target_language, model_id, glossary_version);
