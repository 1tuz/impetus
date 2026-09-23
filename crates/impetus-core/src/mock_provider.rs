//! Mock provider for testing.
//!
//! Returns pre-configured streaming responses without network calls.

use crate::{
    FinishReason, ModelCatalogEntry, ModelCatalogResult, ModelProvider, ProviderError,
    ProviderHealth, ProviderMessage, StreamEvent,
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
    /// Reasoning summary (maps to [`StreamEvent::Reasoning`]).
    Reasoning {
        content: String,
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
    last_stream_options: Arc<Mutex<Option<crate::StreamOptions>>>,
    /// Advertised efforts for catalog/discovery tests (empty = reasoning not supported).
    reasoning_efforts: Vec<String>,
    default_reasoning_effort: Option<String>,
    /// Extra catalog model ids sharing the same advertised efforts (tests).
    catalog_model_ids: Vec<String>,
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
            last_stream_options: Arc::new(Mutex::new(None)),
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            catalog_model_ids: Vec::new(),
        }
    }

    /// Advertise free-form reasoning efforts in catalog discovery (tests / fixtures).
    pub fn with_reasoning_efforts(
        mut self,
        efforts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.reasoning_efforts = efforts.into_iter().map(Into::into).collect();
        if self.default_reasoning_effort.is_none()
            && self.reasoning_efforts.iter().any(|e| e == "medium")
        {
            self.default_reasoning_effort = Some("medium".into());
        }
        self
    }

    /// Extra `/v1/models`-style ids in static catalog (share advertised efforts).
    pub fn with_catalog_models(
        mut self,
        models: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.catalog_model_ids = models.into_iter().map(Into::into).collect();
        self
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

    pub fn last_stream_options(&self) -> Option<crate::StreamOptions> {
        self.last_stream_options
            .lock()
            .ok()
            .and_then(|opts| opts.clone())
    }

    pub fn default_mock() -> Self {
        // Honest SoT for daemon/CI mock path: advertise free-form efforts the
        // fixture understands (not a global invented enum for every provider).
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
        .with_reasoning_efforts(["low", "medium", "high"])
    }

    /// Scripted mock for Unix-socket daemon E2E of durable approvals.
    ///
    /// First turn emits `write_file` (NeedsApproval); after ResolveApproval the
    /// second turn finishes with a text chunk. Gated by
    /// `IMPETUS_MOCK_APPROVAL_FIXTURE=1` in [`crate::Harness::new`] — never the
    /// default production mock path.
    pub fn approval_e2e_fixture() -> Self {
        Self::scripted(
            "mock",
            "mock-model",
            [
                vec![MockStreamItem::ToolCall {
                    id: "e2e-write".into(),
                    tool: "write_file".into(),
                    arguments: r#"{"path":"e2e-approval.txt","content":"from-approval"}"#.into(),
                }],
                vec![MockStreamItem::Chunk {
                    chunk_id: 1,
                    text: "done after approval".into(),
                }],
            ],
        )
        .with_reasoning_efforts(["low", "medium", "high"])
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

    async fn discover_models(&self) -> ModelCatalogResult {
        let mut ids = self.catalog_model_ids.clone();
        if ids.is_empty() {
            ids.push(self.model_id.clone());
        } else if !ids.iter().any(|id| id == &self.model_id) {
            ids.insert(0, self.model_id.clone());
        }
        let models = ids
            .into_iter()
            .map(|model_id| {
                let mut entry = ModelCatalogEntry::id_only(model_id);
                entry.reasoning_efforts = self.reasoning_efforts.clone();
                entry.default_reasoning_effort = self.default_reasoning_effort.clone();
                if !entry.reasoning_efforts.is_empty() {
                    entry.capabilities.reasoning = true;
                }
                entry
            })
            .collect();
        ModelCatalogResult::StaticFallback {
            models,
            reason_redacted: "mock provider has no remote catalog".into(),
        }
    }

    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        _credential: Option<&str>,
        _runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        options: crate::StreamOptions,
        mut on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        if let Ok(mut received) = self.received_messages.lock() {
            received.push(messages.to_vec());
        }
        if let Ok(mut opts) = self.last_stream_options.lock() {
            *opts = Some(options);
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
                MockStreamItem::Reasoning { content } => {
                    on_event(StreamEvent::Reasoning {
                        content: content.clone(),
                    })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_e2e_fixture_emits_write_then_chunk() {
        let fixture = MockProvider::approval_e2e_fixture();
        assert_eq!(fixture.provider_id(), "mock");
        assert_eq!(fixture.model_id(), "mock-model");
        // Same-module test: peek scripted turns without consuming the provider.
        let scripts = fixture.scripts.lock().expect("scripts");
        assert_eq!(scripts.len(), 2);
        assert!(matches!(
            &scripts[0][0],
            MockStreamItem::ToolCall { tool, .. } if tool == "write_file"
        ));
        assert!(matches!(
            &scripts[1][0],
            MockStreamItem::Chunk { text, .. } if text == "done after approval"
        ));
    }
}
