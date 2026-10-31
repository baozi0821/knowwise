//! OpenAI 兼容远程适配器(《M08》§2、《03》§3.6;用户自有 API)。
//!
//! 协议按标准 OpenAI 规范实现(用户未提供差异说明前的缺省选择):
//! - 探测:`GET {base}/models`
//! - 生成:`POST {base}/chat/completions`(结构化输出请求
//!   `response_format={"type":"json_object"}`;schema 基础校验在本地)
//! - 嵌入:`POST {base}/embeddings`(维度与期望不一致整批失败)
//! - 鉴权:`Authorization: Bearer <key>`(密钥由网关注入,不落盘)
//!
//! 硬边界:URL 只来自设置的 base_url;客户端禁重定向/禁代理继承;
//! 响应体有上限;供应商错误正文不进入任何错误对象(只留状态码)。
//! 首版为非流式请求(能力表不含 streaming);取消通过丢弃请求 future
//! 中断传输,适配器收到取消令牌即时退出。

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::shared::stream::{StreamChannel, StreamDelta};

use super::errors::ProviderError;
use super::models::{ModelDescriptor, ProcessingLocation, ProviderCapability, ProviderKind};
use super::provider::{
    DeltaSink, EmbedRequest, EmbedSuccess, GenerateRequest, GenerateSuccess,
    GenerateWithToolsRequest, GenerateWithToolsSuccess, ModelProvider, ProbeResult,
    ProviderAttachment, ProviderFut, ProviderMessage, ProviderTransport, ToolCall, ToolSpec,
};

/// 流式内容的去向:纯文本答案 / 结构化 JSON(仅内部解析)/ 不展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentEmit {
    Answer,
    StructuredJson,
    None,
}

impl ContentEmit {
    fn delta(self, text: String) -> Option<StreamDelta> {
        match self {
            Self::Answer => Some(StreamDelta {
                channel: StreamChannel::Answer,
                text,
            }),
            Self::StructuredJson => Some(StreamDelta {
                channel: StreamChannel::StructuredJson,
                text,
            }),
            Self::None => None,
        }
    }
}

pub struct OpenAiCompatibleProvider {
    id: String,
    display_name: String,
}

/// 适配器声明的能力:支持 SSE 增量(reasoning/content 分流)。
pub const OPENAI_COMPATIBLE_CAPABILITIES: [ProviderCapability; 7] = [
    ProviderCapability::Generate,
    ProviderCapability::Embed,
    ProviderCapability::StructuredOutput,
    ProviderCapability::Streaming,
    ProviderCapability::Cancel,
    ProviderCapability::Vision,
    ProviderCapability::ToolCalling,
];

impl OpenAiCompatibleProvider {
    pub fn new(id: &str, display_name: &str) -> Self {
        Self {
            id: id.to_string(),
            display_name: display_name.to_string(),
        }
    }

    pub fn parse_error_status(status: reqwest::StatusCode) -> Option<ProviderError> {
        match status.as_u16() {
            401 | 403 => Some(ProviderError::AuthRejected),
            404 | 400 => Some(ProviderError::ModelNotFound),
            429 => Some(ProviderError::QuotaExhausted),
            s if s >= 500 => Some(ProviderError::ServerError(s)),
            _ => None,
        }
    }
}

