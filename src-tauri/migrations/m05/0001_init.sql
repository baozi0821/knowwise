-- M05 锚点与证据(《01》§4.3、《M05》§2–§9)。
-- M05 独占 Anchor/Citation 校验与证据保留裁决;不保存回答正文。
-- 结构状态与支持状态分离:structuralStatus 只证明目标存在且引文匹配。

CREATE TABLE anchor (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    -- 选区种类与 M03 RawSelection.selectionKind 对齐。
    kind TEXT NOT NULL CHECK (kind IN ('text_selection','page','section')),
    -- active/orphaned/relocated/invalid;读取时结合文档当前版本派生有效状态。
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN
        ('active','orphaned','relocated','invalid')),
    exact_text TEXT NOT NULL,
    prefix_text TEXT NOT NULL DEFAULT '',
    suffix_text TEXT NOT NULL DEFAULT '',
    -- 原文快照:孤立/失效后仍可查看,不受后续版本影响。
    snapshot_text TEXT NOT NULL,
    text_hash TEXT NOT NULL,
    primary_locator_json TEXT NOT NULL CHECK(json_valid(primary_locator_json)),
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_anchor_document ON anchor(document_id, created_at);
CREATE INDEX idx_anchor_version ON anchor(document_version_id);

-- 多块选区按原文顺序保存块引用与每块引文(《M05》§2)。
CREATE TABLE anchor_block (
    anchor_id TEXT NOT NULL REFERENCES anchor(id),
    block_id TEXT NOT NULL REFERENCES content_block(id),
    position INTEGER NOT NULL,
    block_quote_text TEXT NOT NULL,
    PRIMARY KEY (anchor_id, position)
) WITHOUT ROWID;
CREATE INDEX idx_anchor_block_block ON anchor_block(block_id);

CREATE TABLE citation (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id),
    document_version_id TEXT NOT NULL REFERENCES document_version(id),
    -- M05 不创建 owner,只校验存在与归属(应用服务注册校验器)。
    owner_type TEXT NOT NULL CHECK (owner_type IN
        ('message','note','artifact_version','clarification')),
    owner_id TEXT NOT NULL,
    -- 指向结论段(如 AnswerContent segment id)。
    claim_key TEXT NOT NULL,
    claim_kind TEXT NOT NULL DEFAULT 'inference' CHECK (claim_kind IN
        ('document_fact','inference','example','user_view','general_note')),
    source_type TEXT NOT NULL CHECK (source_type IN
        ('content_block','anchor','note','message','artifact_version','selection',
         'user_view','ai_draft')),
    source_id TEXT,
    -- 块/锚点为多态目标引用:target_missing 的失败引用也必须可保存
    -- (透镜展示缺失证据),故不建普通外键,存在性由 M05 校验(《01》§2)。
    content_block_id TEXT,
    anchor_id TEXT,
    -- 引文必须是单一 ContentBlock 的连续原始片段(规范化匹配命中时
    -- 保存原文切片,匹配方式记录在 source_snapshot)。
    quoted_text TEXT NOT NULL,
    source_snapshot TEXT NOT NULL CHECK(json_valid(source_snapshot)),
    structural_status TEXT NOT NULL CHECK (structural_status IN
        ('valid','target_missing','quote_mismatch','version_mismatch','locator_invalid')),
    support_status TEXT NOT NULL DEFAULT 'unverified' CHECK (support_status IN
        ('unverified','supported','insufficient','conflict')),
    support_source TEXT NOT NULL DEFAULT 'none' CHECK (support_source IN
        ('none','model','rule','user')),
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_citation_owner ON citation(owner_type, owner_id);
CREATE INDEX idx_citation_block ON citation(content_block_id);
CREATE INDEX idx_citation_document ON citation(document_id);
CREATE INDEX idx_citation_anchor ON citation(anchor_id);

-- 重定位候选:仅 proposed 供用户确认;confirmed 才建立新 Anchor(《M05》§7)。
CREATE TABLE anchor_relocation (
    id TEXT PRIMARY KEY,
    anchor_id TEXT NOT NULL REFERENCES anchor(id),
    target_version_id TEXT NOT NULL REFERENCES document_version(id),
    candidate_json TEXT NOT NULL CHECK(json_valid(candidate_json)),
    score REAL NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('proposed','confirmed','rejected')),
    created_at INTEGER NOT NULL,
    confirmed_at INTEGER
);
CREATE INDEX idx_relocation_anchor ON anchor_relocation(anchor_id, status);

-- 证据反馈:append-only,不静默改写历史回答或产物(《01》§4.3)。
CREATE TABLE evidence_feedback (
    id TEXT PRIMARY KEY,
    citation_id TEXT NOT NULL REFERENCES citation(id),
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    feedback_type TEXT NOT NULL CHECK (feedback_type IN
        ('quote_wrong','insufficient_evidence','scope_wrong','conflict_with_source')),
    comment TEXT NOT NULL DEFAULT '',
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_feedback_citation ON evidence_feedback(citation_id, created_at);
