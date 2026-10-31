-- M15 图片附件(《WBS-1》图片多模态)。
-- 正文只存引用(Markdown `attachment:<id>`),图片字节落应用数据目录;
-- 数据库只保存 MIME/尺寸/摘要/相对 storage_key,绝不保存 base64。
-- document_id 用于“附件属于当前文档”的校验;conversation_id/message_id
-- 在上传后、发送前为空,发送时绑定到用户消息。
-- 级联:消息/会话/文档删除时由外键清理元数据;文件由附件服务清理。

CREATE TABLE attachment (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES document(id) ON DELETE CASCADE,
    conversation_id TEXT REFERENCES conversation(id) ON DELETE CASCADE,
    message_id TEXT REFERENCES message(id) ON DELETE CASCADE,
    mime TEXT NOT NULL CHECK (mime IN ('image/jpeg','image/png','image/webp')),
    width INTEGER,
    height INTEGER,
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    byte_size INTEGER NOT NULL CHECK (byte_size > 0),
    -- 相对应用数据目录的存储键;不把绝对路径写入数据库/IPC/日志。
    storage_key TEXT NOT NULL UNIQUE,
    name TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_attachment_document ON attachment(document_id, created_at);
CREATE INDEX idx_attachment_conversation ON attachment(conversation_id);
CREATE INDEX idx_attachment_message ON attachment(message_id);
-- 未绑定附件的孤儿扫描(按创建时间过期清理)。
CREATE INDEX idx_attachment_unbound ON attachment(created_at)
    WHERE message_id IS NULL;
