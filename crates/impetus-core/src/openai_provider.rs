//! OpenAI-compatible provider implementation.
//!
//! Streams chat completions through an OpenAI-compatible endpoint with
//! retry logic and health tracking.

use crate::{
    ModelProvider, ProviderError, ProviderHealth, ProviderMessage, ProviderProfile,
    ProviderProtocolAdapter, StreamEvent, ToolCallAssembler,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MAX_SSE_EVENT_BYTES: usize = 64 * 1024;

/// OpenAI Chat Completions `tools` array from [`crate::builtin_tool_schemas`].
pub fn openai_tools_payload() -> serde_json::Value {
    let tools: Vec<serde_json::Value> = crate::builtin_tool_schemas()
        .iter()
        .map(|schema| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": schema.name,
                    "description": schema.description,
                    "parameters": schema.parameters.clone(),
                }
            })
        })
        .collect();
    serde_json::Value::Array(tools)
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
pub struct OpenAiProvider {
    client: Client,
    profile: ProviderProfile,
    retry_budget: RetryBudget,
    health: Arc<Mutex<ProviderHealth>>,
}

impl OpenAiProvider {
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

    fn chat_completions_url(&self) -> Result<reqwest::Url, ProviderError> {
        self.profile.validate()?;
        let mut endpoint = reqwest::Url::parse(&self.profile.endpoint)
            .map_err(|_| ProviderError::InvalidProfile("endpoint must be an absolute URL"))?;
        let base = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{base}/v1/chat/completions"));
        Ok(endpoint)
    }

    fn update_health(&self, health: ProviderHealth) {
        if let Ok(mut current) = self.health.lock() {
            *current = health;
        }
    }

    async fn stream_with_retry(
        &self,
        messages: &[ProviderMessage],
        credential: Option<&str>,
        _runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        let url = self.chat_completions_url()?;
        let mut attempt = 0u8;

        loop {
            attempt += 1;
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }

            let mut request = self.client.post(url.clone()).json(&serde_json::json!({
                "model": self.profile.model,
                "messages": messages,
                "stream": true,
                "tools": openai_tools_payload(),
                "tool_choice": "auto",
            }));

            if let Some(token) = credential {
                request = request.bearer_auth(token);
            }

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
        let mut tool_calls = ToolCallAssembler::new();

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
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data.trim() == "[DONE]" {
                            // Flush tool calls before ending — early return used to
                            // skip emission after the stream terminator.
                            tool_calls.emit_into(&mut on_event)?;
                            return Ok(());
                        }
                        if let Ok(parsed) = serde_json::from_str::<SseData>(data) {
                            if let Some(delta_choice) = parsed.choices.first() {
                                // Emit text delta
                                if let Some(content) = &delta_choice.delta.content {
                                    on_event(StreamEvent::TextDelta {
                                        delta: content.clone(),
                                    })?;
                                }

                                // Accumulate tool calls via shared assembler
                                if let Some(delta_tool_calls) = &delta_choice.delta.tool_calls {
                                    for tc in delta_tool_calls {
                                        tool_calls.apply_openai_delta(
                                            tc.index,
                                            tc.id.as_deref(),
                                            tc.function.as_ref().and_then(|f| f.name.as_deref()),
                                            tc.function
                                                .as_ref()
                                                .and_then(|f| f.arguments.as_deref()),
                                        );
                                    }
                                }

                                // Emit finish reason
                                if let Some(reason) = &delta_choice.finish_reason {
                                    let finish_reason = match reason.as_str() {
                                        "stop" => crate::FinishReason::Stop,
                                        "length" => crate::FinishReason::Length,
                                        "tool_calls" => crate::FinishReason::ToolCalls,
                                        "content_filter" => crate::FinishReason::ContentFilter,
                                        _ => crate::FinishReason::Other,
                                    };
                                    on_event(StreamEvent::Finish {
                                        reason: finish_reason,
                                    })?;
                                }
                            }

                            // Emit usage
                            if let Some(usage) = parsed.usage {
                                on_event(StreamEvent::Usage {
                                    prompt_tokens: usage.prompt_tokens,
                                    completion_tokens: usage.completion_tokens,
                                    measured: true,
                                })?;
                            }
                        }
                    }
                }
            }
        }

        tool_calls.emit_into(&mut on_event)?;
        Ok(())
    }
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
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

