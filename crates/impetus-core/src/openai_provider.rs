//! OpenAI-compatible provider implementation.
//!
//! Streams chat completions through an OpenAI-compatible endpoint with
//! retry logic and health tracking.

use crate::{ModelProvider, ProviderError, ProviderHealth, ProviderMessage, ProviderProfile};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MAX_SSE_EVENT_BYTES: usize = 64 * 1024;

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
        on_chunk: Box<dyn FnMut(String) -> Result<(), ProviderError> + Send>,
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
            }));

            if let Some(token) = credential {
                request = request.bearer_auth(token);
            }

            match request.send().await {
                Ok(response) if response.status().is_success() => {
                    self.update_health(ProviderHealth::Healthy);
                    return self.consume_sse_stream(response, cancel, on_chunk).await;
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
        mut on_chunk: Box<dyn FnMut(String) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();

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
                            return Ok(());
                        }
                        if let Ok(parsed) = serde_json::from_str::<SseData>(data)
                            && let Some(delta) = parsed.choices.first()
                            && let Some(content) = &delta.delta.content
                        {
                            on_chunk(content.clone())?;
                        }
                    }
                }
            }
        }

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
        on_chunk: Box<dyn FnMut(String) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        self.stream_with_retry(messages, credential, runtime, cancel, on_chunk)
            .await
    }
}

#[derive(Deserialize)]
struct SseData {
    choices: Vec<SseChoice>,
}

#[derive(Deserialize)]
struct SseChoice {
    delta: SseDelta,
}

#[derive(Deserialize)]
struct SseDelta {
    content: Option<String>,
}
