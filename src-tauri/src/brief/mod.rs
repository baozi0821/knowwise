//! 阅读简报导出:原文引用(标注锚点原文)+ 问答(用户问题/回答分段/引用)+ 标注。
//! 只聚合数据库真实数据,不补充推断内容;格式为单文件 Markdown 或 HTML。
use crate::app_shell::AppState;
use crate::shared::dto::ReadingBriefExportResult;
use crate::shared::{AppError, CoreErrorCode};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingBriefExportRequest {
    pub document_id: String,
    /// markdown | html
    pub format: String,
    pub path: String,
}

#[derive(Debug, Default)]
struct BriefData {
    title: String,
    format: String,
    annotations: Vec<BriefAnnotation>,
    qa: Vec<BriefQa>,
}

#[derive(Debug)]
struct BriefAnnotation {
    where_: String,
    quote: String,
    color: String,
    note: String,
}

#[derive(Debug, Default)]
struct BriefQa {
    question: String,
    answer: String,
    citations: Vec<String>,
}

fn brief_error(reason: &'static str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
        .with_detail("reason", reason.into())
}

fn strip_encoded_note(note: &str) -> String {
    let trimmed = note.trim();
    if trimmed.starts_with("ink:v1:") {
        return String::new();
    }
    if let Some(payload) = trimmed.strip_prefix("textbox:v1:") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
            if let Some(text) = value.get("t").and_then(|v| v.as_str()) {
                return text.to_string();
            }
        }
        return String::new();
    }
    trimmed
        .strip_prefix("style:underline:")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| trimmed.to_string())
}

fn locator_label(json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return "文档".into();
    };
    match value.get("kind").and_then(|v| v.as_str()) {
        Some("pdf") => value
            .get("pageNumber")
            .and_then(|v| v.as_i64())
            .map(|n| format!("第 {n} 页"))
            .unwrap_or_else(|| "PDF".into()),
        Some("markdown") | Some("docx") => value
            .get("headingPath")
            .and_then(|v| v.as_array())
            .and_then(|items| items.last())
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "正文".into()),
        _ => "文档".into(),
    }
}

