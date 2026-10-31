-- M17 会话滚动摘要(WBS-5 长上下文)。
-- 每个会话最多一行:保存最近一次成功摘要与覆盖到的消息边界。
-- 失败只记录状态,不覆盖旧的 summary_text/up_to_message_id:
-- 主问答不得因摘要失败而阻塞,但失败必须可见、可诊断。
-- 级联:conversation 删除时摘要随外键删除;up_to_message_id 指向的
-- 消息被删除时置空(不会留下悬空引用)。

CREATE TABLE conversation_summary (
    conversation_id TEXT PRIMARY KEY REFERENCES conversation(id) ON DELETE CASCADE,
    up_to_message_id TEXT REFERENCES message(id) ON DELETE SET NULL,
    summary_text TEXT NOT NULL DEFAULT '',
    token_count INTEGER NOT NULL DEFAULT 0 CHECK (token_count >= 0),
    status TEXT NOT NULL DEFAULT 'ok' CHECK (status IN ('ok', 'failed')),
    error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_conversation_summary_updated ON conversation_summary(updated_at);
