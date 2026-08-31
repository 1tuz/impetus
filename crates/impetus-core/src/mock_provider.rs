//! Mock provider for testing.
//!
//! Returns pre-configured streaming responses without network calls.

use crate::{
    FinishReason, ModelProvider, ProviderError, ProviderHealth, ProviderMessage, StreamEvent,
};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockStreamItem {
    Chunk {
        chunk_id: u32,
        text: String,
    },
    ToolCall {
        id: String,
        tool: String,
        arguments: String,
    },
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    Finish {
        reason: FinishReason,
    },
    Error {
        message: String,
    },
    TransientError {
        message: String,
    },
    PermanentError {
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct MockProvider {
    provider_id: String,
    model_id: String,
    items: Vec<MockStreamItem>,
    scripts: Arc<Mutex<VecDeque<Vec<MockStreamItem>>>>,
    received_messages: Arc<Mutex<Vec<Vec<ProviderMessage>>>>,
}

impl MockProvider {
    pub fn new(
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
        items: impl IntoIterator<Item = MockStreamItem>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
            items: items.into_iter().collect(),
            scripts: Arc::new(Mutex::new(VecDeque::new())),
            received_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn scripted(
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
        scripts: impl IntoIterator<Item = Vec<MockStreamItem>>,
    ) -> Self {
        let mut provider = Self::new(provider_id, model_id, []);
        provider.scripts = Arc::new(Mutex::new(scripts.into_iter().collect()));
        provider
    }

    pub fn received_messages(&self) -> Vec<Vec<ProviderMessage>> {
        self.received_messages
            .lock()
            .map(|messages| messages.clone())
            .unwrap_or_default()
    }

    pub fn default_mock() -> Self {
        Self::new(
            "mock",
            "mock-model",
            [
                MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "Mock response: ".into(),
                },
                MockStreamItem::Chunk {
                    chunk_id: 2,
                    text: "streaming works.".into(),
                },
            ],
        )
    }
}

#[async_trait]
impl ModelProvider for MockProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn health(&self) -> ProviderHealth {
        ProviderHealth::Healthy
    }

    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        _credential: Option<&str>,
        _runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        mut on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        if let Ok(mut received) = self.received_messages.lock() {
            received.push(messages.to_vec());
        }
        let items = self
            .scripts
            .lock()
            .ok()
            .and_then(|mut scripts| scripts.pop_front())
            .unwrap_or_else(|| self.items.clone());
        for item in &items {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }

            match item {
                MockStreamItem::Chunk { text, .. } => {
                    on_event(StreamEvent::TextDelta {
                        delta: text.clone(),
                    })?;
                }
                MockStreamItem::ToolCall {
                    id,
                    tool,
                    arguments,
                } => {
                    // Parse arguments as JSON, fail if invalid
                    let args = serde_json::from_str(arguments)
                        .map_err(|_| ProviderError::MalformedStream)?;
                    on_event(StreamEvent::ToolCall {
                        id: id.clone(),
                        name: tool.clone(),
                        arguments: args,
                    })?;
                }
                MockStreamItem::Usage {
                    prompt_tokens,
                    completion_tokens,
                } => {
                    on_event(StreamEvent::Usage {
                        prompt_tokens: *prompt_tokens,
                        completion_tokens: *completion_tokens,
                        measured: true,
                    })?;
                }
                MockStreamItem::Finish { reason } => {
                    on_event(StreamEvent::Finish { reason: *reason })?;
                }
                MockStreamItem::Error { message } => {
                    return Err(ProviderError::RequestFailed(message.clone()));
                }
                MockStreamItem::TransientError { message: _ } => {
                    return Err(ProviderError::Timeout);
                }
                MockStreamItem::PermanentError { message: _ } => {
                    return Err(ProviderError::MissingCredential);
                }
            }

            // Simulate streaming delay
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        Ok(())
    }
}
