//! Anthropic-native provider implementation.
//!
//! Streams messages through the Anthropic Messages API with native
//! tool call parsing and usage tracking.

use crate::{
    FinishReason, ModelProvider, ProviderError, ProviderHealth, ProviderMessage, ProviderProfile,
    StreamEvent,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MAX_SSE_EVENT_BYTES: usize = 64 * 1024;

/// Accumulates streaming tool call input from Anthropic SSE.
#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    input_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBudget {
    pub max_attempts: u8,
    pub retry_delay: Duration,
    pub request_timeout: Duration,
}

impl Default for RetryBudget {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            retry_delay: Duration::from_millis(100),
            request_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AnthropicProvider {
    client: Client,
    profile: ProviderProfile,
    retry_budget: RetryBudget,
    health: Arc<Mutex<ProviderHealth>>,
}

impl AnthropicProvider {
    pub fn new(profile: ProviderProfile, retry_budget: RetryBudget) -> Result<Self, ProviderError> {
        profile.validate()?;
        let client = Client::builder()
            .timeout(retry_budget.request_timeout)
            .build()
            .map_err(|_| ProviderError::RequestFailed("client initialization failed".into()))?;
        Ok(Self {
            client,
            profile,
            retry_budget,
            health: Arc::new(Mutex::new(ProviderHealth::Unknown)),
        })
    }

    pub fn profile(&self) -> &ProviderProfile {
        &self.profile
    }

    fn messages_url(&self) -> Result<reqwest::Url, ProviderError> {
        self.profile.validate()?;
        let mut endpoint = reqwest::Url::parse(&self.profile.endpoint)
            .map_err(|_| ProviderError::InvalidProfile("endpoint must be an absolute URL"))?;
        let base = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{base}/v1/messages"));
        Ok(endpoint)
    }

    fn update_health(&self, health: ProviderHealth) {
        if let Ok(mut current) = self.health.lock() {
            *current = health;
        }
    }

    /// Convert internal messages to Anthropic format.
    /// Anthropic requires separate system prompt and user/assistant turns.
    fn build_request_body(
        &self,
        messages: &[ProviderMessage],
    ) -> Result<serde_json::Value, ProviderError> {
        let mut system_parts: Vec<String> = Vec::new();
        let mut anthropic_messages: Vec<serde_json::Value> = Vec::new();

        for msg in messages {
            match msg.role() {
                "system" => {
                    system_parts.push(msg.content().to_string());
                }
                "user" | "assistant" => {
                    anthropic_messages.push(serde_json::json!({
                        "role": msg.role(),
                        "content": msg.content(),
                    }));
                }
                _ => {
                    // Treat unknown roles as user messages
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": msg.content(),
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "model": self.profile.model,
            "messages": anthropic_messages,
            "stream": true,
            "max_tokens": 8192,
        });

        if !system_parts.is_empty() {
            body["system"] = serde_json::Value::String(system_parts.join("\n\n"));
        }

        Ok(body)
    }

    async fn stream_with_retry(
        &self,
        messages: &[ProviderMessage],
        credential: Option<&str>,
        _runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        let url = self.messages_url()?;
        let body = self.build_request_body(messages)?;
        let mut attempt = 0u8;

        loop {
            attempt += 1;
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }

            let mut request = self.client.post(url.clone()).json(&body);

            if let Some(token) = credential {
                request = request.header("x-api-key", token);
            }
            request = request.header("anthropic-version", "2023-06-01");

            match request.send().await {
                Ok(response) if response.status().is_success() => {
                    self.update_health(ProviderHealth::Healthy);
                    return self.consume_sse_stream(response, cancel, on_event).await;
                }
                Ok(response) => {
                    let status = response.status();
                    let error = format!("HTTP {status}");
                    self.update_health(ProviderHealth::Unavailable {
                        last_error_redacted: error.clone(),
                    });
                    if attempt >= self.retry_budget.max_attempts {
                        return Err(ProviderError::RequestFailed(error));
                    }
                    tokio::time::sleep(self.retry_budget.retry_delay).await;
                }
                Err(_err) => {
                    let error = "network error".to_string();
                    self.update_health(ProviderHealth::Unavailable {
                        last_error_redacted: error.clone(),
                    });
                    if attempt >= self.retry_budget.max_attempts {
                        return Err(ProviderError::RequestFailed(error));
                    }
                    tokio::time::sleep(self.retry_budget.retry_delay).await;
                }
            }
        }
    }

