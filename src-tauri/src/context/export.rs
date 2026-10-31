//! M07 会话导出(Markdown / JSON,WBS-6/P0 收尾)。
//!
//! 导出是只读投影:消息角色/时间、快照范围摘要、answer.v1 分段与引用、
//! 附件展示图。JSON 是规范格式;Markdown 同时提供可读正文和可解析的
//! 机器附录。图片只以 data URI 内嵌,不写 storage_key、绝对路径或密钥。

use std::collections::HashMap;
use std::path::Path;

use crate::shared::{AppError, CoreErrorCode};

use super::models::{ConversationView, MessageContent, MessageRole, MessageView};

pub const CONVERSATION_EXPORT_SCHEMA_VERSION: &str = "conversation_export.v1";
pub const CONVERSATION_EXPORT_MANIFEST_VERSION: &str = "conversation_export.manifest.v1";

/// 导出用附件投影:data_url 为 verified 字节的 base64 data URI;
/// 校验失败时为 None(导出仍继续,但正文和 JSON 会明确标注不可用)。
#[derive(Debug, Clone, PartialEq)]
pub struct ExportAttachment {
    pub id: String,
    pub message_id: Option<String>,
    pub mime: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub sha256: String,
    pub byte_size: i64,
    pub name: Option<String>,
    pub data_url: Option<String>,
}

