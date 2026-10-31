//! Ollama 本地适配器(《M08》§2、《03》§3.6;可选的本地测试路径)。
//!
//! 用户主链路为自有远程 API(openai_compatible);本适配器保留默认本地
//! 链路:仅连接 `127.0.0.1` 配置端口(设置校验保证回环),无鉴权。
//! 端点:`/api/tags`、`/api/chat`(stream=false)、`/api/embed`。

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::shared::stream::StreamDelta;

use super::errors::ProviderError;
use super::models::{ModelDescriptor, ProcessingLocation, ProviderCapability, ProviderKind};
use super::openai_compatible::{read_capped, wrap_timeout_cancel};
use super::provider::{
    DeltaSink, EmbedRequest, EmbedSuccess, GenerateRequest, GenerateSuccess,
    GenerateWithToolsRequest, GenerateWithToolsSuccess, ModelProvider, ProbeResult,
    ProviderAttachment, ProviderFut, ProviderMessage, ProviderTransport, ToolCall, ToolSpec,
};
/// 流式内容的去向(与 OpenAI 适配器一致;结构化 JSON 仅内部解析)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentEmit {
    Answer,
    StructuredJson,
    None,
}

impl ContentEmit {
    fn delta(self, text: String) -> Option<crate::shared::stream::StreamDelta> {
        match self {
            Self::Answer => Some(crate::shared::stream::StreamDelta::answer(text)),
            Self::StructuredJson => Some(crate::shared::stream::StreamDelta::structured_json(text)),
            Self::None => None,
        }
    }
}

pub struct OllamaProvider {
    id: String,
    display_name: String,
}

pub const OLLAMA_CAPABILITIES: [ProviderCapability; 7] = [
    ProviderCapability::Generate,
    ProviderCapability::Embed,
    ProviderCapability::StructuredOutput,
    ProviderCapability::Streaming,
    ProviderCapability::Cancel,
    ProviderCapability::Vision,
    ProviderCapability::ToolCalling,
];

impl OllamaProvider {
    pub fn new(id: &str, display_name: &str) -> Self {
        Self {
            id: id.to_string(),
            display_name: display_name.to_string(),
        }
    }
}

