-- M06 标注、书签、笔记与待澄清(《01》§6、《M06》§2–§8)。
-- 用户资产:优先于后台解析、索引和 AI 任务保存;只使用 M05 创建的 Anchor。

CREATE TABLE annotation (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    -- 版本由 Anchor 固定;状态由 Anchor 有效状态派生,不落库。
    anchor_id TEXT NOT NULL REFERENCES anchor(id),
    -- 颜色是显示分类,不隐含学习掌握程度。
    color TEXT NOT NULL CHECK (color IN ('yellow','green','blue','pink','purple')),
    note_text TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL DEFAULT 1,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_annotation_document ON annotation(document_id, updated_at);
CREATE INDEX idx_annotation_anchor ON annotation(anchor_id);

-- 书签:可指向页面、块或阅读位置;toggle 用 toggle_key 做幂等身份,
-- 期望状态避免双击创建两个相反结果(《M06》§2)。
CREATE TABLE bookmark (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    document_version_id TEXT,
    anchor_id TEXT REFERENCES anchor(id),
    -- anchor_id 存在时为 anchor:前缀,否则为定位规范串。
    toggle_key TEXT NOT NULL,
    locator_json TEXT NOT NULL CHECK(json_valid(locator_json)),
    label TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    UNIQUE (document_id, toggle_key)
);
CREATE INDEX idx_bookmark_document ON bookmark(document_id, created_at);

CREATE TABLE note (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    -- 锚定笔记的版本由 Anchor 固定;文档笔记可为空。
    document_version_id TEXT,
    anchor_id TEXT REFERENCES anchor(id),
    -- origin 区分用户笔记与 AI 草稿;编辑后仍保留 AI 来源(《M06》§3)。
    origin TEXT NOT NULL CHECK (origin IN ('user','ai_draft','user_edited_ai')),
    body TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN
        ('open','confirmed','needs_clarification')),
    -- SourceReference[] 快照:来源删除后仍可查看当时依据。
    source_refs_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(source_refs_json)),
    revision INTEGER NOT NULL DEFAULT 1,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_note_document ON note(document_id, updated_at);
CREATE INDEX idx_note_anchor ON note(anchor_id);
CREATE INDEX idx_note_status ON note(document_id, status);

-- 标签:文档内规范化名称唯一,显示名称保留大小写。
CREATE TABLE tag (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    normalized_name TEXT NOT NULL,
    display_name TEXT NOT NULL,
    UNIQUE (document_id, normalized_name)
);

-- 标签删除只解除关联,不删除笔记(《M06》§5)。
CREATE TABLE note_tag (
    note_id TEXT NOT NULL REFERENCES note(id),
    tag_id TEXT NOT NULL REFERENCES tag(id),
    PRIMARY KEY (note_id, tag_id)
) WITHOUT ROWID;
CREATE INDEX idx_note_tag_tag ON note_tag(tag_id);

-- 待澄清清单:不是行动项抽取系统,不从文档虚构负责人/日期/依赖。
CREATE TABLE clarification_item (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    question_text TEXT NOT NULL,
    -- 来源可为用户问题、助手回答、标注、笔记或错题;question/answer/quiz
    -- 为前向引用(M07/M11),创建时必须携带来源快照。
    source_type TEXT CHECK (source_type IN
        ('question','answer','annotation','note','quiz')),
    source_id TEXT,
    -- 来源快照与可回跳引用(文本 + anchorId/locator)。
    source_snapshot_json TEXT NOT NULL CHECK(json_valid(source_snapshot_json)),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN
        ('pending','resolved','deferred')),
    -- resolved 必须关联 Note/Message/ArtifactVersion(同文档)。
    resolution_ref_json TEXT CHECK(json_valid(resolution_ref_json)),
    resolved_at INTEGER,
    revision INTEGER NOT NULL DEFAULT 1,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_clarification_doc_status ON clarification_item(document_id, status, updated_at);
CREATE INDEX idx_clarification_source ON clarification_item(source_type, source_id);
