//! M15 图片附件:上传、绑定、读取与清理(《WBS-1》)。
//!
//! 设计要点:
//! - 图片字节落应用数据目录,数据库只存引用与 sha256;
//! - 消息正文只使用 `![截图](attachment:<id>)` 轻量引用;
//! - 展示图只经 IPC 返回 data URL,不写数据库;
//! - 会话/文档删除后由命令层调用文件清理;启动时做孤儿扫描兜底。

pub mod models;
pub mod registration;
pub mod service;

#[cfg(feature = "desktop")]
pub mod commands;

pub use models::{
    build_user_message_text, strip_markdown_images, AttachmentThumbnail, AttachmentView,
    LocalImageData, ALLOWED_MIMES, ATTACHMENT_ORPHAN_TTL_MS, MAX_ATTACHMENTS_PER_MESSAGE,
    MAX_ATTACHMENT_BYTES, MAX_IMAGE_EDGE, MAX_LOCAL_IMAGE_BYTES, MAX_TOTAL_ATTACHMENT_BYTES,
};
pub use service::{infer_image_mime, read_local_image, AttachmentService, UploadAttachment};