impl ModelProvider for OllamaProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::Ollama
    }
    fn location(&self) -> ProcessingLocation {
        ProcessingLocation::Local
    }
    fn display_name(&self) -> &str {
        &self.display_name
    }
    fn declared_capabilities(&self) -> &'static [ProviderCapability] {
        &OLLAMA_CAPABILITIES
    }

    fn probe(self: Arc<Self>, transport: ProviderTransport) -> ProviderFut<ProbeResult> {
        Box::pin(async move {
            let url = format!("{}/api/tags", transport.base_url.trim_end_matches('/'));
            let resp = wrap_timeout_cancel(
                transport.http.get(&url).send(),
                &transport,
                CancellationToken::new(),
            )
            .await?;
            let status = resp.status();
            if !status.is_success() {
                return Err(ProviderError::ServerError(status.as_u16()));
            }
            let body = read_capped(resp, transport.max_response_bytes).await?;
            let parsed: TagsResponse =
                serde_json::from_slice(&body).map_err(|_| ProviderError::InvalidOutput)?;
            let models = parsed
                .models
                .into_iter()
                .map(|m| ModelDescriptor {
                    id: m.name,
                    display_name: None,
                    capabilities: Vec::new(),
                })
                .collect();
            Ok(ProbeResult {
                reachable: true,
                models,
                supports_structured_output: true,
            })
        })
    }

    fn generate(
        self: Arc<Self>,
        transport: ProviderTransport,
        request: GenerateRequest,
        cancel: CancellationToken,
        on_delta: DeltaSink,
    ) -> ProviderFut<GenerateSuccess> {
        Box::pin(async move {
            let structured = request.json_schema.is_some();
            let body = ChatBody {
                model: request.model_id.clone(),
                messages: request.messages.iter().map(chat_message).collect(),
                stream: true,
                // 结构化输出:Ollama 的 JSON 模式;schema 校验在本地。
                format: structured.then(|| "json".to_string()),
                tools: None,
            };
            let outcome = send_chat(
                &transport,
                &body,
                cancel,
                &on_delta,
                if structured {
                    ContentEmit::StructuredJson
                } else {
                    ContentEmit::Answer
                },
            )
            .await?;
            let mut content = outcome.content;
            if let Some(schema) = &request.json_schema {
                content = super::openai_compatible::normalize_structured_output(&content, schema)?;
            }
            Ok(GenerateSuccess {
                content,
                model_id: outcome.model_id.unwrap_or(request.model_id),
                prompt_tokens: outcome.prompt_tokens,
                completion_tokens: outcome.completion_tokens,
            })
        })
    }

    fn generate_with_tools(
        self: Arc<Self>,
        transport: ProviderTransport,
        request: GenerateWithToolsRequest,
        cancel: CancellationToken,
        on_delta: DeltaSink,
    ) -> ProviderFut<GenerateWithToolsSuccess> {
        Box::pin(async move {
            let structured = request.json_schema.is_some();
            let tool_step = !request.tools.is_empty();
            let body = ChatBody {
                model: request.model_id.clone(),
                messages: request.messages.iter().map(chat_message).collect(),
                stream: true,
                // 工具调用与 JSON 模式不能同时声明;收敛请求(无工具)才请求 JSON。
                format: (request.tools.is_empty() && structured).then(|| "json".to_string()),
                tools: tool_step.then(|| request.tools.iter().map(ollama_tool).collect::<Vec<_>>()),
            };
            let outcome = send_chat(
                &transport,
                &body,
                cancel,
                &on_delta,
                if tool_step {
                    ContentEmit::None
                } else if structured {
                    ContentEmit::StructuredJson
                } else {
                    ContentEmit::Answer
                },
            )
            .await?;
            let mut content = outcome.content;
            let tool_calls = outcome.tool_calls;
            if tool_calls.is_empty() {
                if let Some(schema) = &request.json_schema {
                    content =
                        super::openai_compatible::normalize_structured_output(&content, schema)?;
                }
            }
            Ok(GenerateWithToolsSuccess {
                content,
                tool_calls,
                model_id: outcome.model_id.unwrap_or(request.model_id),
                prompt_tokens: outcome.prompt_tokens,
                completion_tokens: outcome.completion_tokens,
            })
        })
    }

    fn embed(
        self: Arc<Self>,
        transport: ProviderTransport,
        request: EmbedRequest,
        cancel: CancellationToken,
    ) -> ProviderFut<EmbedSuccess> {
        Box::pin(async move {
            let body = EmbedBody {
                model: request.model_id.clone(),
                input: request.texts.clone(),
            };
            let url = format!("{}/api/embed", transport.base_url.trim_end_matches('/'));
            let resp = wrap_timeout_cancel(
                transport.http.post(&url).json(&body).send(),
                &transport,
                cancel,
            )
            .await?;
            let status = resp.status();
            if !status.is_success() {
                return Err(ProviderError::ServerError(status.as_u16()));
            }
            let bytes = read_capped(resp, transport.max_response_bytes).await?;
            let parsed: EmbedResponse =
                serde_json::from_slice(&bytes).map_err(|_| ProviderError::InvalidOutput)?;
            if parsed.embeddings.len() != request.texts.len() {
                return Err(ProviderError::InvalidOutput);
            }
            let dims = parsed
                .embeddings
                .first()
                .map(|v| v.len() as i64)
                .unwrap_or(0);
            if dims == 0 || parsed.embeddings.iter().any(|v| v.len() as i64 != dims) {
                return Err(ProviderError::InvalidOutput);
            }
            if let Some(expected) = request.expected_dimensions {
                if expected != dims {
                    return Err(ProviderError::InvalidOutput);
                }
            }
            Ok(EmbedSuccess {
                vectors: parsed.embeddings,
                dimensions: dims,
                model_id: parsed.model.unwrap_or(request.model_id),
            })
        })
    }
}

#[derive(Debug, Default)]
struct OllamaOutcome {
    content: String,
    tool_calls: Vec<ToolCall>,
    model_id: Option<String>,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
}

