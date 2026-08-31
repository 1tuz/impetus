//! Adapter for legacy OpenAiCompatibleProvider to work with ModelProvider trait.

use crate::{
    CredentialResolver, ModelProvider, OpenAiCompatibleProvider, ProviderError, ProviderHealth,
    ProviderMessage,
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Adapter that wraps OpenAiCompatibleProvider with a CredentialResolver.
#[derive(Clone)]
pub struct OpenAiCompatibleAdapter {
    provider: Arc<OpenAiCompatibleProvider>,
    credential_resolver: Arc<dyn CredentialResolver>,
}

impl std::fmt::Debug for OpenAiCompatibleAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatibleAdapter")
            .field("provider_id", &self.provider.profile().id)
            .field("model", &self.provider.profile().model)
            .finish()
    }
}

impl OpenAiCompatibleAdapter {
    pub fn new(
        provider: Arc<OpenAiCompatibleProvider>,
        credential_resolver: Arc<dyn CredentialResolver>,
    ) -> Self {
        Self {
            provider,
            credential_resolver,
        }
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleAdapter {
    fn provider_id(&self) -> &str {
        &self.provider.profile().id
    }

    fn model_id(&self) -> &str {
        &self.provider.profile().model
    }

    fn health(&self) -> ProviderHealth {
        self.provider.health()
    }

    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        _credential: Option<&str>,
        _runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        on_chunk: Box<dyn FnMut(String) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        let credential = self
            .credential_resolver
            .resolve(self.provider.profile())
            .map_err(|_| ProviderError::MissingCredential)?;

        self.provider
            .stream_messages(messages, credential.as_deref(), cancel, on_chunk)
            .await
    }
}
