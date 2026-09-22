//! Provider abstraction for model streaming.
//!
//! This trait defines the unified interface for all provider implementations
//! (Mock, OpenAI-compatible, and future providers). The registry owns
//! provider instances and routes requests by provider_id.

use crate::{AgentRuntime, ProviderError, ProviderHealth, ProviderMessage};
use async_trait::async_trait;
use impetus_protocol::{AgentCapabilitySnapshot, ModelCapabilityFlags};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Per-request stream overrides (session model / reasoning). Empty = profile defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamOptions {
    pub model_id: Option<String>,
    pub reasoning_effort: Option<String>,
}

/// One model from remote or static catalog discovery (provider is source of truth).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCatalogEntry {
    pub model_id: String,
    pub model_display_name: Option<String>,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
    pub capabilities: ModelCapabilityFlags,
    /// Non-secret provider extras (OpenRouter/OpenAI-compat fields as JSON).
    pub provider_options: serde_json::Value,
}

impl ModelCatalogEntry {
    /// Id-only row: empty efforts, all capabilities false/0 (honest when API gave only ids).
    pub fn id_only(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            model_display_name: None,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            capabilities: ModelCapabilityFlags::default(),
            provider_options: serde_json::Value::Null,
        }
    }
}

/// Result of remote model catalog discovery (OpenAI-compat `/v1/models`, etc.).
#[derive(Debug, Clone, PartialEq)]
pub enum ModelCatalogResult {
    /// Remote discovery succeeded with at least one model.
    Discovered { models: Vec<ModelCatalogEntry> },
    /// Discovery unsupported or failed — static profile/catalog only (never fake Healthy).
    StaticFallback {
        models: Vec<ModelCatalogEntry>,
        reason_redacted: String,
    },
}

/// Normalized streaming event from a model provider.
///
/// Providers parse their native protocol (OpenAI, Anthropic, etc.)
/// into these typed events at the boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Incremental text content.
    TextDelta { delta: String },

    /// Structured tool call from the model.
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },

    /// Token usage statistics.
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
        /// True if measured by provider, false if heuristic estimate.
        measured: bool,
    },

    /// Stream completion reason.
    Finish { reason: FinishReason },

    /// Provider-supplied reasoning **summary** only (never hidden CoT).
    Reasoning { content: String },
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Natural completion.
    Stop,
    /// Hit token limit.
    Length,
    /// Model wants to call tools.
    ToolCalls,
    /// Content filter triggered.
    ContentFilter,
    /// Other provider-specific reason.
    Other,
}

/// Unified interface for streaming chat completion providers.
///
/// Implementations must be Send + Sync for concurrent session execution.
#[async_trait]
pub trait ModelProvider: Send + Sync + Debug {
    /// Returns the unique identifier for this provider instance.
    fn provider_id(&self) -> &str;

    /// Returns the model identifier used by this provider.
    fn model_id(&self) -> &str;

    /// Returns the current health status.
    fn health(&self) -> ProviderHealth;

    /// Vendor-neutral agent caps when this provider is an ACP (or similar) backend.
    fn agent_capabilities(&self) -> Option<AgentCapabilitySnapshot> {
        None
    }

    /// Discover models (e.g. OpenAI-compat `GET /v1/models`). Default: static profile model.
    async fn discover_models(&self) -> ModelCatalogResult {
        ModelCatalogResult::StaticFallback {
            models: vec![ModelCatalogEntry::id_only(self.model_id())],
            reason_redacted: "remote model discovery not supported".into(),
        }
    }

    /// Streams messages through the provider.
    ///
    /// The `credential` parameter is resolved transiently by the harness
    /// and never persisted. Implementations must not retain it.
    ///
    /// `options` may override profile model id / reasoning effort for this stream.
    ///
    /// The `on_event` callback receives each typed stream event.
    /// Return `Err` from the callback to stop streaming.
    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        credential: Option<&str>,
        runtime: Option<Arc<AgentRuntime>>,
        cancel: CancellationToken,
        options: StreamOptions,
        on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError>;
}