/// 发送 `/api/chat` 并按 NDJSON 增量消费;`emit_answer=false` 时只上抛 thinking。
async fn send_chat(
    transport: &ProviderTransport,
    body: &ChatBody,
    cancel: CancellationToken,
    on_delta: &DeltaSink,
    emit: ContentEmit,
) -> Result<OllamaOutcome, ProviderError> {
    let url = format!("{}/api/chat", transport.base_url.trim_end_matches('/'));
    let resp = wrap_timeout_cancel(
        transport.http.post(&url).json(body).send(),
        transport,
        cancel.clone(),
    )
    .await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ProviderError::ServerError(status.as_u16()));
    }
    consume_ndjson(resp, transport, cancel, on_delta, emit).await
}

async fn consume_ndjson(
    mut resp: reqwest::Response,
    transport: &ProviderTransport,
    cancel: CancellationToken,
    on_delta: &DeltaSink,
    emit: ContentEmit,
) -> Result<OllamaOutcome, ProviderError> {
    let started = Instant::now();
    let timeout = Duration::from_millis(transport.request_timeout_ms.clamp(1_000, 600_000));
    let mut buf: Vec<u8> = Vec::new();
    let mut out = OllamaOutcome::default();
    let mut received = 0usize;
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(ProviderError::Timeout);
        };
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            res = tokio::time::timeout(remaining, resp.chunk()) => match res {
                Ok(Ok(Some(chunk))) => chunk,
                Ok(Ok(None)) => break,
                Ok(Err(_)) => return Err(ProviderError::Unreachable),
                Err(_) => return Err(ProviderError::Timeout),
            }
        };
        received += chunk.len();
        if received > transport.max_response_bytes {
            return Err(ProviderError::ResponseTooLarge);
        }
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|byte| *byte == b'\n') {
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            if process_ollama_line(
                line.trim_end_matches(['\r', '\n']),
                &mut out,
                on_delta,
                emit,
            )? {
                return Ok(out);
            }
        }
    }
    if !buf.is_empty() {
        let line = String::from_utf8_lossy(&buf);
        let _ = process_ollama_line(
            line.trim_end_matches(['\r', '\n']),
            &mut out,
            on_delta,
            emit,
        )?;
    }
    Ok(out)
}

/// 处理一行 NDJSON:返回 true 表示 `done=true`。
fn process_ollama_line(
    line: &str,
    out: &mut OllamaOutcome,
    on_delta: &DeltaSink,
    emit: ContentEmit,
) -> Result<bool, ProviderError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(false);
    }
    let parsed: ChatResponseLine =
        serde_json::from_str(line).map_err(|_| ProviderError::InvalidOutput)?;
    if parsed.model.is_some() {
        out.model_id = parsed.model;
    }
    if parsed.prompt_eval_count.is_some() {
        out.prompt_tokens = parsed.prompt_eval_count;
    }
    if parsed.eval_count.is_some() {
        out.completion_tokens = parsed.eval_count;
    }
    let ChatResponseMessage {
        content,
        thinking,
        tool_calls,
    } = parsed.message;
    if let Some(thinking) = thinking.filter(|text| !text.is_empty()) {
        on_delta(StreamDelta::reasoning(thinking));
    }
    if !content.is_empty() {
        if let Some(delta) = emit.delta(content.clone()) {
            on_delta(delta);
        }
        out.content.push_str(&content);
    }
    if let Some(calls) = tool_calls {
        out.tool_calls = calls
            .iter()
            .enumerate()
            .map(|(index, call)| ToolCall {
                id: call
                    .id
                    .clone()
                    .filter(|id| !id.trim().is_empty())
                    .unwrap_or_else(|| format!("call_{index}")),
                name: call.function.name.clone(),
                arguments_json: ollama_arguments_json(&call.function.arguments),
            })
            .collect();
    }
    Ok(parsed.done)
}

#[derive(Debug, Deserialize)]
struct ChatResponseLine {
    #[serde(default)]
    message: ChatResponseMessage,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<i64>,
    #[serde(default)]
    eval_count: Option<i64>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
    /// Ollama 协议:裸 base64 数组;纯文本消息不出现该字段。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
    /// assistant 工具调用(Ollama: function.arguments 为对象)。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OllamaToolCall>,
}

