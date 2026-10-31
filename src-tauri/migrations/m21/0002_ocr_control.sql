-- 2D 页级 OCR 暂停/继续:按文档版本持久化暂停信号。
-- 任务层没有暂停语义,暂停由运行中的 OCR 循环在「当前页完成后」检查此表;
-- 不取消任务、不删除已识别页。新 Attempt/终态时由服务清除。
CREATE TABLE page_ocr_control (
    document_version_id TEXT PRIMARY KEY REFERENCES document_version(id),
    paused INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0,1)),
    paused_at INTEGER,
    updated_at INTEGER NOT NULL
);
