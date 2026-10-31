//! 图片附件 DTO 与硬界限(《WBS-1》图片多模态)。
//!
//! 约束:
//! - 单张压缩后 ≤1.5MB;单条消息最多 3 张;3 张合计 ≤5MB;
//! - 允许 MIME 只有 JPEG/PNG/WebP;
//! - DTO 只含相对存储键之外的展示信息,不暴露本机绝对路径;
//! - 缩略图/显示图只经 IPC 返回 data URL,不写入数据库。

use serde::Serialize;
use ts_rs::TS;

/// 单张图片字节上限(压缩后;前端 canvas 需保证)。
pub const MAX_ATTACHMENT_BYTES: usize = 1_500_000;
/// 单条用户消息最多图片数。
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 3;
/// 单条消息图片总字节上限。
pub const MAX_TOTAL_ATTACHMENT_BYTES: usize = 5_000_000;
/// 未绑定附件的保留时长(孤儿扫描;避免刚上传就被清理)。
pub const ATTACHMENT_ORPHAN_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// 本地图片选择后允许读取的原始文件上限(压缩前)。
pub const MAX_LOCAL_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// 单张图片尺寸上限(防御异常输入;不用于缩放)。
pub const MAX_IMAGE_EDGE: i64 = 20_000;

pub const ALLOWED_MIMES: [&str; 3] = ["image/jpeg", "image/png", "image/webp"];

/// 附件元数据(上传结果/消息回看用;不含字节与绝对路径)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AttachmentView {
    pub id: String,
    pub document_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub mime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    pub width: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    pub height: Option<i64>,
    #[ts(type = "number")]
    pub byte_size: i64,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// `attachment_thumbnail` 返回:展示用 data URL 只经 IPC。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct AttachmentThumbnail {
    pub id: String,
    pub mime: String,
    /// data:image/...;base64,...(仅 IPC,不写数据库)。
    pub data_url: String,
}

/// `attachment_import_local` 返回(用户经系统对话框选择的本地图片)。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "src/shared/generated/")]
pub struct LocalImageData {
    pub mime: String,
    pub data_url: String,
}

/// 数据库行(内部;storage_key 不进入 DTO)。
#[derive(Debug, Clone)]
pub(crate) struct AttachmentRow {
    pub id: String,
    pub document_id: String,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
    pub mime: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub sha256: String,
    pub byte_size: i64,
    pub storage_key: String,
    pub name: Option<String>,
    pub created_at: i64,
}

impl AttachmentRow {
    pub(crate) fn to_view(&self) -> AttachmentView {
        AttachmentView {
            id: self.id.clone(),
            document_id: self.document_id.clone(),
            conversation_id: self.conversation_id.clone(),
            message_id: self.message_id.clone(),
            mime: self.mime.clone(),
            width: self.width,
            height: self.height,
            byte_size: self.byte_size,
            sha256: self.sha256.clone(),
            name: self.name.clone(),
            created_at: self.created_at,
        }
    }
}

pub(crate) const ATTACHMENT_COLS: &str = "id, document_id, conversation_id, message_id, mime, \
     width, height, sha256, byte_size, storage_key, name, created_at";

/// 去掉 `![alt](url)` 图片片段(含历史 data URI 与 attachment 引用)。
/// 只用于展示文本;模型输入由 question 独立提供。
pub fn strip_markdown_images(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        let (before, tail) = rest.split_at(start);
        out.push_str(before);
        // alt 以 "](" 结束;URL 以第一个 ")" 结束(data-URI 字符集不含 ")")。
        let Some(alt_end) = tail[2..].find("](") else {
            out.push_str(tail);
            return out;
        };
        let after_alt = &tail[2 + alt_end + 2..];
        let Some(url_end) = after_alt.find(')') else {
            out.push_str(tail);
            return out;
        };
        rest = &after_alt[url_end + 1..];
    }
    out.push_str(rest);
    out
}

/// 用户消息展示/落库文本:服务端只接受纯文本 + 已校验的附件 ID。
/// 客户端传入的图片 Markdown(尤其 base64)一律剥离,再追加轻量引用。
pub fn build_user_message_text(display_text: &str, attachment_ids: &[String]) -> String {
    let mut parts: Vec<String> = vec![strip_markdown_images(display_text).trim().to_string()];
    parts.extend(
        attachment_ids
            .iter()
            .map(|id| format!("![截图](attachment:{id})")),
    );
    parts.retain(|part| !part.is_empty());
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_never_contains_client_base64() {
        let legacy = "看这张图\n\n![截图](data:image/png;base64,AAAA)";
        assert_eq!(build_user_message_text(legacy, &[]), "看这张图".to_string());
        assert_eq!(
            build_user_message_text(legacy, &["att-1".into(), "att-2".into()]),
            "看这张图\n\n![截图](attachment:att-1)\n\n![截图](attachment:att-2)"
        );
        assert!(!build_user_message_text(legacy, &["att-1".into()]).contains("base64"));
    }

    #[test]
    fn title_source_strips_attachment_references() {
        assert_eq!(
            strip_markdown_images("预算怎么算?\n\n![截图](attachment:att-1)"),
            "预算怎么算?\n\n"
        );
    }
}
