//! Agent 单步生成端口的生产实现:M08 EgressGateway 唯一出口。
//!
//! 每一步都重新签发许可:载荷 = 本轮完整消息(系统/历史/问题/工具结果)
//! + 附件字节摘要。禁止复用上一部 permit,也不允许适配器自建 HTTP 客户端。

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::model_privacy::models::ModelPurpose;
use crate::model_privacy::permit::EgressPayloadPart;
use crate::model_privacy::service::ModelPrivacyService;
use crate::model_privacy::{noop_delta, EgressRequestSpec};
use crate::retrieval_ai::error as m09err;
use crate::shared::answer_stream::{AnswerStreamFinish, IncrementalAnswerExtractor};
use crate::shared::AppError;

use super::models::{
    AgentGenerationPort, AgentModelCapability, AgentModelRequest, AgentModelResponse,
};

pub struct AgentGenerationAdapter {
    models: Arc<ModelPrivacyService>,
    /// 独立运行时:桥接网关异步发送;Agent 循环本身在阻塞工作线程中执行。
    rt: tokio::runtime::Runtime,
}

impl AgentGenerationAdapter {
    pub fn new(models: Arc<ModelPrivacyService>) -> Self {
        Self {
            models,
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("agent generation bridge runtime"),
        }
    }
}

impl AgentGenerationPort for AgentGenerationAdapter {
    fn capability(&self, document_id: &str) -> Result<AgentModelCapability, AppError> {
        // 质量链路:quality 角色;未配置时回退 generate/fast。
        let Some((provider_id, model_id)) = self.models.reference_quality() else {
            return Ok(AgentModelCapability {
                available: false,
                authorized: false,
                provider_id: None,
                model_id: None,
            });
        };
        let available = self
            .models
            .gateway()
            .provider(&provider_id)
            .is_some_and(|provider| {
                provider
                    .declared_capabilities()
                    .contains(&crate::model_privacy::ProviderCapability::ToolCalling)
            });
        let authorized = self.models.generation_authorized(document_id, &provider_id);
        Ok(AgentModelCapability {
            available,
            authorized,
            provider_id: Some(provider_id),
            model_id: Some(model_id),
        })
    }

    fn generate(
        &self,
        request: &AgentModelRequest,
        cancel: &CancellationToken,
    ) -> Result<AgentModelResponse, AppError> {
        self.generate_inner(request, cancel, noop_delta())
    }

    fn generate_streamed(
        &self,
        request: &AgentModelRequest,
        cancel: &CancellationToken,
        observer: &crate::shared::stream::SharedObserver,
    ) -> Result<AgentModelResponse, AppError> {
        let observer = Arc::clone(observer);
        let extractor = Arc::new(Mutex::new(IncrementalAnswerExtractor::new()));
        let extractor_for_sink = Arc::clone(&extractor);
        let observer_for_sink = Arc::clone(&observer);
        let sink: crate::model_privacy::DeltaSink = Arc::new(
            move |delta: crate::shared::stream::StreamDelta| match delta.channel {
                crate::shared::stream::StreamChannel::Reasoning => {
                    observer_for_sink.on_reasoning(&delta.text)
                }
                crate::shared::stream::StreamChannel::Answer => {
                    observer_for_sink.on_answer(&delta.text)
                }
                crate::shared::stream::StreamChannel::StructuredJson => {
                    let mut guard = extractor_for_sink.lock().unwrap();
                    for text in guard.push(&delta.text) {
                        observer_for_sink.on_answer(&text);
                    }
                }
            },
        );
        let result = self.generate_inner(request, cancel, sink);
        match &result {
            Ok(success) => {
                let guard = extractor.lock().unwrap();
                if guard.finish(&success.content) == AnswerStreamFinish::Reset {
                    observer.on_answer_reset("structured_stream_mismatch");
                }
            }
            Err(_) => {
                if extractor.lock().unwrap().has_emitted() {
                    observer.on_answer_reset("structured_stream_failed");
                }
            }
        }
        result
    }
}

