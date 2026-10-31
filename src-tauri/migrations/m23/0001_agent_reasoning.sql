-- P0 1E:用户显式开启“保存思考记录”时才写入。
-- 默认不落库;删除会话/文档或隐私清除时按文档级联删除。
-- 只保存 provider 原始 reasoning 文本与稳定关联 ID,不写密钥/路径。

CREATE TABLE agent_reasoning (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    provider_source TEXT NOT NULL DEFAULT 'model_reasoning',
    text TEXT NOT NULL,
    char_count INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (message_id)
);

CREATE INDEX idx_agent_reasoning_document ON agent_reasoning(document_id);
CREATE INDEX idx_agent_reasoning_task ON agent_reasoning(task_id);
