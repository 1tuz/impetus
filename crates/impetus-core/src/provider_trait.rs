//! Provider abstraction for model streaming.
//!
//! This trait defines the unified interface for all provider implementations
//! (Mock, OpenAI-compatible, and future providers). The registry owns
//! provider instances and routes requests by provider_id.

use crate::{ProviderError, ProviderHealth, ProviderMessage};
use async_trait::async_trait;
use std::fmt::Debug;
use tokio_util::sync::CancellationToken;

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
    /// The `on_chunk` callback receives each streamed text chunk.
    /// Return `Err` from the callback to stop streaming.
    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        credential: Option<&str>,
        cancel: CancellationToken,
        on_chunk: Box<dyn FnMut(String) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError>;
}