impl ProviderProtocolAdapter for OpenAiProvider {
    fn protocol_id(&self) -> &'static str {
        "openai_chat_completions"
    }
}

#[derive(Deserialize)]
struct SseData {
    choices: Vec<SseChoice>,
    usage: Option<SseUsage>,
}

#[derive(Deserialize)]
struct SseChoice {
    delta: SseDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct SseDelta {
    content: Option<String>,
    tool_calls: Option<Vec<SseToolCall>>,
}

#[derive(Deserialize)]
struct SseToolCall {
    index: usize,
    id: Option<String>,
    function: Option<SseFunction>,
}

#[derive(Deserialize)]
struct SseFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct SseUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_sse_data_deserialize_text_delta() {
        let json = r#"{"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let data: SseData = serde_json::from_str(json).unwrap();
        assert_eq!(data.choices.len(), 1);
        assert_eq!(data.choices[0].delta.content, Some("Hello".to_string()));
    }

    #[test]
    fn openai_sse_data_deserialize_tool_call() {
        let json = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"get_weather","arguments":"{\"city\":"}}]},"finish_reason":null}]}"#;
        let data: SseData = serde_json::from_str(json).unwrap();
        let tc = &data.choices[0].delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id, Some("call_abc".to_string()));
        assert_eq!(
            tc.function.as_ref().unwrap().name,
            Some("get_weather".to_string())
        );
    }

    #[test]
    fn openai_sse_data_deserialize_usage() {
        let json = r#"{"choices":[],"usage":{"prompt_tokens":25,"completion_tokens":15}}"#;
        let data: SseData = serde_json::from_str(json).unwrap();
        let usage = data.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 25);
        assert_eq!(usage.completion_tokens, 15);
    }

    #[test]
    fn openai_sse_data_deserialize_finish_reason() {
        let json = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let data: SseData = serde_json::from_str(json).unwrap();
        assert_eq!(data.choices[0].finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn protocol_adapter_id_is_chat_completions() {
        let provider = OpenAiProvider::new(
            crate::ProviderProfile {
                id: "openai".into(),
                model: "gpt-4o".into(),
                endpoint: "http://127.0.0.1:8080".into(),
                credential_strategy: crate::CredentialStrategy::None,
            },
            RetryBudget::default(),
        )
        .unwrap();
        assert_eq!(provider.protocol_id(), "openai_chat_completions");
    }

    #[test]
    fn retry_budget_default() {
        let budget = RetryBudget::default();
        assert_eq!(budget.max_attempts, 2);
        assert_eq!(budget.retry_delay, Duration::from_millis(100));
        assert_eq!(budget.request_timeout, Duration::from_secs(30));
    }

    #[test]
    fn openai_tools_payload_matches_builtin_schemas() {
        let tools = openai_tools_payload();
        let arr = tools.as_array().expect("tools array");
        let schemas = crate::builtin_tool_schemas();
        assert_eq!(arr.len(), schemas.len());
        for (entry, schema) in arr.iter().zip(schemas.iter()) {
            assert_eq!(entry["type"], "function");
            assert_eq!(entry["function"]["name"], schema.name);
            assert_eq!(entry["function"]["description"], schema.description);
            assert_eq!(entry["function"]["parameters"], schema.parameters);
        }
        let names: Vec<&str> = arr
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
    }

    #[test]
    fn chat_completions_request_body_includes_tools() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "tools": openai_tools_payload(),
            "tool_choice": "auto",
        });
        assert_eq!(body["tool_choice"], "auto");
        let tools = body["tools"].as_array().expect("tools");
        assert!(!tools.is_empty());
        assert_eq!(tools[0]["type"], "function");
        assert!(tools[0]["function"]["parameters"].is_object());
    }
}
