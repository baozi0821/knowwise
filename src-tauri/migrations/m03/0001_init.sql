-- M03 阅读位置(《01》§6、《M03》§7)。
-- 每文档唯一;位置不是掌握度,不参与学习画像。
-- locator_json 使用统一 SourceLocator 封闭联合;不含缩放/CSS 像素。

CREATE TABLE reading_position (
    document_id TEXT PRIMARY KEY REFERENCES document(id),
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    locator_json TEXT NOT NULL CHECK(json_valid(locator_json)),
    -- 辅助恢复:标题路径与锚点文本哈希(精确 Locator 失效时降级查找)。
    heading_path_json TEXT CHECK(heading_path_json IS NULL OR json_valid(heading_path_json)),
    anchor_text_hash TEXT,
    revision INTEGER NOT NULL DEFAULT 1,
    saved_at INTEGER NOT NULL
);
