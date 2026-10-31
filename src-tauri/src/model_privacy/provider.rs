//! ModelProvider 抽象(《M08》§2/§3)。
//!
//! 业务模块(M04/M09)只见到 `EgressGateway` 的许可+发送接口,不接触
//! 提供方 URL、HTTP 客户端或密钥。适配器实现本 trait,收到的
//! `ProviderTransport` 由网关构建:目的地唯一(base_url 来自设置且已校验)、
//! 禁自动重定向、禁代理继承、响应体有上限。密钥由网关在发送前注入,
//! 适配器不持有凭据存储。
//!
//! 流式增量经 `DeltaSink` 回调上抛(仅用于临时显示,不落库);
//! reasoning 与 answer 分通道,原生流式适配器在接收过程中即时上抛。
//! 取消令牌在发送与读取流期间都被监听。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::errors::ProviderError;
use super::models::{
    ModelDescriptor, ModelProviderDescriptor, ProcessingLocation, ProviderCapability, ProviderKind,
};

/// 增量回调:承载 reasoning/answer 片段,永不落库
/// (《M08》§7 流中内容不写数据库)。
pub type DeltaSink = Arc<dyn Fn(crate::shared::stream::StreamDelta) + Send + Sync>;

pub fn noop_delta() -> DeltaSink {
    Arc::new(|_| {})
}

/// 装箱异步结果(trait object 兼容;不引入 async-trait 依赖)。
/// 方法接收者为 `Arc<Self>`、传输句柄按值传入:future 持有自身与目的地,
/// 网关可在并发闸后自由调度。
pub type ProviderFut<T> = Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send>>;