impl ModelProvider for OpenAiCompatibleProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenaiCompatible
    }
    fn location(&self) -> ProcessingLocation {
        ProcessingLocation::Remote
    }
    fn display_name(&self) -> &str {
        &self.display_name
    }
    fn declared_capabilities(&self) -> &'static [ProviderCapability] {
        &OPENAI_COMPATIBLE_CAPABILITIES
    }

    fn probe(self: Arc<Self>, transport: ProviderTransport) -> ProviderFut<ProbeResult> {
        Box::pin(async move {
            let url = format!("{}/models", transport.base_url.trim_end_matches('/'));
            let mut req = transport.http.get(&url);
            if let Some(bearer) = &transport.bearer {
                req = req.bearer_auth(bearer);
            }
            let resp =
                wrap_timeout_cancel(req.send(), &transport, CancellationToken::new()).await?;
            let status = resp.status();
            if let Some(e) = Self::parse_error_status(status) {
                return Err(e);
            }
            if !status.is_success() {
                return Err(ProviderError::ServerError(status.as_u16()));
            }
            let body = read_capped(resp, transport.max_response_bytes).await?;
            let parsed: ModelsResponse =
                serde_json::from_slice(&body).map_err(|_| ProviderError::InvalidOutput)?;
            let models = parsed
                .data
                .into_iter()
                .map(|m| ModelDescriptor {
                    id: m.id,
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
            let mut body = ChatCompletionsBody {
                model: request.model_id.clone(),
                messages: request.messages.iter().map(chat_message).collect(),
                stream: true,
                response_format: None,
                tools: None,
                tool_choice: None,
            };
            if structured {
                body.response_format = Some(ResponseFormat {
                    r#type: "json_object".into(),
                });
            }
            let outcome = send_with_non_stream_fallback(
                &transport,
                &body,
                cancel,
                &on_delta,
                // 结构化 JSON 不直接展示;内部通道供增量解析,纯文本才直接显示。
                if structured {
                    ContentEmit::StructuredJson
                } else {
                    ContentEmit::Answer
                },
            )
            .await?;
            let mut content = outcome.content;
            // 本地最终校验与归一化:兼容代码块/前后缀包裹的 JSON。
            // 纯文本(未按 schema 输出)不在此丢弃:用结构化降级信封保留正文,
            // 由编排层标注“未校验”,保证任何提问都有可见回复。
            if let Some(schema) = &request.json_schema {
                content = normalize_structured_output(&content, schema)
                    .unwrap_or_else(|_| plain_text_envelope(&content));
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
            let mut body = ChatCompletionsBody {
                model: request.model_id.clone(),
                messages: request.messages.iter().map(chat_message).collect(),
                stream: true,
                response_format: None,
                tools: None,
                tool_choice: None,
            };
            if tool_step {
                body.tools = Some(request.tools.iter().map(chat_tool).collect());
                body.tool_choice = Some("auto".into());
            }
            if structured {
                body.response_format = Some(ResponseFormat {
                    r#type: "json_object".into(),
                });
            }
            let outcome = send_with_non_stream_fallback(
                &transport,
                &body,
                cancel,
                &on_delta,
                // 工具步骤的中间文本不是最终答案;收敛请求是结构化 JSON。
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
            // 收敛请求(无工具)仍按结构化输出校验并归一化最终 JSON。
            if tool_calls.is_empty() {
                if let Some(schema) = &request.json_schema {
                    content = normalize_structured_output(&content, schema)
                        .unwrap_or_else(|_| plain_text_envelope(&content));
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
            let url = format!("{}/embeddings", transport.base_url.trim_end_matches('/'));
            let body = EmbeddingsBody {
                model: request.model_id.clone(),
                input: request.texts.clone(),
            };
            let mut req = transport.http.post(&url).json(&body);
            if let Some(bearer) = &transport.bearer {
                req = req.bearer_auth(bearer);
            }
            let resp = wrap_timeout_cancel(req.send(), &transport, cancel).await?;
            let status = resp.status();
            if let Some(e) = Self::parse_error_status(status) {
                return Err(e);
            }
            if !status.is_success() {
                return Err(ProviderError::ServerError(status.as_u16()));
            }
            let bytes = read_capped(resp, transport.max_response_bytes).await?;
            let parsed: EmbeddingsResponse =
                serde_json::from_slice(&bytes).map_err(|_| ProviderError::InvalidOutput)?;
            // 按 index 还原顺序(规范允许乱序返回)。
            let mut items = parsed.data;
            items.sort_by_key(|d| d.index);
            if items.len() != request.texts.len() {
                return Err(ProviderError::InvalidOutput);
            }
            let dims = items.first().map(|d| d.embedding.len() as i64).unwrap_or(0);
            if dims == 0 || items.iter().any(|d| d.embedding.len() as i64 != dims) {
                return Err(ProviderError::InvalidOutput);
            }
            if let Some(expected) = request.expected_dimensions {
                if expected != dims {
                    // 维度不一致整批失败,不截断/补零(《01》§8.2)。
                    return Err(ProviderError::InvalidOutput);
                }
            }
            Ok(EmbedSuccess {
                vectors: items.into_iter().map(|d| d.embedding).collect(),
                dimensions: dims,
                model_id: parsed.model.unwrap_or(request.model_id),
            })
        })
    }
}

/// 请求超时 + 取消 + 连接错误归一化。供应商错误正文永不进入错误对象。
pub(crate) async fn wrap_timeout_cancel(
    fut: impl std::future::Future<Output = Result<reqwest::Response, reqwest::Error>>,
    transport: &ProviderTransport,
    cancel: CancellationToken,
) -> Result<reqwest::Response, ProviderError> {
    let timeout =
        std::time::Duration::from_millis(transport.request_timeout_ms.clamp(1_000, 600_000));
    tokio::select! {
        out = tokio::time::timeout(timeout, fut) => match out {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => {
                if e.is_timeout() {
                    Err(ProviderError::Timeout)
                } else {
                    Err(ProviderError::Unreachable)
                }
            }
            Err(_) => Err(ProviderError::Timeout),
        },
        _ = cancel.cancelled() => Err(ProviderError::Cancelled),
    }
}

/// 读取响应体并施加字节上限;超限立即中断(不无限缓冲)。
pub(crate) async fn read_capped(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, ProviderError> {
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return Err(ProviderError::ResponseTooLarge);
        }
    }
    let mut resp = resp;
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|_| ProviderError::Unreachable)? {
        if buf.len() + chunk.len() > max_bytes {
            return Err(ProviderError::ResponseTooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// 单次流式调用的聚合结果(最终仍以适配器返回的整体内容为准)。
#[derive(Debug, Default)]
struct ChatOutcome {
    content: String,
    tool_calls: Vec<ToolCall>,
    tool_acc: Vec<ToolCallAcc>,
    model_id: Option<String>,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
}

#[derive(Debug, Default)]
struct ToolCallAcc {
    id: Option<String>,
    name: String,
    arguments: String,
}

impl ChatOutcome {
    fn finalize_tools(&mut self) {
        self.tool_calls = std::mem::take(&mut self.tool_acc)
            .into_iter()
            .enumerate()
            .filter(|(_, acc)| !acc.name.trim().is_empty())
            .map(|(index, acc)| ToolCall {
                id: acc
                    .id
                    .filter(|id| !id.trim().is_empty())
                    .unwrap_or_else(|| format!("call_{index}")),
                name: acc.name,
                arguments_json: if acc.arguments.trim().is_empty() {
                    "{}".into()
                } else {
                    acc.arguments
                },
            })
            .collect();
    }
}

/// 流式请求被提供方拒绝(400/404)时自动退回非流式,避免兼容性回归。
/// 只对 ModelNotFound 重试:鉴权/额度/超时不做二次请求。
async fn send_with_non_stream_fallback(
    transport: &ProviderTransport,
    body: &ChatCompletionsBody,
    cancel: CancellationToken,
    on_delta: &DeltaSink,
    emit: ContentEmit,
) -> Result<ChatOutcome, ProviderError> {
    match send_chat_completion(transport, body, cancel.clone(), on_delta, emit).await {
        Err(ProviderError::ModelNotFound) if body.stream => {
            tracing::warn!("streaming request rejected; retrying without stream");
            let fallback = ChatCompletionsBody {
                stream: false,
                ..body.clone()
            };
            send_chat_completion(transport, &fallback, cancel, on_delta, emit).await
        }
        other => other,
    }
}

/// chat/completions 的自动重试上限(总尝试次数)。只重试"尚未收到任何响应内容"
/// 的连接失败与提供方 5xx;一旦进入 SSE/JSON 正文消费就不再重试,
/// 避免重复计费或把半截回答拼成成功。
const CHAT_ATTEMPT_LIMIT: u8 = 2;
const CHAT_RETRY_DELAY_MS: u64 = 400;
/// 客户端 connect_timeout 为 5s,与整体请求超时共用 `ProviderError::Timeout`;
/// 失败明显早于整体超时说明发生在连接阶段(请求尚未被处理),可安全重试。
const CONNECT_FAILURE_WINDOW_MS: u128 = 10_000;

fn chat_request(
    transport: &ProviderTransport,
    body: &ChatCompletionsBody,
) -> reqwest::RequestBuilder {
    let url = format!(
        "{}/chat/completions",
        transport.base_url.trim_end_matches('/')
    );
    let mut req = transport.http.post(&url).json(body);
    if let Some(bearer) = &transport.bearer {
        req = req.bearer_auth(bearer);
    }
    req
}

/// 是否属于"响应到达前"的瞬态失败(可安全重试一次)。
fn retryable_before_response(err: &ProviderError, elapsed: Duration) -> bool {
    match err {
        ProviderError::Unreachable => true,
        ProviderError::Timeout => elapsed.as_millis() < CONNECT_FAILURE_WINDOW_MS,
        ProviderError::ServerError(status) => *status >= 500,
        _ => false,
    }
}

/// 取消感知的退避;返回 false 表示等待期间被取消。
async fn retry_backoff(cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(Duration::from_millis(CHAT_RETRY_DELAY_MS)) => !cancel.is_cancelled(),
    }
}

/// 发送 chat/completions:优先按 SSE 增量消费;提供方不支持流式时回退整包 JSON。
/// `emit` 决定内容去向;结构化 JSON 走内部通道,绝不直接展示。
async fn send_chat_completion(
    transport: &ProviderTransport,
    body: &ChatCompletionsBody,
    cancel: CancellationToken,
    on_delta: &DeltaSink,
    emit: ContentEmit,
) -> Result<ChatOutcome, ProviderError> {
    let started = Instant::now();
    let mut attempt: u8 = 0;
    loop {
        attempt += 1;
        let resp = match wrap_timeout_cancel(
            chat_request(transport, body).send(),
            transport,
            cancel.clone(),
        )
        .await
        {
            Ok(resp) => resp,
            Err(error) => {
                if attempt < CHAT_ATTEMPT_LIMIT
                    && retryable_before_response(&error, started.elapsed())
                {
                    tracing::warn!(
                        event = "provider_request_retry",
                        phase = if matches!(error, ProviderError::Timeout) {
                            "connect_timeout"
                        } else {
                            "unreachable"
                        },
                        count = i64::from(attempt),
                        "retrying chat request after transport failure"
                    );
                    if retry_backoff(&cancel).await {
                        continue;
                    }
                    return Err(ProviderError::Cancelled);
                }
                return Err(error);
            }
        };
        let status = resp.status();
        if let Some(error) = OpenAiCompatibleProvider::parse_error_status(status) {
            // 只记录状态码与非敏感元数据,绝不记录供应商正文。
            tracing::warn!(
                status = status.as_u16(),
                stream = body.stream,
                structured = body.response_format.is_some(),
                tool_step = body.tools.is_some(),
                "provider chat request rejected"
            );
            if attempt < CHAT_ATTEMPT_LIMIT && retryable_before_response(&error, started.elapsed())
            {
                tracing::warn!(
                    event = "provider_request_retry",
                    phase = "server_error",
                    count = i64::from(attempt),
                    "retrying chat request after provider error"
                );
                if retry_backoff(&cancel).await {
                    continue;
                }
                return Err(ProviderError::Cancelled);
            }
            return Err(error);
        }
        if !status.is_success() {
            tracing::warn!(status = status.as_u16(), "provider chat request failed");
            return Err(ProviderError::ServerError(status.as_u16()));
        }
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/event-stream"));
        tracing::info!(
            event = "provider_response",
            status = status.as_u16(),
            phase = if is_sse { "sse" } else { "json" }
        );
        return if is_sse {
            consume_sse(resp, transport, cancel, on_delta, emit).await
        } else {
            consume_json(resp, transport, on_delta, emit).await
        };
    }
}

async fn consume_json(
    resp: reqwest::Response,
    transport: &ProviderTransport,
    on_delta: &DeltaSink,
    emit: ContentEmit,
) -> Result<ChatOutcome, ProviderError> {
    let bytes = read_capped(resp, transport.max_response_bytes).await?;
    let parsed: ChatCompletionsResponse = serde_json::from_slice(&bytes).map_err(|_| {
        tracing::warn!(
            event = "provider_json_parse_failed",
            count = bytes.len() as i64
        );
        ProviderError::InvalidOutput
    })?;
    let choice = parsed.choices.first().ok_or(ProviderError::InvalidOutput)?;
    let out = ChatOutcome {
        content: choice.message.content.clone().unwrap_or_default(),
        model_id: parsed.model,
        prompt_tokens: parsed.usage.as_ref().and_then(|u| u.prompt_tokens),
        completion_tokens: parsed.usage.as_ref().and_then(|u| u.completion_tokens),
        tool_calls: tool_calls_from_message(&choice.message),
        ..ChatOutcome::default()
    };
    if let Some(reasoning) = reasoning_of_message(&choice.message) {
        on_delta(StreamDelta::reasoning(reasoning));
    }
    if !out.content.is_empty() {
        if let Some(delta) = emit.delta(out.content.clone()) {
            on_delta(delta);
        }
    }
    Ok(out)
}

fn reasoning_of_message(message: &ChatResponseMessage) -> Option<&str> {
    message
        .reasoning_content
        .as_deref()
        .or(message.reasoning.as_deref())
        .or(message.thinking.as_deref())
        .filter(|text| !text.is_empty())
}

async fn consume_sse(
    mut resp: reqwest::Response,
    transport: &ProviderTransport,
    cancel: CancellationToken,
    on_delta: &DeltaSink,
    emit: ContentEmit,
) -> Result<ChatOutcome, ProviderError> {
    let started = Instant::now();
    let timeout = Duration::from_millis(transport.request_timeout_ms.clamp(1_000, 600_000));
    let mut buf: Vec<u8> = Vec::new();
    let mut out = ChatOutcome::default();
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
            if process_sse_line(
                line.trim_end_matches(['\r', '\n']),
                &mut out,
                on_delta,
                emit,
            )? {
                out.finalize_tools();
                return Ok(out);
            }
        }
    }
    if !buf.is_empty() {
        let line = String::from_utf8_lossy(&buf);
        if process_sse_line(
            line.trim_end_matches(['\r', '\n']),
            &mut out,
            on_delta,
            emit,
        )? {
            out.finalize_tools();
            return Ok(out);
        }
    }
    out.finalize_tools();
    Ok(out)
}

/// 处理一行 SSE:返回 true 表示 `[DONE]`。
fn process_sse_line(
    line: &str,
    out: &mut ChatOutcome,
    on_delta: &DeltaSink,
    emit: ContentEmit,
) -> Result<bool, ProviderError> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return Ok(false);
    }
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(false);
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(false);
    }
    if data == "[DONE]" {
        return Ok(true);
    }
    let chunk: ChatStreamChunk =
        serde_json::from_str(data).map_err(|_| ProviderError::InvalidOutput)?;
    if chunk.model.is_some() {
        out.model_id = chunk.model;
    }
    if let Some(usage) = chunk.usage {
        out.prompt_tokens = usage.prompt_tokens;
        out.completion_tokens = usage.completion_tokens;
    }
    for choice in chunk.choices {
        let delta = choice.delta;
        if let Some(reasoning) = delta
            .reasoning_content
            .or(delta.reasoning)
            .or(delta.thinking)
            .filter(|text| !text.is_empty())
        {
            on_delta(StreamDelta {
                channel: StreamChannel::Reasoning,
                text: reasoning,
            });
        }
        if let Some(content) = delta.content.filter(|text| !text.is_empty()) {
            if let Some(event) = emit.delta(content.clone()) {
                on_delta(event);
            }
            out.content.push_str(&content);
        }
        if let Some(calls) = delta.tool_calls {
            for call in calls {
                let index = call.index.unwrap_or(0);
                while out.tool_acc.len() <= index {
                    out.tool_acc.push(ToolCallAcc::default());
                }
                let acc = &mut out.tool_acc[index];
                if let Some(id) = call.id.filter(|id| !id.trim().is_empty()) {
                    acc.id = Some(id);
                }
                if let Some(function) = call.function {
                    if let Some(name) = function.name.filter(|name| !name.is_empty()) {
                        acc.name.push_str(&name);
                    }
                    if let Some(arguments) = function.arguments {
                        acc.arguments.push_str(&arguments);
                    }
                }
            }
        }
    }
    Ok(false)
}

#[derive(Debug, Deserialize)]
struct ChatStreamChunk {
    #[serde(default)]
    choices: Vec<ChatStreamChoice>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatStreamChoice {
    #[serde(default)]
    delta: ChatStreamDelta,
}

#[derive(Debug, Default, Deserialize)]
struct ChatStreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ChatStreamToolCall {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChatStreamToolFunction>,
}

#[derive(Debug, Deserialize)]
struct ChatStreamToolFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// 容错提取并归一化结构化输出:部分 OpenAI 兼容聚合服务会把 JSON
/// 包在 ```json 代码块或解释性前后缀中。这里只做 JSON 提取、const 字段
/// 注入与 required 键检查,不记录正文;失败返回 InvalidOutput,
/// 由上层走可见兜底。
pub(crate) fn normalize_structured_output(
    content: &str,
    schema: &serde_json::Value,
) -> Result<String, ProviderError> {
    let mut value = parse_json_tolerant(content).ok_or_else(|| {
        let trimmed = content.trim_start();
        let phase = if trimmed.starts_with("```") {
            "fenced"
        } else if trimmed.starts_with('{') {
            "json_like"
        } else {
            "text"
        };
        // 诊断只记录结构特征,不记录模型正文/密钥。
        tracing::warn!(
            event = "structured_output_invalid",
            phase = phase,
            count = content.len() as i64,
            has_open = content.contains('{'),
            has_close = content.contains('}'),
            line_count = content.lines().count() as i64,
            starts_with_brace = content.trim_start().starts_with('{')
        );
        ProviderError::InvalidOutput
    })?;
    // 模型经常省略固定常量字段(如 answer.v1 的 schemaVersion):
    // 按 schema 的 const 声明补齐,不改变语义,也不伪造可变字段。
    if let (Some(props), Some(obj)) = (
        schema.get("properties").and_then(|p| p.as_object()),
        value.as_object_mut(),
    ) {
        for (key, spec) in props {
            if !obj.contains_key(key) {
                if let Some(constant) = spec.get("const").and_then(|c| c.as_str()) {
                    obj.insert(key.clone(), serde_json::Value::String(constant.to_string()));
                }
            }
        }
    }
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        let obj = value.as_object().ok_or(ProviderError::InvalidOutput)?;
        for key in required {
            if let Some(k) = key.as_str() {
                if !obj.contains_key(k) {
                    // 只记录缺失的 schema 键名(稳定契约字段),不记录模型正文。
                    tracing::warn!(event = "structured_output_missing_key", phase = k);
                    return Err(ProviderError::InvalidOutput);
                }
            }
        }
    }
    Ok(serde_json::to_string(&value).unwrap_or_else(|_| content.to_string()))
}

/// 模型未按 schema 输出时的结构化降级信封:
/// 保留可见正文,显式标记 plain_text_fallback;绝不把原文包成已验证答案。
pub(crate) fn plain_text_envelope(content: &str) -> String {
    // 截断/损坏的结构化 JSON 不能原样进入用户正文:先提取可读文本;
    // 完全无内容时保留原文,由编排层决定是否显示错误。
    let readable = crate::shared::sanitize::salvage_readable_markdown(content);
    let text = if readable.trim().is_empty() {
        content.trim()
    } else {
        readable.trim()
    };
    serde_json::json!({
        "schemaVersion": "plain_text_fallback.v1",
        "text": text,
    })
    .to_string()
}

fn parse_json_tolerant(content: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        return Some(value);
    }
    let trimmed = content.trim();
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|rest| rest.strip_suffix("```"))
        .map(str::trim);
    if let Some(unfenced) = unfenced {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(unfenced) {
            return Some(value);
        }
    }
    if let (Some(start), Some(end)) = (content.find('{'), content.rfind('}')) {
        if end > start {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content[start..=end]) {
                return Some(value);
            }
        }
    }
    None
}

