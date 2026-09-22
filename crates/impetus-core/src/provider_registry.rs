//! Provider registry for model routing.
//!
//! Manages registered providers and routes requests by provider_id.
//! No central concrete enum: providers are registered at runtime.

use crate::{ModelProvider, ProviderError, ProviderHealth};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub use impetus_protocol::{ModelProviderHealthLabel, ModelProviderStatus};

impl From<ProviderHealth> for ModelProviderHealthLabel {
    fn from(health: ProviderHealth) -> Self {
        match health {
            ProviderHealth::Unknown => Self::Unknown,
            ProviderHealth::Healthy => Self::Healthy,
            ProviderHealth::Unavailable {
                last_error_redacted,
            } => Self::Unavailable {
                last_error_redacted,
            },
        }
    }
}

/// Registry of available model providers.
///
/// Providers are registered by ID and retrieved for streaming requests.
/// The registry uses Arc internally, so cloning is cheap.
#[derive(Clone)]
pub struct ProviderRegistry {
    providers: Arc<RwLock<HashMap<String, Arc<dyn ModelProvider>>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a provider. Overwrites any existing provider with the same ID.
    pub fn register(&self, provider: Arc<dyn ModelProvider>) -> Result<(), ProviderError> {
        let provider_id = provider.provider_id().to_string();
        if provider_id.is_empty() {
            return Err(ProviderError::InvalidProfile("provider_id cannot be empty"));
        }

        if let Ok(mut providers) = self.providers.write() {
            providers.insert(provider_id, provider);
            Ok(())
        } else {
            Err(ProviderError::RequestFailed(
                "registry lock unavailable".into(),
            ))
        }
    }

    /// Retrieve a provider by ID.
    pub fn get(&self, provider_id: &str) -> Result<Arc<dyn ModelProvider>, ProviderError> {
        if let Ok(providers) = self.providers.read() {
            providers.get(provider_id).cloned().ok_or_else(|| {
                ProviderError::RequestFailed(format!("provider not found: {}", provider_id))
            })
        } else {
            Err(ProviderError::RequestFailed(
                "registry lock unavailable".into(),
            ))
        }
    }

    /// List all registered provider IDs.
    pub fn list_provider_ids(&self) -> Vec<String> {
        if let Ok(providers) = self.providers.read() {
            providers.keys().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Labels + health for every registered provider (sorted by provider_id).
    pub fn list_status(&self, default_provider_id: &str) -> Vec<ModelProviderStatus> {
        let mut ids = self.list_provider_ids();
        ids.sort();
        ids.into_iter()
            .filter_map(|provider_id| {
                let provider = self.get(&provider_id).ok()?;
                Some(ModelProviderStatus {
                    is_default: provider_id == default_provider_id,
                    provider_id,
                    model_id: provider.model_id().to_string(),
                    health: provider.health().into(),
                })
            })
            .collect()
    }

    /// Check if a provider is registered.
    pub fn contains(&self, provider_id: &str) -> bool {
        if let Ok(providers) = self.providers.read() {
            providers.contains_key(provider_id)
        } else {
            false
        }
    }

    /// Remove a provider from the registry.
    pub fn unregister(&self, provider_id: &str) -> bool {
        if let Ok(mut providers) = self.providers.write() {
            providers.remove(provider_id).is_some()
        } else {
            false
        }
    }

    /// Clear all registered providers.
    pub fn clear(&self) {
        if let Ok(mut providers) = self.providers.write() {
            providers.clear();
        }
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockProvider;

    #[test]
    fn register_and_retrieve() {
        let registry = ProviderRegistry::new();
        let provider = Arc::new(MockProvider::default_mock());

        registry.register(provider.clone()).unwrap();
        assert!(registry.contains("mock"));

        let retrieved = registry.get("mock").unwrap();
        assert_eq!(retrieved.provider_id(), "mock");
        assert_eq!(retrieved.model_id(), "mock-model");
    }

    #[test]
    fn list_providers() {
        let registry = ProviderRegistry::new();
        let mock1 = Arc::new(MockProvider::new("mock1", "model1", []));
        let mock2 = Arc::new(MockProvider::new("mock2", "model2", []));

        registry.register(mock1).unwrap();
        registry.register(mock2).unwrap();

        let ids = registry.list_provider_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"mock1".to_string()));
        assert!(ids.contains(&"mock2".to_string()));

        let status = registry.list_status("mock1");
        assert_eq!(status.len(), 2);
        assert_eq!(status[0].provider_id, "mock1");
        assert!(status[0].is_default);
        assert_eq!(status[0].model_id, "model1");
        assert!(!status[1].is_default);
    }

    #[test]
    fn unregister_provider() {
        let registry = ProviderRegistry::new();
        let provider = Arc::new(MockProvider::default_mock());

        registry.register(provider).unwrap();
        assert!(registry.contains("mock"));

        assert!(registry.unregister("mock"));
        assert!(!registry.contains("mock"));
        assert!(!registry.unregister("mock"));
    }

    #[test]
    fn get_nonexistent() {
        let registry = ProviderRegistry::new();
        let result = registry.get("nonexistent");
        assert!(result.is_err());
    }
}
