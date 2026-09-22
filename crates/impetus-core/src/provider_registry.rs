//! Provider registry for model routing.
//!
//! Manages registered providers and routes requests by provider_id.
//! No central concrete enum: providers are registered at runtime.

use crate::{ModelCatalogEntry, ModelCatalogResult, ModelProvider, ProviderError, ProviderHealth};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub use impetus_protocol::{ModelAvailability, ModelProviderHealthLabel, ModelProviderStatus};

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
    /// Sync path: static profile model only (no remote `/v1/models` probe).
    pub fn list_status(&self, default_provider_id: &str) -> Vec<ModelProviderStatus> {
        let mut ids = self.list_provider_ids();
        ids.sort();
        ids.into_iter()
            .filter_map(|provider_id| {
                let provider = self.get(&provider_id).ok()?;
                let is_default = provider_id == default_provider_id;
                let mut status = ModelProviderStatus::basic(
                    provider_id,
                    provider.model_id().to_string(),
                    provider.health().into(),
                    is_default,
                );
                status.agent_capabilities = provider.agent_capabilities();
                Some(status)
            })
            .collect()
    }

    /// Enrich catalog with OpenAI-compat `/v1/models` (or honest static fallback).
    pub async fn list_status_with_discovery(
        &self,
        default_provider_id: &str,
    ) -> Vec<ModelProviderStatus> {
        let mut ids = self.list_provider_ids();
        ids.sort();
        let mut out = Vec::new();
        for provider_id in ids {
            let Ok(provider) = self.get(&provider_id) else {
                continue;
            };
            let agent_caps = provider.agent_capabilities();
            let catalog = provider.discover_models().await;
            let is_default_provider = provider_id == default_provider_id;
            match catalog {
                ModelCatalogResult::Discovered { models } => {
                    for (idx, entry) in models.into_iter().enumerate() {
                        let mut status = status_from_entry(
                            &provider_id,
                            entry,
                            ModelProviderHealthLabel::Healthy,
                            is_default_provider && idx == 0,
                        );
                        status.availability = ModelAvailability::Available;
                        status.agent_capabilities = agent_caps.clone();
                        status.provider_options =
                            merge_discovery_tag(status.provider_options, "remote_v1_models", None);
                        out.push(status);
                    }
                }
                ModelCatalogResult::StaticFallback {
                    models,
                    reason_redacted,
                } => {
                    for (idx, entry) in models.into_iter().enumerate() {
                        let mut status = status_from_entry(
                            &provider_id,
                            entry,
                            // Honest: do not claim Healthy when discovery failed/absent.
                            ModelProviderHealthLabel::Unknown,
                            is_default_provider && idx == 0,
                        );
                        status.availability = ModelAvailability::Unknown;
                        status.agent_capabilities = agent_caps.clone();
                        status.provider_options = merge_discovery_tag(
                            status.provider_options,
                            "static_fallback",
                            Some(reason_redacted.as_str()),
                        );
                        out.push(status);
                    }
                }
            }
        }
        // Ensure at most one is_default across the whole catalog.
        let mut saw_default = false;
        for status in &mut out {
            if status.is_default {
                if saw_default {
                    status.is_default = false;
                } else {
                    saw_default = true;
                }
            }
        }
        if !saw_default
            && let Some(first) = out
                .iter_mut()
                .find(|s| s.provider_id == default_provider_id)
        {
            first.is_default = true;
        }
        out
    }

    /// Advertise reasoning efforts for `provider_id`/`model_id` via discovery.
    ///
    /// Empty advertised list is honest when the catalog entry exists but has no
    /// efforts. Unknown model id with a non-empty catalog → [`ProviderError::ModelUnavailable`].
    /// Empty catalog → allow id with empty efforts (honest unknown / no metadata).
    pub async fn advertised_reasoning_efforts(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(Vec<String>, Option<String>), ProviderError> {
        let provider = self.get(provider_id)?;
        let catalog = provider.discover_models().await;
        let models = match catalog {
            ModelCatalogResult::Discovered { models }
            | ModelCatalogResult::StaticFallback { models, .. } => models,
        };
        if let Some(entry) = models.iter().find(|m| m.model_id == model_id) {
            return Ok((
                entry.reasoning_efforts.clone(),
                entry.default_reasoning_effort.clone(),
            ));
        }
        if models.is_empty() {
            return Ok((Vec::new(), None));
        }
        Err(ProviderError::ModelUnavailable(format!(
            "model `{model_id}` not in provider `{provider_id}` catalog"
        )))
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

fn status_from_entry(
    provider_id: &str,
    entry: ModelCatalogEntry,
    health: ModelProviderHealthLabel,
    is_default: bool,
) -> ModelProviderStatus {
    let mut status =
        ModelProviderStatus::basic(provider_id, entry.model_id.clone(), health, is_default);
    if let Some(name) = entry.model_display_name {
        status.model_display_name = Some(name);
    }
    status.reasoning_efforts = entry.reasoning_efforts;
    status.default_reasoning_effort = entry.default_reasoning_effort;
    status.capabilities = entry.capabilities;
    status.provider_options = entry.provider_options;
    status
}

fn merge_discovery_tag(
    existing: serde_json::Value,
    discovery: &str,
    reason: Option<&str>,
) -> serde_json::Value {
    let mut map = match existing {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    map.insert(
        "discovery".into(),
        serde_json::Value::String(discovery.into()),
    );
    if let Some(reason) = reason {
        map.insert(
            "reason".into(),
            serde_json::Value::String(reason.to_owned()),
        );
    }
    serde_json::Value::Object(map)
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
        assert!(status[0].reasoning_efforts.is_empty());
        assert!(!status[0].capabilities.reasoning);
    }

    #[tokio::test]
    async fn list_status_with_discovery_static_fallback_is_unknown_not_healthy() {
        let registry = ProviderRegistry::new();
        registry
            .register(Arc::new(MockProvider::default_mock()))
            .unwrap();
        let status = registry.list_status_with_discovery("mock").await;
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].health, ModelProviderHealthLabel::Unknown);
        assert_eq!(status[0].availability, ModelAvailability::Unknown);
        assert_eq!(
            status[0].provider_options["discovery"],
            serde_json::json!("static_fallback")
        );
        assert!(status[0].is_default);
        assert_eq!(status[0].reasoning_efforts, vec!["low", "medium", "high"]);
        assert!(!status[0].capabilities.tools);
        assert!(status[0].capabilities.reasoning);
    }

    #[tokio::test]
    async fn advertised_efforts_come_from_mock_catalog() {
        let registry = ProviderRegistry::new();
        registry
            .register(Arc::new(
                MockProvider::default_mock().with_reasoning_efforts(["low", "medium", "high"]),
            ))
            .unwrap();
        let (efforts, default) = registry
            .advertised_reasoning_efforts("mock", "mock-model")
            .await
            .unwrap();
        assert_eq!(efforts, vec!["low", "medium", "high"]);
        assert_eq!(default.as_deref(), Some("medium"));
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
