//! Provider abstraction for model streaming.
//!
//! This trait defines the unified interface for all provider implementations
//! (Mock, OpenAI-compatible, and future providers). The registry owns
//! provider instances and routes requests by provider_id.

use crate::{AgentRuntime, ProviderError, ProviderHealth, ProviderMessage};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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

    /// Provider-specific reasoning trace (e.g., Claude thinking).
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

    /// Streams messages through the provider.
    ///
    /// The `credential` parameter is resolved transiently by the harness
    /// and never persisted. Implementations must not retain it.
    ///
    /// The `on_event` callback receives each typed stream event.
    /// Return `Err` from the callback to stop streaming.
    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        credential: Option<&str>,
        runtime: Option<Arc<AgentRuntime>>,
        cancel: CancellationToken,
        on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError>;
}
