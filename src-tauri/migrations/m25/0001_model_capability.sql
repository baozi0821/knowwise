-- 2F 长上下文:模型能力表(上下文窗口/最大输出/能力标记)。
-- 自动探测结果与用户手动修正都写这里;source 区分来源,便于 UI 标注“已修正”。
CREATE TABLE model_capability (
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    context_window INTEGER NOT NULL CHECK(context_window > 0),
    max_output INTEGER NOT NULL CHECK(max_output > 0),
    streaming INTEGER NOT NULL DEFAULT 1 CHECK(streaming IN (0,1)),
    reasoning INTEGER NOT NULL DEFAULT 0 CHECK(reasoning IN (0,1)),
    tool_calling INTEGER NOT NULL DEFAULT 0 CHECK(tool_calling IN (0,1)),
    vision INTEGER NOT NULL DEFAULT 0 CHECK(vision IN (0,1)),
    json_schema INTEGER NOT NULL DEFAULT 0 CHECK(json_schema IN (0,1)),
    -- auto = 探测/内置默认;manual = 用户在设置中修正。
    source TEXT NOT NULL CHECK(source IN ('auto','manual')),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (provider_id, model_id)
);