/// 命令层从 M07/M15 读取后交给纯渲染函数的快照。
#[derive(Debug, Clone)]
pub struct ConversationExportData {
    pub conversation: ConversationView,
    pub messages: Vec<MessageView>,
    pub attachments: Vec<ExportAttachment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationExportFormat {
    Markdown,
    Json,
}

impl ConversationExportFormat {
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "markdown" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            _ => Err(
                AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                    .with_detail("reason", "unsupported_format".into())
                    .with_detail("format", raw.to_string().into()),
            ),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

pub fn render(
    data: &ConversationExportData,
    format: ConversationExportFormat,
) -> Result<String, AppError> {
    match format {
        ConversationExportFormat::Json => {
            let value = export_value(data, CONVERSATION_EXPORT_SCHEMA_VERSION, true);
            serde_json::to_string_pretty(&value).map_err(|e| AppError::internal(e.to_string()))
        }
        ConversationExportFormat::Markdown => Ok(render_markdown(data)),
    }
}

fn export_value(
    data: &ConversationExportData,
    schema_version: &str,
    include_data_urls: bool,
) -> serde_json::Value {
    let by_message = attachments_by_message(&data.attachments);
    let messages: Vec<serde_json::Value> = data
        .messages
        .iter()
        .map(|message| {
            let attachment_list =
                attachments_value(by_message.get(message.id.as_str()), include_data_urls);
            let mut value = serde_json::json!({
                "id": message.id,
                "role": message.role.as_str(),
                "createdAt": message.created_at,
                "createdAtIso": utc_datetime(message.created_at),
                "updatedAt": message.updated_at,
                "answerStatus": message.answer_status.as_str(),
                "content": serde_json::to_value(&message.content).unwrap_or(serde_json::Value::Null),
                "attachments": attachment_list,
            });
            if let Some(snapshot) = &message.snapshot {
                value["snapshot"] = serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null);
            }
            if let Some(task_status) = &message.task_status {
                value["taskStatus"] = serde_json::Value::String(task_status.clone());
            }
            if let Some(error_code) = &message.error_code {
                value["errorCode"] = serde_json::Value::String(error_code.clone());
            }
            if let Some(note) = &message.context_note {
                value["contextNote"] = serde_json::Value::String(note.clone());
            }
            value
        })
        .collect();
    serde_json::json!({
        "schemaVersion": schema_version,
        "conversation": {
            "id": data.conversation.id,
            "documentId": data.conversation.document_id,
            "title": data.conversation.title,
            "messageCount": data.conversation.message_count,
            "createdAt": data.conversation.created_at,
            "createdAtIso": utc_datetime(data.conversation.created_at),
            "updatedAt": data.conversation.updated_at,
            "updatedAtIso": utc_datetime(data.conversation.updated_at),
        },
        "messages": messages,
    })
}

fn attachments_by_message(
    attachments: &[ExportAttachment],
) -> HashMap<&str, Vec<&ExportAttachment>> {
    let mut out: HashMap<&str, Vec<&ExportAttachment>> = HashMap::new();
    for attachment in attachments {
        if let Some(message_id) = attachment.message_id.as_deref() {
            out.entry(message_id).or_default().push(attachment);
        }
    }
    out
}

fn attachments_value(
    attachments: Option<&Vec<&ExportAttachment>>,
    include_data_urls: bool,
) -> serde_json::Value {
    let Some(attachments) = attachments else {
        return serde_json::Value::Array(Vec::new());
    };
    serde_json::Value::Array(
        attachments
            .iter()
            .map(|attachment| {
                let mut value = serde_json::json!({
                    "id": attachment.id,
                    "mime": attachment.mime,
                    "width": attachment.width,
                    "height": attachment.height,
                    "sha256": attachment.sha256,
                    "byteSize": attachment.byte_size,
                    "name": attachment.name,
                    "available": attachment.data_url.is_some(),
                });
                if include_data_urls {
                    value["dataUrl"] = match &attachment.data_url {
                        Some(data_url) => serde_json::Value::String(data_url.clone()),
                        None => serde_json::Value::Null,
                    };
                } else {
                    value["embeddedInBody"] =
                        serde_json::Value::Bool(attachment.data_url.is_some());
                }
                value
            })
            .collect(),
    )
}

fn render_markdown(data: &ConversationExportData) -> String {
    let by_message = attachments_by_message(&data.attachments);
    let mut out = String::new();
    out.push_str(&format!("# 会话:{}\n\n", inline(&data.conversation.title)));
    out.push_str(&format!(
        "- 会话 ID:{}\n- 文档 ID:{}\n- 创建时间:{} UTC\n- 更新时间:{} UTC\n- 消息数:{}\n\n",
        data.conversation.id,
        data.conversation.document_id,
        utc_datetime(data.conversation.created_at),
        utc_datetime(data.conversation.updated_at),
        data.conversation.message_count
    ));
    for (index, message) in data.messages.iter().enumerate() {
        out.push_str(&format!(
            "## {}. {} · {} UTC\n\n",
            index + 1,
            role_label(message.role),
            utc_datetime(message.created_at)
        ));
        if let Some(snapshot) = &message.snapshot {
            out.push_str(&format!(
                "范围:{}（{}）\n\n",
                inline(&snapshot.name),
                snapshot.scope_kind.as_str()
            ));
        }
        let attachments = by_message.get(message.id.as_str());
        match &message.content {
            MessageContent::UserMessage { text } => {
                out.push_str(&render_user_text(text, attachments));
                out.push_str("\n\n");
                render_extra_attachments(&mut out, attachments, text);
            }
            MessageContent::AssistantPlaceholder {} => {
                out.push_str(&format!(
                    "未产出答案（{}）\n\n",
                    message.answer_status.as_str()
                ));
            }
            MessageContent::Answer { content } => {
                render_answer(&mut out, content);
            }
            MessageContent::AgentAnswer { content } => {
                out.push_str(&format!(
                    "模式:深度研究 · {}\n\n",
                    if content.verified {
                        "最终答复已校验"
                    } else {
                        "最终答复未校验"
                    }
                ));
                render_answer(&mut out, &content.answer);
                if !content.steps.is_empty() {
                    out.push_str(&format!(
                        "工具步骤:{} 步(详见 JSON 附录)\n\n",
                        content.steps.len()
                    ));
                }
            }
            MessageContent::AssistantNotice { notice } => {
                out.push_str(&format!(
                    "未产出答案（{}）:{}\n\n",
                    if notice.fallback_used {
                        "兜底"
                    } else {
                        "通知"
                    },
                    inline(&notice.message)
                ));
            }
        }
        if let Some(error_code) = &message.error_code {
            out.push_str(&format!("错误码:{error_code}\n\n"));
        }
        if let Some(note) = &message.context_note {
            out.push_str(&format!("上下文说明:{}\n\n", inline(note)));
        }
        out.push_str("---\n\n");
    }
    let manifest = export_value(data, CONVERSATION_EXPORT_MANIFEST_VERSION, false);
    out.push_str("## 机器可读附录\n\n");
    out.push_str("以下清单与上方正文等价;图片已内嵌在用户消息正文中,不重复存放字节。\n\n");
    out.push_str("```json\n");
    out.push_str(&serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| "{}".to_string()));
    out.push_str("\n```\n");
    out
}

