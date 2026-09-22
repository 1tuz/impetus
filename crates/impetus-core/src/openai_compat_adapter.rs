//! Adapter for legacy OpenAiCompatibleProvider to work with ModelProvider trait.

use crate::{
    CredentialResolver, ModelCatalogEntry, ModelCatalogResult, ModelProvider,
    OpenAiCompatibleProvider, ProviderError, ProviderHealth, ProviderMessage, StreamEvent,
    StreamOptions,
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

    async fn discover_models(&self) -> ModelCatalogResult {
        let credential = match self.credential_resolver.resolve(self.provider.profile()) {
            Ok(value) => value,
            Err(_) => {
                return ModelCatalogResult::StaticFallback {
                    models: vec![ModelCatalogEntry::id_only(self.model_id())],
                    reason_redacted: "credential unavailable for model discovery".into(),
                };
            }
        };
        match self
            .provider
            .list_remote_models(credential.as_deref())
            .await
        {
            Ok(models) => ModelCatalogResult::Discovered { models },
            Err(error) => ModelCatalogResult::StaticFallback {
                models: vec![ModelCatalogEntry::id_only(self.model_id())],
                reason_redacted: error.to_string(),
            },
        }
    }

    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        _credential: Option<&str>,
        _runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        options: StreamOptions,
        mut on_event: Box<dyn FnMut(StreamEvent) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        let credential = self
            .credential_resolver
            .resolve(self.provider.profile())
            .map_err(|_| ProviderError::MissingCredential)?;

        self.provider
            .stream_messages(
                messages,
                credential.as_deref(),
                &options,
                cancel,
                Box::new(move |text| on_event(StreamEvent::TextDelta { delta: text })),
            )
            .await
    }
}
