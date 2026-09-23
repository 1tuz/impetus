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

/// Per-request stream overrides (session model / reasoning / options). Empty = profile defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamOptions {
    pub model_id: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    /// Non-secret adapter extras merged into the HTTP body (never credentials).
    pub provider_options: serde_json::Value,
}

impl StreamOptions {
    /// Merge first-class + generic options into an OpenAI-compat/native chat body.
    /// Does not overwrite reserved transport keys (`model`, `messages`, `stream`, …).
    /// Secret-shaped keys in `provider_options` are skipped.
    pub fn apply_to_request_body(&self, body: &mut serde_json::Value) {
        if let Some(effort) = self.reasoning_effort.as_deref().filter(|e| !e.is_empty()) {
            body["reasoning_effort"] = serde_json::Value::String(effort.to_string());
        }
        if let Some(tier) = self.service_tier.as_deref().filter(|t| !t.is_empty()) {
            body["service_tier"] = serde_json::Value::String(tier.to_string());
        }
        let Some(obj) = self.provider_options.as_object() else {
            return;
        };
        const RESERVED: &[&str] = &[
            "model",
            "messages",
            "stream",
            "input",
            "tools",
            "tool_choice",
            "system",
            "max_tokens",
        ];
        for (key, value) in obj {
            if RESERVED.contains(&key.as_str()) {
                continue;
            }
            if provider_option_key_is_secretish(key) {
                continue;
            }
            // First-class fields win when already set.
            if (key == "reasoning_effort" || key == "service_tier") && body.get(key).is_some() {
                continue;
            }
            body[key] = value.clone();
        }
    }
}

/// True when a provider-options key looks credential-shaped (never persist/send).
pub fn provider_option_key_is_secretish(key: &str) -> bool {
    const SECRETISH: &[&str] = &[
        "api_key",
        "token",
        "secret",
        "password",
        "authorization",
        "credential",
    ];
    let lower = key.to_ascii_lowercase();
    SECRETISH.iter().any(|s| lower.contains(s))
}

/// Reject non-object / secret-shaped session provider_options (fail-closed).
pub fn validate_session_provider_options(value: &serde_json::Value) -> Result<(), String> {
    match value {
        serde_json::Value::Null => Ok(()),
        serde_json::Value::Object(map) => {
            for key in map.keys() {
                if provider_option_key_is_secretish(key) {
                    return Err(format!(
                        "provider_options key `{key}` rejected: looks like a secret"
                    ));
                }
            }
            Ok(())
        }
        _ => Err("provider_options must be a JSON object or null".into()),
    }
}

/// One model from remote or static catalog discovery (provider is source of truth).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCatalogEntry {
    pub model_id: String,
    pub model_display_name: Option<String>,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
    pub capabilities: ModelCapabilityFlags,
    /// Advertised service tiers (empty = tier not supported / not advertised).
    pub service_tiers: Vec<String>,
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
            service_tiers: Vec::new(),
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