fn render_answer(out: &mut String, content: &super::models::AnswerContent) {
    out.push_str(&format!("状态:{}\n\n", answer_status_label(content.status)));
    for segment in &content.segments {
        let badge = match segment.kind {
            super::models::SegmentKind::DocumentFact => "",
            super::models::SegmentKind::Inference => "【推断】",
            super::models::SegmentKind::Example => "【示例】",
            super::models::SegmentKind::UserView => "【用户观点】",
            super::models::SegmentKind::GeneralNote => "【说明】",
        };
        out.push_str(&format!("{badge}{}\n\n", segment.text));
    }
    if !content.citations.is_empty() {
        out.push_str("引用来源:\n\n");
        for citation in &content.citations {
            out.push_str(&format!(
                "- [`{}`] 「{}」\n",
                citation.block_id,
                inline(&citation.quoted_text)
            ));
        }
        out.push('\n');
    }
    for conflict in &content.conflicts {
        out.push_str(&format!("- 冲突:{}\n", inline(&conflict.description)));
    }
    if !content.unverified_notes.is_empty() {
        out.push_str(&format!(
            "- 未验证说明:{}\n",
            content
                .unverified_notes
                .iter()
                .map(|note| inline(note))
                .collect::<Vec<_>>()
                .join(";")
        ));
    }
    out.push('\n');
}

fn render_user_text(text: &str, attachments: Option<&Vec<&ExportAttachment>>) -> String {
    let mut by_id: HashMap<&str, &ExportAttachment> = HashMap::new();
    if let Some(attachments) = attachments {
        for attachment in attachments {
            by_id.insert(attachment.id.as_str(), attachment);
        }
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        let (before, tail) = rest.split_at(start);
        out.push_str(before);
        let Some(alt_end) = tail[2..].find("](") else {
            out.push_str(tail);
            return out;
        };
        let after_alt = &tail[2 + alt_end + 2..];
        let Some(url_end) = after_alt.find(')') else {
            out.push_str(tail);
            return out;
        };
        let url = &after_alt[..url_end];
        let alt = &tail[2..2 + alt_end];
        if let Some(id) = url.strip_prefix("attachment:") {
            if let Some(attachment) = by_id.get(id) {
                match &attachment.data_url {
                    Some(data_url) => out.push_str(&format!("![{alt}]({data_url})")),
                    None => out.push_str(&format!("[图片不可用:{id}]")),
                }
            } else {
                out.push_str(&format!("[图片不可用:{id}]"));
            }
        } else {
            out.push_str(&tail[..2 + alt_end + 2 + url_end + 1]);
        }
        rest = &after_alt[url_end + 1..];
    }
    out.push_str(rest);
    out
}

fn render_extra_attachments(
    out: &mut String,
    attachments: Option<&Vec<&ExportAttachment>>,
    original_text: &str,
) {
    let Some(attachments) = attachments else {
        return;
    };
    for attachment in attachments {
        if !attachment_referenced(original_text, &attachment.id) {
            match &attachment.data_url {
                Some(data_url) => {
                    out.push_str(&format!(
                        "![{}]({})\n\n",
                        attachment.name.as_deref().unwrap_or("附件"),
                        data_url
                    ));
                }
                None => out.push_str(&format!("[图片不可用:{}]\n\n", attachment.id)),
            }
        }
    }
}

fn attachment_referenced(text: &str, id: &str) -> bool {
    text.contains(&format!("attachment:{id}"))
}

fn inline(text: &str) -> String {
    text.replace(['\n', '\r'], " ").trim().to_string()
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "用户",
        MessageRole::Assistant => "助手",
    }
}

