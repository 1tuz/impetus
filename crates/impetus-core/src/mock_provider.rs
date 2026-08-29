//! Mock provider for testing.
//!
//! Returns pre-configured streaming responses without network calls.

use crate::{ModelProvider, ProviderError, ProviderHealth, ProviderMessage};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockStreamItem {
    Chunk { chunk_id: u32, text: String },
    ToolCall { tool: String, arguments: String },
    Error { message: String },
}

#[derive(Clone, Debug)]
pub struct MockProvider {
    provider_id: String,
    model_id: String,
    items: Vec<MockStreamItem>,
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
        }
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
        _messages: &[ProviderMessage],
        _credential: Option<&str>,
        cancel: CancellationToken,
        mut on_chunk: Box<dyn FnMut(String) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        for item in &self.items {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }

            match item {
                MockStreamItem::Chunk { text, .. } => {
                    on_chunk(text.clone())?;
                }
                MockStreamItem::ToolCall { .. } => {
                    // Tool calls will be handled in future implementation
                    continue;
                }
                MockStreamItem::Error { message } => {
                    return Err(ProviderError::RequestFailed(message.clone()));
                }
            }

            // Simulate streaming delay
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        Ok(())
    }
}