impl AgentGenerationAdapter {
    fn generate_inner(
        &self,
        request: &AgentModelRequest,
        cancel: &CancellationToken,
        sink: crate::model_privacy::DeltaSink,
    ) -> Result<AgentModelResponse, AppError> {
        let (provider_id, model_id) = self
            .models
            .reference_quality()
            .ok_or_else(m09err::model_unavailable)?;
        // 设置变化时不得静默换模型:与启动时选定值不一致即要求重试。
        if request
            .provider_id
            .as_deref()
            .is_some_and(|expected| expected != provider_id)
            || request
                .model_id
                .as_deref()
                .is_some_and(|expected| expected != model_id)
        {
            return Err(
                m09err::model_unavailable().with_detail("reason", "settings_changed_retry".into())
            );
        }
        let spec = EgressRequestSpec {
            scope: crate::model_privacy::gateway::EgressScope::Task,
            task_id: request.task_id.clone(),
            attempt_id: request.attempt_id.clone(),
            document_id: request.document_id.clone(),
            snapshot_id: request.snapshot_id.clone(),
            purpose: ModelPurpose::Generate,
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            parts: payload_parts(request),
        };
        let gateway = self.models.gateway();
        let permit = gateway.issue_permit(&spec)?;
        let permit_id = permit.permit_id.clone();
        let messages = request.messages.clone();
        let tools = request.tools.clone();
        let schema = request.json_schema.clone();
        let spec_for_send = spec.clone();
        let cancel_clone = cancel.clone();
        let gw = Arc::clone(&gateway);
        let success = self
            .rt
            .block_on(async move {
                gw.send_generation_with_tools(
                    &permit_id,
                    &spec_for_send,
                    messages,
                    tools,
                    schema,
                    cancel_clone,
                    sink,
                )
                .await
            })
            .map_err(|err| {
                if err.code == "PRIVACY_EGRESS_DENIED" {
                    // 设置变更/撤权导致在途许可失效:给用户明确“重试”语义。
                    return m09err::model_unavailable()
                        .with_detail("reason", "settings_changed_retry".into());
                }
                m09err::map_egress_error(err)
            })?;
        Ok(AgentModelResponse {
            content: success.content,
            tool_calls: success.tool_calls,
            provider_id,
            model_id: success.model_id,
        })
    }
}

/// 本轮完整消息 → 许可载荷。附件只以 sha256 摘要绑定,字节不进哈希正文。
fn payload_parts(request: &AgentModelRequest) -> Vec<EgressPayloadPart> {
    let mut parts = Vec::new();
    for (index, message) in request.messages.iter().enumerate() {
        // 角色 + 文本 + 工具调用参数/回填 id 都进入哈希绑定(防在途替换)。
        let mut text = format!(
            "[role:{}]
{}",
            message.role.as_str(),
            message.content
        );
        for call in &message.tool_calls {
            text.push_str(&format!(
                "
[tool_call {} {} {}]",
                call.id, call.name, call.arguments_json
            ));
        }
        if let Some(tool_call_id) = &message.tool_call_id {
            text.push_str(&format!(
                "
[tool_call_id {tool_call_id}]"
            ));
        }
        parts.push(EgressPayloadPart {
            source_id: format!("__agent_msg_{index}"),
            text,
            attachment_digest: None,
        });
        for (att_index, attachment) in message.attachments.iter().enumerate() {
            parts.push(EgressPayloadPart {
                source_id: format!("__agent_att_{index}_{att_index}"),
                text: String::new(),
                attachment_digest: Some(crate::shared::textnorm::sha256_hex(&attachment.bytes)),
            });
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_privacy::provider::{ProviderAttachment, ProviderMessage, ToolCall};

    #[test]
    fn payload_parts_bind_tool_arguments_and_attachment_digest() {
        let request = AgentModelRequest {
            task_id: "t".into(),
            attempt_id: "a".into(),
            document_id: "d".into(),
            snapshot_id: "s".into(),
            provider_id: Some("p".into()),
            model_id: Some("m".into()),
            messages: vec![
                ProviderMessage::text(
                    crate::model_privacy::ProviderMessageRole::User,
                    "看图并搜索",
                ),
                ProviderMessage {
                    role: crate::model_privacy::ProviderMessageRole::Assistant,
                    content: String::new(),
                    attachments: Vec::new(),
                    tool_calls: vec![ToolCall {
                        id: "call-1".into(),
                        name: "search_blocks".into(),
                        arguments_json: "{\"query\":\"预算\"}".into(),
                    }],
                    tool_call_id: None,
                },
                ProviderMessage {
                    role: crate::model_privacy::ProviderMessageRole::User,
                    content: "图片".into(),
                    attachments: vec![ProviderAttachment {
                        mime: "image/png".into(),
                        bytes: vec![1, 2, 3],
                        name: None,
                    }],
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
            ],
            tools: Vec::new(),
            json_schema: None,
        };
        let parts = payload_parts(&request);
        assert!(parts[1].text.contains("call-1"));
        assert!(parts[1].text.contains("预算"));
        assert!(parts[3]
            .attachment_digest
            .as_deref()
            .is_some_and(|digest| digest.len() == 64));
    }
}