fn load_brief(state: &AppState, document_id: &str) -> Result<BriefData, AppError> {
    let id = document_id.to_string();
    state.db.read(move |conn| {
        use rusqlite::OptionalExtension;
        let (title, format): (String, String) = conn
            .query_row(
                "SELECT display_name, format FROM document WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| crate::persistence::connection::db_app_error(&e))?
            .ok_or_else(|| {
                AppError::new(CoreErrorCode::DocNotFound, crate::shared::new_trace_id())
            })?;
        let mut data = BriefData {
            title,
            format,
            annotations: Vec::new(),
            qa: Vec::new(),
        };
        {
            let mut stmt = conn
                .prepare(
                    "SELECT a.color, a.note_text, an.exact_text, an.primary_locator_json \
                     FROM annotation a JOIN anchor an ON an.id = a.anchor_id \
                     WHERE a.document_id=?1 ORDER BY a.created_at, a.id",
                )
                .map_err(|e| crate::persistence::connection::db_app_error(&e))?;
            let rows = stmt
                .query_map([&id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(|e| crate::persistence::connection::db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| crate::persistence::connection::db_app_error(&e))?;
            for (color, note, quote, locator) in rows {
                data.annotations.push(BriefAnnotation {
                    where_: locator_label(&locator),
                    quote,
                    color,
                    note: strip_encoded_note(&note),
                });
            }
        }
        {
            let mut stmt = conn
                .prepare(
                    "SELECT m.role, m.content_json FROM message m \
                     JOIN conversation c ON c.id = m.conversation_id \
                     WHERE c.document_id=?1 ORDER BY c.created_at, m.created_at, m.id",
                )
                .map_err(|e| crate::persistence::connection::db_app_error(&e))?;
            let rows = stmt
                .query_map([&id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| crate::persistence::connection::db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| crate::persistence::connection::db_app_error(&e))?;
            let mut current: Option<BriefQa> = None;
            for (role, content_json) in rows {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&content_json) else {
                    continue;
                };
                let schema = value
                    .get("schemaVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if schema == "user_message.v1" {
                    if let Some(qa) = current.take() {
                        if !qa.answer.trim().is_empty() {
                            data.qa.push(qa);
                        }
                    }
                    current = Some(BriefQa {
                        question: value
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        ..Default::default()
                    });
                    continue;
                }
                if schema == "answer.v1" {
                    let Some(content) = value.get("content") else {
                        continue;
                    };
                    let answer = content
                        .get("segments")
                        .and_then(|v| v.as_array())
                        .map(|segments| {
                            segments
                                .iter()
                                .filter_map(|segment| segment.get("text").and_then(|v| v.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n\n")
                        })
                        .unwrap_or_default();
                    let citations = content
                        .get("citations")
                        .and_then(|v| v.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.get("quotedText").and_then(|v| v.as_str()))
                                .map(|s| s.to_string())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if let Some(qa) = current.as_mut() {
                        if !answer.trim().is_empty() {
                            qa.answer = answer;
                            qa.citations.extend(citations);
                        }
                    }
                    let _ = role;
                }
            }
            if let Some(qa) = current.take() {
                if !qa.answer.trim().is_empty() {
                    data.qa.push(qa);
                }
            }
        }
        Ok(data)
    })
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_markdown(data: &BriefData) -> String {
    let mut out = String::new();
    out.push_str(&format!("# 阅读简报 · {}\n\n", data.title));
    out.push_str(&format!(
        "- 格式:{}\n- 标注:{} 条\n- 问答:{} 轮\n- 说明:以下内容来自真实标注锚点原文与已保存回答,不含推断。\n\n",
        data.format,
        data.annotations.len(),
        data.qa.len()
    ));
    out.push_str("## 原文引用与标注\n\n");
    if data.annotations.is_empty() {
        out.push_str("(无标注)\n\n");
    }
    for annotation in &data.annotations {
        out.push_str(&format!("### {}\n\n", annotation.where_));
        out.push_str(&format!("> {}\n\n", annotation.quote.replace('\n', "\n> ")));
        out.push_str(&format!("- 颜色:{}\n", annotation.color));
        if !annotation.note.is_empty() {
            out.push_str(&format!("- 备注:{}\n", annotation.note));
        }
        out.push('\n');
    }
    out.push_str("## 问答\n\n");
    if data.qa.is_empty() {
        out.push_str("(无已保存问答)\n");
    }
    for (index, qa) in data.qa.iter().enumerate() {
        out.push_str(&format!("### {}. {}\n\n", index + 1, qa.question.trim()));
        out.push_str(&format!("{}\n\n", qa.answer.trim()));
        if !qa.citations.is_empty() {
            out.push_str("引用:\n\n");
            for citation in &qa.citations {
                out.push_str(&format!("- 「{}」\n", citation.replace('\n', " ")));
            }
            out.push('\n');
        }
    }
    out
}

fn render_html(data: &BriefData) -> String {
    let mut out = String::from(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>阅读简报</title><style>body{font-family:system-ui,\"Segoe UI\",sans-serif;max-width:860px;margin:40px auto;padding:0 20px;line-height:1.7;color:#1f2937}h1{font-size:24px}h2{margin-top:32px;border-bottom:1px solid #e5e7eb;padding-bottom:4px}h3{font-size:16px;margin-top:20px}blockquote{margin:8px 0;padding:6px 12px;border-left:3px solid #c7cbd4;color:#4b5563;background:#f8f8f6}.meta{color:#6b7280;font-size:13px}.cite{color:#6b7280;font-size:13px}pre{white-space:pre-wrap}</style></head><body>",
    );
    out.push_str(&format!("<h1>阅读简报 · {}</h1>", escape_html(&data.title)));
    out.push_str(&format!(
        "<p class=\"meta\">格式:{} · 标注 {} 条 · 问答 {} 轮 · 内容来自真实标注锚点原文与已保存回答,不含推断。</p>",
        escape_html(&data.format),
        data.annotations.len(),
        data.qa.len()
    ));
    out.push_str("<h2>原文引用与标注</h2>");
    if data.annotations.is_empty() {
        out.push_str("<p class=\"meta\">(无标注)</p>");
    }
    for annotation in &data.annotations {
        out.push_str(&format!("<h3>{}</h3>", escape_html(&annotation.where_)));
        out.push_str(&format!(
            "<blockquote>{}</blockquote>",
            escape_html(&annotation.quote)
        ));
        out.push_str(&format!(
            "<p class=\"meta\">颜色:{} · 备注:{}</p>",
            escape_html(&annotation.color),
            escape_html(if annotation.note.is_empty() {
                "—"
            } else {
                &annotation.note
            })
        ));
    }
    out.push_str("<h2>问答</h2>");
    if data.qa.is_empty() {
        out.push_str("<p class=\"meta\">(无已保存问答)</p>");
    }
    for (index, qa) in data.qa.iter().enumerate() {
        out.push_str(&format!(
            "<h3>{}. {}</h3><pre>{}</pre>",
            index + 1,
            escape_html(qa.question.trim()),
            escape_html(qa.answer.trim())
        ));
        if !qa.citations.is_empty() {
            out.push_str("<p class=\"cite\">引用:<br>");
            for citation in &qa.citations {
                out.push_str(&format!("「{}」<br>", escape_html(citation)));
            }
            out.push_str("</p>");
        }
    }
    out.push_str("</body></html>");
    out
}

#[tauri::command]
pub async fn reading_brief_export(
    state: tauri::State<'_, AppState>,
    request: ReadingBriefExportRequest,
) -> Result<ReadingBriefExportResult, AppError> {
    let format = request.format.to_lowercase();
    if format != "markdown" && format != "html" {
        return Err(brief_error("unsupported_brief_format"));
    }
    let data = load_brief(&state, &request.document_id)?;
    let body = if format == "html" {
        render_html(&data)
    } else {
        render_markdown(&data)
    };
    let path = std::path::PathBuf::from(&request.path);
    let bytes = body.into_bytes();
    let written = bytes.len() as u64;
    tauri::async_runtime::spawn_blocking(move || std::fs::write(&path, &bytes))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .map_err(|e| {
            AppError::new(CoreErrorCode::Internal, crate::shared::new_trace_id())
                .with_detail("reason", "brief_write_failed".into())
                .with_detail("message", e.to_string().into())
        })?;
    Ok(ReadingBriefExportResult {
        bytes_written: written,
        format,
        annotation_count: data.annotations.len() as i64,
        qa_count: data.qa.len() as i64,
    })
}