fn chat_message(message: &ProviderMessage) -> ChatMessage {
    ChatMessage {
        role: message.role.as_str().to_string(),
        content: message.content.clone(),
        images: message
            .attachments
            .iter()
            .map(|attachment: &ProviderAttachment| BASE64.encode(&attachment.bytes))
            .collect(),
        tool_calls: message
            .tool_calls
            .iter()
            .map(|call| OllamaToolCall {
                function: OllamaFunction {
                    name: call.name.clone(),
                    arguments: ollama_arguments_value(&call.arguments_json),
                },
            })
            .collect(),
    }
}

#[derive(Debug, Serialize)]
struct OllamaToolCall {
    function: OllamaFunction,
}

#[derive(Debug, Serialize)]
struct OllamaFunction {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct OllamaTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OllamaToolSpec,
}

#[derive(Debug, Serialize)]
struct OllamaToolSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

fn ollama_tool(tool: &ToolSpec) -> OllamaTool {
    OllamaTool {
        kind: "function",
        function: OllamaToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        },
    }
}

/// 内部 arguments 是 JSON 字符串;Ollama 要求对象。非法/空值降级为 `{}`。
fn ollama_arguments_value(raw: &str) -> serde_json::Value {
    let parsed: serde_json::Value =
        serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({}));
    if parsed.is_object() {
        parsed
    } else {
        serde_json::json!({})
    }
}

/// Ollama 响应的 arguments 可能是对象或 JSON 字符串;统一为协议层字符串。
fn ollama_arguments_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(raw) => {
            if serde_json::from_str::<serde_json::Value>(raw).is_ok() {
                raw.clone()
            } else {
                "{}".into()
            }
        }
        serde_json::Value::Null => "{}".into(),
        other => other.to_string(),
    }
}

#[derive(Debug, Serialize)]
struct ChatBody {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OllamaTool>>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatResponseMessage {
    #[serde(default)]
    content: String,
    /// 思考模式增量(先于 content;没有该字段时不伪造)。
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OllamaResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseToolCall {
    #[serde(default)]
    id: Option<String>,
    function: OllamaResponseFunction,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseFunction {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct EmbedBody {
    model: String,
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<TagsModel>,
}

#[derive(Debug, Deserialize)]
struct TagsModel {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_only_message_omits_images() {
        let message =
            ProviderMessage::text(super::super::provider::ProviderMessageRole::User, "你好");
        assert_eq!(
            serde_json::to_value(chat_message(&message)).unwrap(),
            serde_json::json!({"role": "user", "content": "你好"})
        );
    }

    #[test]
    fn ndjson_parser_separates_thinking_and_suppresses_json_answer() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let sink: DeltaSink = Arc::new(move |delta| {
            sink_events.lock().unwrap().push(delta);
        });
        let mut out = OllamaOutcome::default();
        let thinking = r#"{"message":{"thinking":"先读第 12 页。"},"done":false}"#;
        assert!(!process_ollama_line(thinking, &mut out, &sink, ContentEmit::None).unwrap());
        let content = r#"{"message":{"content":"{\"answer\":1}"},"done":false}"#;
        assert!(!process_ollama_line(content, &mut out, &sink, ContentEmit::None).unwrap());
        let done = r#"{"message":{"content":""},"done":true,"eval_count":3}"#;
        assert!(process_ollama_line(done, &mut out, &sink, ContentEmit::None).unwrap());
        assert_eq!(out.completion_tokens, Some(3));
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].channel,
            crate::shared::stream::StreamChannel::Reasoning
        );
        assert_eq!(events[0].text, "先读第 12 页。");
    }

    #[test]
    fn attachment_message_uses_bare_base64_images() {
        let message = ProviderMessage {
            role: super::super::provider::ProviderMessageRole::User,
            content: "看图".into(),
            attachments: vec![ProviderAttachment {
                mime: "image/png".into(),
                bytes: vec![1, 2, 3],
                name: None,
            }],
            tool_calls: Vec::new(),
            tool_call_id: None,
        };
        assert_eq!(
            serde_json::to_value(chat_message(&message)).unwrap(),
            serde_json::json!({"role": "user", "content": "看图", "images": ["AQID"]})
        );
    }
}