fn answer_status_label(status: super::models::AnswerStatus) -> &'static str {
    match status {
        super::models::AnswerStatus::Answered => "已回答",
        super::models::AnswerStatus::InsufficientEvidence => "证据不足(以下内容仅为可核对线索)",
        super::models::AnswerStatus::Conflict => "文档内存在冲突,不做裁决",
        super::models::AnswerStatus::OutOfScope => "超出当前快照范围",
    }
}

/// UTC 日期(1970 起的天数 → 年月日 civil 算法;不依赖本机时区)。
fn utc_date(epoch_ms: i64) -> String {
    let secs = epoch_ms.div_euclid(1_000);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn utc_datetime(epoch_ms: i64) -> String {
    let date = utc_date(epoch_ms);
    let secs = epoch_ms.div_euclid(1_000);
    let sod = secs.rem_euclid(86_400);
    format!(
        "{date}T{:02}:{:02}:{:02}Z",
        sod / 3_600,
        (sod % 3_600) / 60,
        sod % 60
    )
}

/// 写导出文件:已存在绝不覆盖;错误只返回稳定码,不回显 OS 错误或路径。
pub fn write_export_file(path: &Path, bytes: &[u8]) -> Result<i64, AppError> {
    if path.as_os_str().is_empty() {
        return Err(
            AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
                .with_detail("reason", "export_path_empty".into()),
        );
    }
    if path.exists() {
        return Err(AppError::new(
            CoreErrorCode::ExportTargetExists,
            crate::shared::new_trace_id(),
        )
        .with_detail("reason", "target_exists".into()));
    }
    match std::fs::write(path, bytes) {
        Ok(()) => Ok(bytes.len() as i64),
        Err(error) => match error.kind() {
            std::io::ErrorKind::PermissionDenied => Err(AppError::new(
                CoreErrorCode::ExportPermissionDenied,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "permission_denied".into())),
            std::io::ErrorKind::StorageFull => Err(AppError::new(
                CoreErrorCode::ExportDiskFull,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "disk_full".into())),
            _ => Err(
                AppError::new(CoreErrorCode::Internal, crate::shared::new_trace_id())
                    .with_detail("reason", "export_write_failed".into()),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::models::{
        AnswerCitation, AnswerContent, AnswerLifecycle, AnswerSegment, AnswerStatus,
        MessageContent, MessageRole, MessageView, ScopeKind, SegmentKind, SnapshotBrief,
    };

    fn conversation() -> ConversationView {
        ConversationView {
            id: "conv-1".into(),
            document_id: "doc-1".into(),
            title: "预算讨论".into(),
            active_snapshot_id: None,
            active_snapshot: None,
            message_count: 2,
            pinned: false,
            created_at: 1_000,
            updated_at: 2_000,
        }
    }

    fn message(
        id: &str,
        role: MessageRole,
        content: MessageContent,
        answer_status: AnswerLifecycle,
    ) -> MessageView {
        MessageView {
            id: id.into(),
            conversation_id: "conv-1".into(),
            document_id: "doc-1".into(),
            role,
            snapshot_id: None,
            snapshot: Some(SnapshotBrief {
                snapshot_id: "snap-1".into(),
                scope_kind: ScopeKind::Section,
                name: "第 2 节".into(),
                coverage: None,
            }),
            task_id: None,
            task_status: None,
            content,
            answer_status,
            model_provider_id: None,
            model_id: None,
            error_code: None,
            reasoning: None,
            context_note: None,
            created_at: 1_000,
            updated_at: 1_000,
        }
    }

    fn data() -> ConversationExportData {
        let answer = AnswerContent {
            schema_version: "answer.v1".into(),
            status: AnswerStatus::Answered,
            segments: vec![AnswerSegment {
                id: "s1".into(),
                kind: SegmentKind::DocumentFact,
                text: "预算为 120 万。".into(),
                verification_status: "structurally_valid".into(),
                citation_ids: vec!["c1".into()],
            }],
            citations: vec![AnswerCitation {
                id: "c1".into(),
                block_id: "blk-1".into(),
                quoted_text: "预算为 120 万。".into(),
                claim_segment_ids: vec!["s1".into()],
            }],
            conflicts: vec![],
            suggestions: vec![],
            unverified_notes: vec![],
        };
        ConversationExportData {
            conversation: conversation(),
            messages: vec![
                message(
                    "m1",
                    MessageRole::User,
                    MessageContent::UserMessage {
                        text: "请看这张图 ![截图](attachment:att-1)".into(),
                    },
                    AnswerLifecycle::Completed,
                ),
                message(
                    "m2",
                    MessageRole::Assistant,
                    MessageContent::Answer { content: answer },
                    AnswerLifecycle::Completed,
                ),
            ],
            attachments: vec![ExportAttachment {
                id: "att-1".into(),
                message_id: Some("m1".into()),
                mime: "image/png".into(),
                width: Some(10),
                height: Some(10),
                sha256: "a".repeat(64),
                byte_size: 3,
                name: Some("截图.png".into()),
                data_url: Some("data:image/png;base64,AAAA".into()),
            }],
        }
    }

    #[test]
    fn json_export_is_reparseable_and_has_no_absolute_paths() {
        let text = render(&data(), ConversationExportFormat::Json).unwrap();
        assert!(!text.contains("storage_key"));
        assert!(!text.contains("C:\\"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schemaVersion"], CONVERSATION_EXPORT_SCHEMA_VERSION);
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(
            value["messages"][1]["content"]["schemaVersion"],
            "answer.v1"
        );
        assert_eq!(
            value["messages"][1]["content"]["content"]["citations"][0]["quotedText"],
            "预算为 120 万。"
        );
        assert_eq!(
            value["messages"][0]["attachments"][0]["dataUrl"],
            "data:image/png;base64,AAAA"
        );
        assert_eq!(value["conversation"]["messageCount"], 2);
    }

    #[test]
    fn markdown_export_embeds_images_and_has_parseable_appendix() {
        let text = render(&data(), ConversationExportFormat::Markdown).unwrap();
        assert!(text.contains("# 会话:预算讨论"));
        assert!(text.contains("data:image/png;base64,AAAA"));
        assert!(text.contains("引用来源"));
        assert!(text.contains("预算为 120 万。"));
        assert!(!text.contains("C:\\"));
        let appendix = text.split("## 机器可读附录").nth(1).expect("appendix");
        let json_start = appendix.find("```json").unwrap() + "```json".len();
        let json_end = appendix[json_start..].find("```").unwrap() + json_start;
        let manifest: serde_json::Value =
            serde_json::from_str(appendix[json_start..json_end].trim()).unwrap();
        assert_eq!(
            manifest["schemaVersion"],
            CONVERSATION_EXPORT_MANIFEST_VERSION
        );
        assert_eq!(
            manifest["messages"][0]["attachments"][0]["embeddedInBody"],
            true
        );
    }

    #[test]
    fn unknown_export_format_is_rejected_with_stable_reason() {
        let error = ConversationExportFormat::parse("pdf").unwrap_err();
        assert_eq!(error.code, "INVALID_REQUEST");
        assert!(format!("{:?}", error.details).contains("unsupported_format"));
    }

    #[test]
    fn unavailable_attachment_is_explicit_not_silent() {
        let mut data = data();
        data.attachments[0].data_url = None;
        let markdown = render(&data, ConversationExportFormat::Markdown).unwrap();
        assert!(markdown.contains("[图片不可用:att-1]"));
        let json = render(&data, ConversationExportFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["messages"][0]["attachments"][0]["available"], false);
        assert!(value["messages"][0]["attachments"][0]["dataUrl"].is_null());
    }

    #[test]
    fn utc_datetime_is_stable_and_timezone_independent() {
        assert_eq!(utc_datetime(0), "1970-01-01T00:00:00Z");
        // 2025-10-23T12:34:56Z
        assert_eq!(utc_datetime(1_761_222_896_000), "2025-10-23T12:34:56Z");
    }

    #[test]
    fn export_writer_refuses_overwrite_without_leaking_path() {
        let dir = std::env::temp_dir().join(format!("kw-conv-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("会话.json");
        let written = write_export_file(&path, b"{\"ok\":true}").unwrap();
        assert_eq!(written, 11);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"ok\":true}");
        let error = write_export_file(&path, b"again").unwrap_err();
        assert_eq!(error.code, "EXPORT_TARGET_EXISTS");
        assert!(!format!("{error:?}").contains(dir.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