#[derive(Debug, Clone, Serialize)]
struct ResponseFormat {
    r#type: String,
}

#[derive(Debug, Clone, Serialize)]
struct ChatMessage {
    role: String,
    /// assistant 工具调用消息允许 content 为空(OpenAI: null)。
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<ChatContent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ChatToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: ChatToolFunction,
}

#[derive(Debug, Clone, Serialize)]
struct ChatToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize)]
struct ChatTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: ChatToolSpec,
}

#[derive(Debug, Clone, Serialize)]
struct ChatToolSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// 无附件 = 字符串(与旧版请求体逐字节一致);有附件 = 多段数组。
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum ChatContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlPart },
}

#[derive(Debug, Clone, Serialize)]
struct ImageUrlPart {
    url: String,
}

fn chat_message(message: &ProviderMessage) -> ChatMessage {
    // assistant 工具调用消息允许空 content;其余消息保持字符串/多段格式。
    let content = if message.role == super::provider::ProviderMessageRole::Assistant
        && !message.tool_calls.is_empty()
        && message.content.is_empty()
    {
        None
    } else if message.attachments.is_empty() {
        Some(ChatContent::Text(message.content.clone()))
    } else {
        let mut parts = vec![ContentPart::Text {
            text: message.content.clone(),
        }];
        parts.extend(message.attachments.iter().map(image_part));
        Some(ChatContent::Parts(parts))
    };
    ChatMessage {
        role: message.role.as_str().to_string(),
        content,
        tool_calls: message
            .tool_calls
            .iter()
            .map(|call| ChatToolCall {
                id: call.id.clone(),
                kind: "function",
                function: ChatToolFunction {
                    name: call.name.clone(),
                    arguments: call.arguments_json.clone(),
                },
            })
            .collect(),
        tool_call_id: message.tool_call_id.clone(),
    }
}