/// 网关发放的传输句柄:唯一允许的目的地与鉴权材料。
/// `bearer` 仅在远程提供方配置了密钥时存在;不进日志。
#[derive(Clone)]
pub struct ProviderTransport {
    pub http: reqwest::Client,
    /// 已校验的请求目的地前缀(来自设置;文档内容永不能提供 URL)。
    pub base_url: String,
    pub bearer: Option<String>,
    pub max_response_bytes: usize,
    pub request_timeout_ms: u64,
    /// 提供方配置的嵌入期望维度(缺省由适配器按返回值一致性校验)。
    pub embed_dimensions: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMessageRole {
    System,
    User,
    Assistant,
    /// 工具结果回填(OpenAI: role="tool" + tool_call_id)。
    Tool,
}

impl ProviderMessageRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// 工具调用(协议层最小投影;参数保持原始 JSON 字符串,适配器不解释)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

/// 模型可调用的工具声明(名称/说明/JSON Schema 参数)。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 随消息发送的图片附件(协议层最小载荷;数据库只存引用,绝不存 base64)。
/// `mime` 必须是图片 MIME;字节由附件服务读取并在读取时校验 sha256。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderAttachment {
    pub mime: String,
    pub bytes: Vec<u8>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderMessage {
    pub role: ProviderMessageRole,
    pub content: String,
    /// 默认空:纯文本请求的协议载荷与旧版完全一致。
    pub attachments: Vec<ProviderAttachment>,
    /// assistant 消息携带的工具调用;其他角色为空。
    pub tool_calls: Vec<ToolCall>,
    /// role=tool 时对应 assistant 的 tool_call.id。
    pub tool_call_id: Option<String>,
}

impl ProviderMessage {
    /// 纯文本消息构造(附件/工具字段为空)。
    pub fn text(role: ProviderMessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// 生成请求:任务/快照绑定信息在许可层,这里只承载协议载荷。
#[derive(Debug, Clone)]
pub struct GenerateRequest {
    pub model_id: String,
    pub messages: Vec<ProviderMessage>,
    /// 结构化输出 JSON Schema(schemars 生成);适配器按协议尽力声明,
    /// Rust 侧最终解析与基础校验不依赖供应商承诺。
    pub json_schema: Option<serde_json::Value>,
}

/// 工具循环的单步请求(与 GenerateRequest 共用许可/取消/流式语义)。
#[derive(Debug, Clone)]
pub struct GenerateWithToolsRequest {
    pub model_id: String,
    pub messages: Vec<ProviderMessage>,
    /// 空 = 最后收敛请求:不提供工具,只要求最终答复。
    pub tools: Vec<ToolSpec>,
    pub json_schema: Option<serde_json::Value>,
}

/// 工具循环的单步响应:content 与 tool_calls 至少一个非空。
#[derive(Debug, Clone, PartialEq)]
pub struct GenerateWithToolsSuccess {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub model_id: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerateSuccess {
    /// 最终文本/JSON 内容(流式片段不在此)。
    pub content: String,
    pub model_id: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct EmbedRequest {
    pub model_id: String,
    pub texts: Vec<String>,
    /// 期望维度;与返回不一致整批失败(不截断/补零,《01》§8.2)。
    pub expected_dimensions: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbedSuccess {
    pub vectors: Vec<Vec<f32>>,
    pub dimensions: i64,
    pub model_id: String,
}

/// 探测结果:可达性与可用模型(不含提示词或密钥)。
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    pub reachable: bool,
    pub models: Vec<ModelDescriptor>,
    /// 协议层声明的结构化输出支持。
    pub supports_structured_output: bool,
}

/// 提供方适配器端口。`probe/generate/embed` 均为装箱 future;
/// 接收 `Arc<Self>` 使 future 自包含,不借用网关栈上的临时值。
pub trait ModelProvider: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> ProviderKind;
    fn location(&self) -> ProcessingLocation;
    fn display_name(&self) -> &str;

    fn probe(self: Arc<Self>, transport: ProviderTransport) -> ProviderFut<ProbeResult>;

    fn generate(
        self: Arc<Self>,
        transport: ProviderTransport,
        request: GenerateRequest,
        cancel: CancellationToken,
        on_delta: DeltaSink,
    ) -> ProviderFut<GenerateSuccess>;

    /// 带工具的单步生成(P2 Agent 只读循环)。每次调用仍由网关单独签发许可;
    /// 适配器只负责协议映射,不得自行创建 HTTP 客户端或绕过许可。
    fn generate_with_tools(
        self: Arc<Self>,
        transport: ProviderTransport,
        request: GenerateWithToolsRequest,
        cancel: CancellationToken,
        on_delta: DeltaSink,
    ) -> ProviderFut<GenerateWithToolsSuccess>;

    fn embed(
        self: Arc<Self>,
        transport: ProviderTransport,
        request: EmbedRequest,
        cancel: CancellationToken,
    ) -> ProviderFut<EmbedSuccess>;

    /// 静态描述(能力表随探测结果由服务层合成)。
    fn base_descriptor(&self) -> ModelProviderDescriptor {
        ModelProviderDescriptor {
            id: self.id().to_string(),
            kind: self.kind(),
            location: self.location(),
            display_name: self.display_name().to_string(),
            capabilities: self.declared_capabilities().to_vec(),
            configured: true,
            reachable: false,
            available_models: Vec::new(),
        }
    }

    fn declared_capabilities(&self) -> &'static [ProviderCapability] {
        &[
            ProviderCapability::Generate,
            ProviderCapability::Embed,
            ProviderCapability::StructuredOutput,
            ProviderCapability::Cancel,
            ProviderCapability::Vision,
            ProviderCapability::ToolCalling,
        ]
    }
}

// —— 假提供方:仅测试(《00》§1 假实现不进生产组装根)——

/// 脚本化假提供方:不依赖网络与真实密钥;记录每次调用与取消行为。
#[cfg(test)]
pub struct FakeProvider {
    id: String,
    kind: ProviderKind,
    location: ProcessingLocation,
    reachable: std::sync::atomic::AtomicBool,
    models: std::sync::Mutex<Vec<ModelDescriptor>>,
    /// 每次生成延迟(ms);期间监听取消令牌(A5)。
    generate_delay_ms: std::sync::atomic::AtomicU64,
    generate_outcome: std::sync::Mutex<GenerateOutcome>,
    tools_outcome: std::sync::Mutex<GenerateWithToolsOutcome>,
    embed_dimensions: std::sync::atomic::AtomicI64,
    pub calls: CallCounters,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg(test)]
pub enum GenerateOutcome {
    Success(String),
    Error(ProviderError),
}

#[derive(Debug, Clone, PartialEq)]
#[cfg(test)]
pub enum GenerateWithToolsOutcome {
    Success {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    Error(ProviderError),
}

#[cfg(test)]
#[derive(Default)]
pub struct CallCounters {
    pub probe: std::sync::atomic::AtomicU64,
    pub generate: std::sync::atomic::AtomicU64,
    pub generate_with_tools: std::sync::atomic::AtomicU64,
    pub embed: std::sync::atomic::AtomicU64,
    pub canceled: std::sync::atomic::AtomicU64,
    /// 最近一次 generate 的 model_id(验证模型切换语义)。
    pub last_model: std::sync::Mutex<Option<String>>,
    /// 最近一次 embed 的 model_id(验证模型切换语义)。
    pub last_embed_model: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
impl FakeProvider {
    pub fn new(id: &str, kind: ProviderKind, location: ProcessingLocation) -> Self {
        Self {
            id: id.to_string(),
            kind,
            location,
            reachable: std::sync::atomic::AtomicBool::new(true),
            models: std::sync::Mutex::new(Vec::new()),
            generate_delay_ms: std::sync::atomic::AtomicU64::new(0),
            generate_outcome: std::sync::Mutex::new(GenerateOutcome::Success("{}".into())),
            tools_outcome: std::sync::Mutex::new(GenerateWithToolsOutcome::Success {
                content: "{}".into(),
                tool_calls: Vec::new(),
            }),
            embed_dimensions: std::sync::atomic::AtomicI64::new(4),
            calls: CallCounters::default(),
        }
    }

    pub fn set_reachable(&self, reachable: bool) {
        self.reachable
            .store(reachable, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn set_models(&self, models: Vec<ModelDescriptor>) {
        *self.models.lock().unwrap() = models;
    }

    pub fn set_generate_delay_ms(&self, ms: u64) {
        self.generate_delay_ms
            .store(ms, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn set_generate_outcome(&self, outcome: GenerateOutcome) {
        *self.generate_outcome.lock().unwrap() = outcome;
    }

    pub fn set_tools_outcome(&self, outcome: GenerateWithToolsOutcome) {
        *self.tools_outcome.lock().unwrap() = outcome;
    }

    pub fn calls_generate_with_tools(&self) -> u64 {
        self.calls
            .generate_with_tools
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn set_embed_dimensions(&self, dims: i64) {
        self.embed_dimensions
            .store(dims, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn calls_generate(&self) -> u64 {
        self.calls
            .generate
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn calls_embed(&self) -> u64 {
        self.calls.embed.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn calls_canceled(&self) -> u64 {
        self.calls
            .canceled
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn last_embed_model(&self) -> Option<String> {
        self.calls.last_embed_model.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl ModelProvider for FakeProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    fn location(&self) -> ProcessingLocation {
        self.location
    }
    fn display_name(&self) -> &str {
        "Fake Provider"
    }

    fn probe(self: Arc<Self>, _transport: ProviderTransport) -> ProviderFut<ProbeResult> {
        Box::pin(async move {
            self.calls
                .probe
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ProbeResult {
                reachable: self.reachable.load(std::sync::atomic::Ordering::SeqCst),
                models: self.models.lock().unwrap().clone(),
                supports_structured_output: true,
            })
        })
    }

    fn generate(
        self: Arc<Self>,
        _transport: ProviderTransport,
        request: GenerateRequest,
        cancel: CancellationToken,
        _on_delta: DeltaSink,
    ) -> ProviderFut<GenerateSuccess> {
        Box::pin(async move {
            self.calls
                .generate
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.calls.last_model.lock().unwrap() = Some(request.model_id.clone());
            let delay = self
                .generate_delay_ms
                .load(std::sync::atomic::Ordering::SeqCst);
            if delay > 0 {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {}
                    _ = cancel.cancelled() => {
                        self.calls.canceled.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        return Err(ProviderError::Cancelled);
                    }
                }
            } else if cancel.is_cancelled() {
                self.calls
                    .canceled
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(ProviderError::Cancelled);
            }
            match self.generate_outcome.lock().unwrap().clone() {
                GenerateOutcome::Success(content) => Ok(GenerateSuccess {
                    content,
                    model_id: request.model_id,
                    prompt_tokens: None,
                    completion_tokens: None,
                }),
                GenerateOutcome::Error(e) => Err(e),
            }
        })
    }

    fn generate_with_tools(
        self: Arc<Self>,
        _transport: ProviderTransport,
        request: GenerateWithToolsRequest,
        cancel: CancellationToken,
        _on_delta: DeltaSink,
    ) -> ProviderFut<GenerateWithToolsSuccess> {
        Box::pin(async move {
            self.calls
                .generate_with_tools
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.calls.last_model.lock().unwrap() = Some(request.model_id.clone());
            let delay = self
                .generate_delay_ms
                .load(std::sync::atomic::Ordering::SeqCst);
            if delay > 0 {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {}
                    _ = cancel.cancelled() => {
                        self.calls.canceled.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        return Err(ProviderError::Cancelled);
                    }
                }
            } else if cancel.is_cancelled() {
                self.calls
                    .canceled
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(ProviderError::Cancelled);
            }
            match self.tools_outcome.lock().unwrap().clone() {
                GenerateWithToolsOutcome::Success {
                    content,
                    tool_calls,
                } => Ok(GenerateWithToolsSuccess {
                    content,
                    tool_calls,
                    model_id: request.model_id,
                    prompt_tokens: None,
                    completion_tokens: None,
                }),
                GenerateWithToolsOutcome::Error(e) => Err(e),
            }
        })
    }

    fn embed(
        self: Arc<Self>,
        _transport: ProviderTransport,
        request: EmbedRequest,
        cancel: CancellationToken,
    ) -> ProviderFut<EmbedSuccess> {
        Box::pin(async move {
            self.calls
                .embed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.calls.last_embed_model.lock().unwrap() = Some(request.model_id.clone());
            if cancel.is_cancelled() {
                self.calls
                    .canceled
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(ProviderError::Cancelled);
            }
            let dims = self
                .embed_dimensions
                .load(std::sync::atomic::Ordering::SeqCst);
            if let Some(expected) = request.expected_dimensions {
                if expected != dims {
                    return Err(ProviderError::InvalidOutput);
                }
            }
            let vectors = request
                .texts
                .iter()
                .map(|t| {
                    let mut v = vec![0f32; dims as usize];
                    if !v.is_empty() {
                        v[0] = (t.len() % 97) as f32;
                    }
                    v
                })
                .collect();
            Ok(EmbedSuccess {
                vectors,
                dimensions: dims,
                model_id: request.model_id,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_provider_cancel_interrupts_delayed_generate() {
        let p = FakeProvider::new(
            "f",
            ProviderKind::OpenaiCompatible,
            ProcessingLocation::Remote,
        );
        p.set_generate_delay_ms(60_000);
        let transport = ProviderTransport {
            http: reqwest::Client::new(),
            base_url: "https://example.com/v1".into(),
            bearer: None,
            max_response_bytes: 1024,
            request_timeout_ms: 1_000,
            embed_dimensions: None,
        };
        let cancel = CancellationToken::new();
        let handle = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                cancel.cancel();
            })
        };
        let started = std::time::Instant::now();
        let out = Arc::new(p)
            .generate(
                transport,
                GenerateRequest {
                    model_id: "m".into(),
                    messages: vec![ProviderMessage::text(ProviderMessageRole::User, "hi")],
                    json_schema: None,
                },
                cancel,
                noop_delta(),
            )
            .await;
        handle.abort();
        assert!(matches!(out, Err(ProviderError::Cancelled)));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "取消必须在 1 秒内中断"
        );
    }
}