    async fn consume_sse_stream(
        &self,
        response: reqwest::Response,
        cancel: CancellationToken,
        mut on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut tool_call_accumulators: std::collections::HashMap<usize, ToolCallAccumulator> =
            std::collections::HashMap::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;

        while let Some(chunk_result) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }

            let chunk = chunk_result.map_err(|_| ProviderError::MalformedStream)?;
            buffer.extend_from_slice(&chunk);

            if buffer.len() > MAX_SSE_EVENT_BYTES {
                return Err(ProviderError::MalformedStream);
            }

            while let Some(pos) = buffer.windows(2).position(|w| w == b"\n\n") {
                let event = buffer.drain(..pos + 2).collect::<Vec<_>>();
                let text = String::from_utf8_lossy(&event);

                for line in text.lines() {
                    if let Some(data) = line.strip_prefix("data: ")
                        && let Ok(parsed) = serde_json::from_str::<AnthropicSseEvent>(data)
                    {
                        match parsed.event_type.as_str() {
                            "message_start" => {
                                // Extract input tokens from message_start
                                if let Some(message) = parsed.message
                                    && let Some(usage) = message.usage
                                {
                                    input_tokens = usage.input_tokens.unwrap_or(0);
                                }
                            }
                            "content_block_start" => {
                                if let Some(content_block) = parsed.content_block
                                    && content_block.type_field == "tool_use"
                                {
                                    let index = parsed.index.unwrap_or(0);
                                    let acc = tool_call_accumulators.entry(index).or_default();
                                    acc.id = content_block.id.unwrap_or_default();
                                    acc.name = content_block.name.unwrap_or_default();
                                }
                            }
                            "content_block_delta" => {
                                if let Some(delta) = parsed.delta {
                                    let index = parsed.index.unwrap_or(0);
                                    match delta.type_field.as_deref().unwrap_or("") {
                                        "text_delta" => {
                                            if let Some(text) = delta.text {
                                                on_event(StreamEvent::TextDelta { delta: text })?;
                                            }
                                        }
                                        "input_json_delta" => {
                                            if let Some(partial_json) = delta.partial_json
                                                && let Some(acc) =
                                                    tool_call_accumulators.get_mut(&index)
                                            {
                                                acc.input_json.push_str(&partial_json);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            "message_delta" => {
                                // Extract output tokens and stop reason
                                if let Some(usage) = parsed.usage {
                                    output_tokens = usage.output_tokens.unwrap_or(0);
                                }
                                if let Some(delta) = parsed.delta
                                    && let Some(stop_reason) = delta.stop_reason
                                {
                                    let finish_reason = match stop_reason.as_str() {
                                        "end_turn" => FinishReason::Stop,
                                        "max_tokens" => FinishReason::Length,
                                        "tool_use" => FinishReason::ToolCalls,
                                        "stop_sequence" => FinishReason::Stop,
                                        _ => FinishReason::Other,
                                    };
                                    on_event(StreamEvent::Finish {
                                        reason: finish_reason,
                                    })?;
                                }
                            }
                            "message_stop" => {
                                // Emit final usage
                                if input_tokens > 0 || output_tokens > 0 {
                                    on_event(StreamEvent::Usage {
                                        prompt_tokens: input_tokens,
                                        completion_tokens: output_tokens,
                                        measured: true,
                                    })?;
                                }
                                return Ok(());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Emit accumulated tool calls
        for (_index, acc) in tool_call_accumulators {
            if !acc.id.is_empty() && !acc.name.is_empty() {
                let arguments = if acc.input_json.is_empty() {
                    serde_json::Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&acc.input_json).map_err(|_| {
                        ProviderError::MalformedToolCall(format!(
                            "invalid JSON in tool call arguments for {}",
                            acc.name
                        ))
                    })?
                };
                on_event(StreamEvent::ToolCall {
                    id: acc.id,
                    name: acc.name,
                    arguments,
                })?;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn provider_id(&self) -> &str {
        &self.profile.id
    }

    fn model_id(&self) -> &str {
        &self.profile.model
    }

    fn health(&self) -> ProviderHealth {
        self.health
            .lock()
            .map(|h| h.clone())
            .unwrap_or(ProviderHealth::Unavailable {
                last_error_redacted: "health state unavailable".into(),
            })
    }

    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        credential: Option<&str>,
        runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        self.stream_with_retry(messages, credential, runtime, cancel, on_event)
            .await
    }
}

#[derive(Deserialize)]
struct AnthropicSseEvent {
    #[serde(rename = "type")]
    event_type: String,
    message: Option<AnthropicMessage>,
    index: Option<usize>,
    content_block: Option<AnthropicContentBlock>,
    delta: Option<AnthropicDelta>,
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicMessage {
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    type_field: String,
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    type_field: Option<String>,
    text: Option<String>,
    partial_json: Option<String>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_sse_event_deserialize_message_start() {
        let json =
            r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":0}}}"#;
        let event: AnthropicSseEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "message_start");
        assert!(event.message.is_some());
        let msg = event.message.unwrap();
        assert!(msg.usage.is_some());
        assert_eq!(msg.usage.unwrap().input_tokens, Some(25));
    }

    #[test]
    fn anthropic_sse_event_deserialize_content_block_start_tool_use() {
        let json = r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_abc123","name":"get_weather"}}"#;
        let event: AnthropicSseEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "content_block_start");
        assert_eq!(event.index, Some(0));
        let cb = event.content_block.unwrap();
        assert_eq!(cb.type_field, "tool_use");
        assert_eq!(cb.id, Some("toolu_abc123".to_string()));
        assert_eq!(cb.name, Some("get_weather".to_string()));
    }

    #[test]
    fn anthropic_sse_event_deserialize_text_delta() {
        let json = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello world"}}"#;
        let event: AnthropicSseEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "content_block_delta");
        let delta = event.delta.unwrap();
        assert_eq!(delta.type_field.as_deref(), Some("text_delta"));
        assert_eq!(delta.text, Some("Hello world".to_string()));
    }

    #[test]
    fn anthropic_sse_event_deserialize_input_json_delta() {
        let json = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\": \"San"}}"#;
        let event: AnthropicSseEvent = serde_json::from_str(json).unwrap();
        let delta = event.delta.unwrap();
        assert_eq!(delta.type_field.as_deref(), Some("input_json_delta"));
        assert_eq!(delta.partial_json, Some("{\"city\": \"San".to_string()));
    }

    #[test]
    fn anthropic_sse_event_deserialize_message_delta() {
        let json = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#;
        let event: AnthropicSseEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "message_delta");
        let delta = event.delta.unwrap();
        assert_eq!(delta.stop_reason, Some("end_turn".to_string()));
        let usage = event.usage.unwrap();
        assert_eq!(usage.output_tokens, Some(15));
    }

    #[test]
    fn anthropic_sse_event_deserialize_message_stop() {
        let json = r#"{"type":"message_stop"}"#;
        let event: AnthropicSseEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "message_stop");
    }

    #[test]
    fn tool_call_accumulator_default() {
        let acc = ToolCallAccumulator::default();
        assert!(acc.id.is_empty());
        assert!(acc.name.is_empty());
        assert!(acc.input_json.is_empty());
    }

    #[test]
    fn retry_budget_default() {
        let budget = RetryBudget::default();
        assert_eq!(budget.max_attempts, 2);
        assert_eq!(budget.retry_delay, Duration::from_millis(100));
        assert_eq!(budget.request_timeout, Duration::from_secs(30));
    }
}
