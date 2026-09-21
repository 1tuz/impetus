//! Credential-resolving adapter for native [`OpenAiProvider`] (Chat Completions SSE).

use crate::{
    CredentialResolver, ModelProvider, OpenAiProvider, ProviderError, ProviderHealth,
    ProviderMessage, StreamEvent,
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Wraps [`OpenAiProvider`] with Keychain/local credential resolution.
#[derive(Clone)]
pub struct OpenAiNativeAdapter {
    provider: Arc<OpenAiProvider>,
    credential_resolver: Arc<dyn CredentialResolver>,
}

impl std::fmt::Debug for OpenAiNativeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiNativeAdapter")
            .field("provider_id", &self.provider.provider_id())
            .field("model", &self.provider.model_id())
            .finish()
    }
}

impl OpenAiNativeAdapter {
    pub fn new(
        provider: Arc<OpenAiProvider>,
        credential_resolver: Arc<dyn CredentialResolver>,
    ) -> Self {
        Self {
            provider,
            credential_resolver,
        }
    }
}

#[async_trait]
impl ModelProvider for OpenAiNativeAdapter {
    fn provider_id(&self) -> &str {
        self.provider.provider_id()
    }

    fn model_id(&self) -> &str {
        self.provider.model_id()
    }

    fn health(&self) -> ProviderHealth {
        self.provider.health()
    }

    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        _credential: Option<&str>,
        runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        let credential = self
            .credential_resolver
            .resolve(self.provider.profile())
            .map_err(|_| ProviderError::MissingCredential)?;

        self.provider
            .stream_messages(messages, credential.as_deref(), runtime, cancel, on_event)
            .await
    }
}
