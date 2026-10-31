//! 脱敏日志基线(M00,验收 M00-A4)。
//! 统一 formatter 只输出白名单字段;正文、提示词、密钥、完整路径等
//! 禁止字段在 layer 层被丢弃,不依赖调用点自觉。

use std::fmt;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// 允许进入日志输出的字段白名单(全小写比较)。
pub const ALLOWED_FIELDS: &[&str] = &[
    "message",
    "trace_id",
    "traceid",
    "task_id",
    "taskid",
    "attempt_id",
    "attemptid",
    "document_id",
    "documentid",
    "document_version_id",
    "documentversionid",
    "generation_id",
    "generationid",
    "block_id",
    "blockid",
    "error_code",
    "errorcode",
    "duration_ms",
    "durationms",
    "status",
    "phase",
    "event",
    "count",
    "module",
    "version",
    // 请求形状布尔量(非内容、非密钥):用于区分 5xx 是否与流式/结构化/工具请求相关。
    "stream",
    "structured",
    "tool_step",
];

/// 禁止字段黑名单:出现即丢弃(防御性双保险,白名单已足够)。
pub fn is_field_allowed(name: &str) -> bool {
    ALLOWED_FIELDS.contains(&name.to_ascii_lowercase().as_str())
}

/// 记录器:白名单字段照写,其余字段替换为 "[redacted:<name>]"。
struct RedactingVisitor<'a> {
    writer: Writer<'a>,
    result: fmt::Result,
}

impl<'a> Visit for RedactingVisitor<'a> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if self.result.is_err() {
            return;
        }
        if !is_field_allowed(field.name()) {
            // 敏感/未知字段只留下名字,不留值。
            self.result = write!(self.writer, " {}=[redacted]", field.name());
            return;
        }
        self.result = write!(self.writer, " {}={:?}", field.name(), value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_debug(field, &value)
    }
}

pub struct RedactingFormatter;

impl<S, N> FormatEvent<S, N> for RedactingFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        writeln!(writer, "{} {}", meta.level(), meta.target())?;
        let mut visitor = RedactingVisitor {
            writer: writer.by_ref(),
            result: Ok(()),
        };
        event.record(&mut visitor);
        visitor.result?;
        writeln!(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for SharedBuf {
        type Writer = SharedBuf;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
    impl Clone for SharedBuf {
        fn clone(&self) -> Self {
            Self(Arc::clone(&self.0))
        }
    }

    fn capture(f: impl FnOnce()) -> String {
        let buf = SharedBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .event_format(RedactingFormatter)
            .with_writer(buf.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = buf.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn sensitive_fields_are_redacted_and_allowed_kept() {
        // M00-A4:写入正文/密钥/路径的尝试必须被过滤。
        let out = capture(|| {
            tracing::info!(
                prompt = "SECRET-PROMPT",
                content = "机密正文",
                secret = "sk-123",
                path = "C:/data/example.pdf",
                task_id = "t1",
                error_code = "PARSE_FAILED",
                duration_ms = 42,
                "op completed"
            );
        });
        assert!(!out.contains("SECRET-PROMPT"), "prompt leaked: {out}");
        assert!(!out.contains("机密正文"), "content leaked: {out}");
        assert!(!out.contains("sk-123"), "secret leaked: {out}");
        assert!(!out.contains("doc.pdf"), "path leaked: {out}");
        assert!(out.contains("prompt=[redacted]"));
        assert!(out.contains("content=[redacted]"));
        assert!(out.contains("task_id=\"t1\""));
        assert!(out.contains("error_code=\"PARSE_FAILED\""));
        assert!(out.contains("duration_ms=42"));
        assert!(out.contains("op completed"), "message 字段保留");
    }
}