fn tool_calls_from_message(message: &ChatResponseMessage) -> Vec<ToolCall> {
    message
        .tool_calls
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .map(|(index, call)| ToolCall {
            id: call
                .id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| format!("call_{index}")),
            name: call.function.name.clone(),
            arguments_json: if call.function.arguments.trim().is_empty() {
                "{}".into()
            } else {
                call.function.arguments.clone()
            },
        })
        .collect()
}

fn chat_tool(tool: &ToolSpec) -> ChatTool {
    ChatTool {
        kind: "function",
        function: ChatToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        },
    }
}

fn image_part(attachment: &ProviderAttachment) -> ContentPart {
    ContentPart::ImageUrl {
        image_url: ImageUrlPart {
            url: format!(
                "data:{};base64,{}",
                attachment.mime,
                BASE64.encode(&attachment.bytes)
            ),
        },
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChatCompletionsBody {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ChatTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    #[serde(default)]
    content: Option<String>,
    /// 非流式回退时的推理字段兼容(DeepSeek/vLLM/部分兼容实现)。
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ChatResponseToolCall {
    #[serde(default)]
    id: Option<String>,
    function: ChatResponseFunction,
}

#[derive(Debug, Deserialize)]
struct ChatResponseFunction {
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<ChatChoice>,
    model: Option<String>,
    usage: Option<ChatUsage>,
}

#[derive(Debug, Clone, Serialize)]
struct EmbeddingsBody {
    model: String,
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    index: i64,
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelsItem>,
}

#[derive(Debug, Deserialize)]
struct ModelsItem {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::super::provider::noop_delta;
    use super::*;

    #[test]
    fn status_mapping_is_distinct() {
        assert_eq!(
            OpenAiCompatibleProvider::parse_error_status(reqwest::StatusCode::UNAUTHORIZED),
            Some(ProviderError::AuthRejected)
        );
        assert_eq!(
            OpenAiCompatibleProvider::parse_error_status(reqwest::StatusCode::NOT_FOUND),
            Some(ProviderError::ModelNotFound)
        );
        assert_eq!(
            OpenAiCompatibleProvider::parse_error_status(reqwest::StatusCode::TOO_MANY_REQUESTS),
            Some(ProviderError::QuotaExhausted)
        );
        assert_eq!(
            OpenAiCompatibleProvider::parse_error_status(reqwest::StatusCode::BAD_GATEWAY),
            Some(ProviderError::ServerError(502))
        );
        assert_eq!(
            OpenAiCompatibleProvider::parse_error_status(reqwest::StatusCode::OK),
            None
        );
    }

    #[test]
    fn text_only_message_keeps_legacy_string_content() {
        let message =
            ProviderMessage::text(super::super::provider::ProviderMessageRole::User, "你好");
        assert_eq!(
            serde_json::to_value(chat_message(&message)).unwrap(),
            serde_json::json!({"role": "user", "content": "你好"})
        );
    }

    #[test]
    fn attachment_message_uses_image_url_data_uri() {
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
        let value = serde_json::to_value(chat_message(&message)).unwrap();
        assert_eq!(
            value["content"],
            serde_json::json!([
                {"type": "text", "text": "看图"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AQID"}}
            ])
        );
    }

    #[test]
    fn assistant_tool_call_message_serializes_function_call() {
        let message = ProviderMessage {
            role: super::super::provider::ProviderMessageRole::Assistant,
            content: String::new(),
            attachments: Vec::new(),
            tool_calls: vec![ToolCall {
                id: "call-1".into(),
                name: "search_blocks".into(),
                arguments_json: "{\"query\":\"预算\"}".into(),
            }],
            tool_call_id: None,
        };
        let value = serde_json::to_value(chat_message(&message)).unwrap();
        assert!(value.get("content").is_none());
        assert_eq!(value["tool_calls"][0]["id"], "call-1");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "search_blocks");
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            "{\"query\":\"预算\"}"
        );
    }

    #[test]
    fn tool_result_message_carries_tool_call_id() {
        let message = ProviderMessage {
            role: super::super::provider::ProviderMessageRole::Tool,
            content: "{\"items\":[]}".into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-1".into()),
        };
        let value = serde_json::to_value(chat_message(&message)).unwrap();
        assert_eq!(value["role"], "tool");
        assert_eq!(value["tool_call_id"], "call-1");
    }

    #[test]
    fn tool_specs_serialize_as_openai_functions() {
        let tool = chat_tool(&ToolSpec {
            name: "read_pages".into(),
            description: "read pages".into(),
            parameters: serde_json::json!({"type": "object"}),
        });
        let value = serde_json::to_value(tool).unwrap();
        assert_eq!(value["type"], "function");
        assert_eq!(value["function"]["name"], "read_pages");
        assert_eq!(value["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn response_tool_calls_parse_and_default_missing_ids_arguments() {
        let parsed: ChatCompletionsResponse = serde_json::from_value(serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call-a",
                        "type": "function",
                        "function": {"name": "search_blocks", "arguments": "{\"query\":\"x\"}"}
                    }, {
                        "type": "function",
                        "function": {"name": "get_selection"}
                    }]
                }
            }]
        }))
        .unwrap();
        let calls = tool_calls_from_message(&parsed.choices[0].message);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "search_blocks");
        assert_eq!(calls[0].arguments_json, "{\"query\":\"x\"}");
        assert_eq!(calls[1].name, "get_selection");
        assert_eq!(calls[1].arguments_json, "{}");
        assert!(!calls[1].id.is_empty());
    }

    #[test]
    fn structured_output_validation_rejects_bad_json_and_missing_keys() {
        let schema = serde_json::json!({"required": ["answer"]});
        assert!(normalize_structured_output(r#"{"answer":"x"}"#, &schema).is_ok());
        assert!(normalize_structured_output("not json", &schema).is_err());
        assert!(normalize_structured_output(r#"{"other":1}"#, &schema).is_err());
    }

    #[test]
    fn normalize_structured_output_tolerates_fences_and_prefixes() {
        let schema = serde_json::json!({"required": ["answer"]});
        assert_eq!(
            normalize_structured_output("```json\n{\"answer\":1}\n```", &schema).unwrap(),
            "{\"answer\":1}"
        );
        assert_eq!(
            normalize_structured_output("好的,结果如下:\n{\"answer\":2}\n以上。", &schema).unwrap(),
            "{\"answer\":2}"
        );
        assert!(normalize_structured_output("纯文本回答", &schema).is_err());
        assert!(normalize_structured_output("{\"other\":1}", &schema).is_err());

        // 模型省略固定常量字段时按 schema const 补齐。
        let schema = serde_json::json!({
            "required": ["schemaVersion", "status"],
            "properties": {
                "schemaVersion": {"const": "answer.v1"},
                "status": {"enum": ["answered"]}
            }
        });
        let normalized = normalize_structured_output(r#"{"status":"answered"}"#, &schema).unwrap();
        assert!(normalized.contains("answer.v1"));
    }

    #[test]
    fn sse_parser_separates_reasoning_and_routes_structured_answer_to_internal_channel() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let sink: DeltaSink = Arc::new(move |delta| {
            sink_events.lock().unwrap().push(delta);
        });
        let mut out = ChatOutcome::default();
        let reasoning = r#"data: {"choices":[{"delta":{"reasoning_content":"先确认预算。"}}]}"#;
        assert!(!process_sse_line(reasoning, &mut out, &sink, ContentEmit::None).unwrap());
        let content = r#"data: {"choices":[{"delta":{"content":"{\"answer\":1}"}}]}"#;
        assert!(!process_sse_line(content, &mut out, &sink, ContentEmit::StructuredJson).unwrap());
        assert!(out.content.contains("answer"));
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].channel, StreamChannel::Reasoning);
        assert_eq!(events[0].text, "先确认预算。");
        // 原始 JSON 只进入内部通道,由生成端口转换成 segments[].text。
        assert_eq!(events[1].channel, StreamChannel::StructuredJson);
        assert_eq!(events[1].text, r#"{"answer":1}"#);
        assert!(events
            .iter()
            .all(|event| event.channel != StreamChannel::Answer));
    }

    #[test]
    fn sse_parser_accumulates_tool_call_fragments() {
        let sink: DeltaSink = noop_delta();
        let mut out = ChatOutcome::default();
        let first = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"search_blocks","arguments":"{\"query\":"}}]}}]}"#;
        let second = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"预算\"}"}}]}}]}"#;
        process_sse_line(first, &mut out, &sink, ContentEmit::None).unwrap();
        process_sse_line(second, &mut out, &sink, ContentEmit::None).unwrap();
        out.finalize_tools();
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "search_blocks");
        assert_eq!(out.tool_calls[0].arguments_json, r#"{"query":"预算"}"#);
    }

    #[test]
    fn sse_parser_stops_on_done() {
        let sink: DeltaSink = noop_delta();
        let mut out = ChatOutcome::default();
        assert!(process_sse_line("data: [DONE]", &mut out, &sink, ContentEmit::Answer).unwrap());
        assert!(out.content.is_empty());
    }

    #[test]
    fn retry_policy_only_covers_pre_response_failures() {
        let short = Duration::from_millis(5_000);
        let long = Duration::from_millis(60_000);
        assert!(retryable_before_response(&ProviderError::Unreachable, long));
        // 连接超时(5s connect_timeout)可重试;整体请求超时不重试。
        assert!(retryable_before_response(&ProviderError::Timeout, short));
        assert!(!retryable_before_response(&ProviderError::Timeout, long));
        assert!(retryable_before_response(
            &ProviderError::ServerError(502),
            short
        ));
        assert!(retryable_before_response(
            &ProviderError::ServerError(503),
            short
        ));
        for error in [
            ProviderError::AuthRejected,
            ProviderError::ModelNotFound,
            ProviderError::QuotaExhausted,
            ProviderError::InvalidOutput,
            ProviderError::ResponseTooLarge,
            ProviderError::SecretUnavailable,
            ProviderError::Cancelled,
        ] {
            assert!(!retryable_before_response(&error, short), "{error:?}");
        }
    }

    #[tokio::test]
    async fn chat_request_retries_once_after_5xx_then_succeeds() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            let mut statuses = Vec::new();
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read timeout");
                // 读到请求头结束即可;请求体很小,已随头部一起到达。
                let mut buf = [0u8; 4096];
                let mut seen: Vec<u8> = Vec::new();
                loop {
                    let n = stream.read(&mut buf).expect("read");
                    if n == 0 {
                        break;
                    }
                    seen.extend_from_slice(&buf[..n]);
                    if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                statuses.push(attempt);
                if attempt == 0 {
                    stream
                        .write_all(
                            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write 502");
                } else {
                    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"成\"}}]}\n\n\
                                data: {\"choices\":[{\"delta\":{\"content\":\"功\"}}]}\n\n\
                                data: [DONE]\n\n";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(head.as_bytes()).expect("write head");
                    stream.write_all(body.as_bytes()).expect("write body");
                }
                let _ = stream.flush();
            }
            statuses.len()
        });

        let transport = ProviderTransport {
            http: reqwest::Client::builder().build().expect("client"),
            base_url: format!("http://127.0.0.1:{port}"),
            bearer: None,
            max_response_bytes: 1024 * 1024,
            request_timeout_ms: 30_000,
            embed_dimensions: None,
        };
        let provider = Arc::new(OpenAiCompatibleProvider::new("test", "Test"));
        let request = GenerateRequest {
            model_id: "test-model".into(),
            messages: vec![ProviderMessage::text(
                super::super::provider::ProviderMessageRole::User,
                "hi",
            )],
            json_schema: None,
        };
        let success = provider
            .generate(transport, request, CancellationToken::new(), noop_delta())
            .await
            .expect("retry should succeed");
        assert_eq!(success.content, "成功");
        assert_eq!(
            server.join().expect("server"),
            2,
            "should retry exactly once"
        );
    }
}
