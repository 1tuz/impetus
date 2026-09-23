//! Direct-provider boundary for the v0.2 harness.
//!
//! A profile is explicit and contains no credential bytes.  The caller supplies
//! a resolved credential only at request time; this adapter has no filesystem
//! or process capability and persists nothing.

use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const MAX_SSE_EVENT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum CredentialStrategy {
    /// An explicit local endpoint; no credential is sent.
    None,
    /// Opaque macOS Keychain locator. This is an identifier, never a token.
    KeychainReference { service: String, account: String },
    /// System-browser OAuth flow. URL is shown to user who explicitly opens it.
    /// Callback is handled by local server; token stored in Keychain after exchange.
    SystemBrowserOAuth {
        authorization_url: String,
        token_url: String,
        client_id: String,
        /// Opaque Keychain reference where token will be stored after successful flow.
        keychain_service: String,
        keychain_account: String,
    },
}

/// OpenAI HTTP API surface for native [`crate::OpenAiProvider`].
///
/// Default remains Chat Completions (production daemon path). `Responses` is
/// opt-in via provider profile — never flips the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiHttpApi {
    #[default]
    ChatCompletions,
    Responses,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    pub id: String,
    pub endpoint: String,
    pub model: String,
    pub credential_strategy: CredentialStrategy,
    /// Opt-in OpenAI wire protocol. Default: Chat Completions.
    #[serde(default)]
    pub openai_http_api: OpenAiHttpApi,
}

/// A transient chat message sent to a provider. It is never a durable event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderMessage {
    role: &'static str,
    content: String,
}

impl ProviderMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system",
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user",
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant",
            content: content.into(),
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: "tool",
            content: content.into(),
        }
    }

    pub fn role(&self) -> &str {
        self.role
    }

    pub fn content(&self) -> &str {
        &self.content
    }
}

/// Resolves an explicit profile's credential only when a provider request is
/// about to start. Implementations belong to the harness, never to client IPC
/// or the OpenAI-compatible transport.
pub trait CredentialResolver: Send + Sync {
    fn resolve(&self, profile: &ProviderProfile) -> Result<Option<String>, ProviderError>;
}

/// Resolver for profiles that deliberately do not carry a credential.
pub struct NoCredentialResolver;

impl CredentialResolver for NoCredentialResolver {
    fn resolve(&self, profile: &ProviderProfile) -> Result<Option<String>, ProviderError> {
        match profile.credential_strategy {
            CredentialStrategy::None => Ok(None),
            CredentialStrategy::KeychainReference { .. } => Err(ProviderError::MissingCredential),
            CredentialStrategy::SystemBrowserOAuth { .. } => Err(ProviderError::MissingCredential),
        }
    }
}

impl ProviderProfile {
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.id.trim().is_empty() || self.model.trim().is_empty() {
            return Err(ProviderError::InvalidProfile("id and model are required"));
        }
        let endpoint = reqwest::Url::parse(&self.endpoint)
            .map_err(|_| ProviderError::InvalidProfile("endpoint must be an absolute URL"))?;
        if endpoint.query().is_some() || endpoint.fragment().is_some() {
            return Err(ProviderError::InvalidProfile(
                "endpoint must not contain query or fragment",
            ));
        }
        let local = matches!(endpoint.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
        match (&self.credential_strategy, endpoint.scheme(), local) {
            (CredentialStrategy::None, "http" | "https", true) => Ok(()),
            (CredentialStrategy::KeychainReference { service, account }, "https", _)
                if !service.is_empty() && !account.is_empty() =>
            {
                Ok(())
            }
            (
                CredentialStrategy::SystemBrowserOAuth {
                    authorization_url,
                    token_url,
                    client_id,
                    keychain_service,
                    keychain_account,
                },
                "https",
                _,
            ) if !authorization_url.is_empty()
                && !token_url.is_empty()
                && !client_id.is_empty()
                && !keychain_service.is_empty()
                && !keychain_account.is_empty()
                && reqwest::Url::parse(authorization_url).is_ok()
                && reqwest::Url::parse(token_url).is_ok() =>
            {
                Ok(())
            }
            (CredentialStrategy::None, _, _) => Err(ProviderError::InvalidProfile(
                "no-secret profiles are limited to loopback endpoints",
            )),
            (CredentialStrategy::KeychainReference { .. }, _, _) => {
                Err(ProviderError::InvalidProfile(
                    "credential profiles require HTTPS and non-empty Keychain reference",
                ))
            }
            (CredentialStrategy::SystemBrowserOAuth { .. }, _, _) => {
                Err(ProviderError::InvalidProfile(
                    "OAuth profiles require HTTPS endpoint and valid authorization/token URLs",
                ))
            }
        }
    }

    fn chat_completions_url(&self) -> Result<reqwest::Url, ProviderError> {
        self.validate()?;
        let mut endpoint = reqwest::Url::parse(&self.endpoint)
            .map_err(|_| ProviderError::InvalidProfile("endpoint must be an absolute URL"))?;
        let base = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{base}/v1/chat/completions"));
        Ok(endpoint)
    }

    fn models_url(&self) -> Result<reqwest::Url, ProviderError> {
        self.validate()?;
        let mut endpoint = reqwest::Url::parse(&self.endpoint)
            .map_err(|_| ProviderError::InvalidProfile("endpoint must be an absolute URL"))?;
        let base = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{base}/v1/models"));
        Ok(endpoint)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBudget {
    pub max_attempts: u8,
    pub retry_delay: Duration,
    pub request_timeout: Duration,
}

impl Default for RetryBudget {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            retry_delay: Duration::from_millis(100),
            request_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderHealth {
    Unknown,
    Healthy,
    Unavailable { last_error_redacted: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    #[error("invalid provider profile: {0}")]
    InvalidProfile(&'static str),
    #[error("provider credential is required but unavailable")]
    MissingCredential,
    #[error("provider request cancelled")]
    Cancelled,
    /// Stream ended without a definitive completion — never map to harness Completed.
    #[error("provider interrupted with unknown outcome: {0}")]
    InterruptedUnknown(String),
    #[error("provider request failed: {0}")]
    RequestFailed(String),
    #[error("provider returned malformed stream")]
    MalformedStream,
    #[error("rate limit exceeded: {0}")]
    RateLimited(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("timeout")]
    Timeout,
    #[error("model unavailable: {0}")]
    ModelUnavailable(String),
    #[error("malformed tool call: {0}")]
    MalformedToolCall(String),
}

impl ProviderError {
    /// Classify error as transient (safe to retry) or permanent
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited(_)
                | ProviderError::Network(_)
                | ProviderError::Timeout
                | ProviderError::ModelUnavailable(_)
        )
    }
}

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    client: Client,
    profile: ProviderProfile,
    retry_budget: RetryBudget,
    health: Arc<Mutex<ProviderHealth>>,
}

impl OpenAiCompatibleProvider {
    pub fn new(profile: ProviderProfile, retry_budget: RetryBudget) -> Result<Self, ProviderError> {
        profile.validate()?;
        let client = Client::builder()
            .timeout(retry_budget.request_timeout)
            .build()
            .map_err(|_| ProviderError::RequestFailed("client initialization failed".into()))?;
        Ok(Self {
            client,
            profile,
            retry_budget,
            health: Arc::new(Mutex::new(ProviderHealth::Unknown)),
        })
    }

    pub fn health(&self) -> ProviderHealth {
        self.health
            .lock()
            .map(|value| value.clone())
            .unwrap_or(ProviderHealth::Unavailable {
                last_error_redacted: "provider health state unavailable".into(),
            })
    }

    pub fn profile(&self) -> &ProviderProfile {
        &self.profile
    }

    /// `GET {base}/v1/models` — Keychain credential by reference only; never logged.
    /// Returns catalog entries; metadata only when present in JSON (never invents efforts).
    pub async fn list_remote_models(
        &self,
        credential: Option<&str>,
    ) -> Result<Vec<crate::ModelCatalogEntry>, ProviderError> {
        if matches!(
            self.profile.credential_strategy,
            CredentialStrategy::KeychainReference { .. }
        ) && credential.filter(|value| !value.is_empty()).is_none()
        {
            return Err(ProviderError::MissingCredential);
        }
        let url = self.profile.models_url()?;
        let mut request = self.client.get(url);
        if let Some(credential) = credential {
            request = request.bearer_auth(credential);
        }
        let response = request.send().await.map_err(redact_request_error)?;
        if !response.status().is_success() {
            return Err(ProviderError::RequestFailed(format!(
                "HTTP {}",
                response.status()
            )));
        }
        let body = response.bytes().await.map_err(redact_request_error)?;
        parse_openai_models_list(&body)
    }

    /// Streams a single user message through the OpenAI-compatible endpoint.
    /// `credential` is supplied transiently by the Keychain-owning harness;
    /// it is never retained by this type or included in errors.
    pub async fn stream_user_message<F>(
        &self,
        message: &str,
        credential: Option<&str>,
        cancel: CancellationToken,
        on_chunk: F,
    ) -> Result<(), ProviderError>
    where
        F: FnMut(String) -> Result<(), ProviderError>,
    {
        self.stream_messages(
            &[ProviderMessage::user(message)],
            credential,
            &crate::StreamOptions::default(),
            cancel,
            on_chunk,
        )
        .await
    }

    /// Streams an ordered transient provider message list.
    pub async fn stream_messages<F>(
        &self,
        messages: &[ProviderMessage],
        credential: Option<&str>,
        options: &crate::StreamOptions,
        cancel: CancellationToken,
        mut on_chunk: F,
    ) -> Result<(), ProviderError>
    where
        F: FnMut(String) -> Result<(), ProviderError>,
    {
        let attempts = self.retry_budget.max_attempts.max(1);
        let mut last_error = None;
        for attempt in 1..=attempts {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            match self
                .stream_once(messages, credential, options, cancel.clone(), &mut on_chunk)
                .await
            {
                Ok(()) => {
                    self.set_health(ProviderHealth::Healthy);
                    return Ok(());
                }
                Err(StreamAttemptError::Provider(ProviderError::Cancelled)) => {
                    return Err(ProviderError::Cancelled);
                }
                Err(StreamAttemptError::Provider(error)) => {
                    last_error = Some(error);
                    if attempt < attempts {
                        tokio::select! {
                            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                            _ = tokio::time::sleep(self.retry_budget.retry_delay) => {}
                        }
                    }
                }
                // Retrying a partially emitted SSE stream would duplicate its
                // durable chunks: OpenAI-compatible streams lack a resume id.
                Err(StreamAttemptError::AfterChunk(error)) => {
                    self.set_health(ProviderHealth::Unavailable {
                        last_error_redacted: error.to_string(),
                    });
                    return Err(error);
                }
            }
        }
        let error = last_error.expect("at least one provider attempt");
        self.set_health(ProviderHealth::Unavailable {
            last_error_redacted: error.to_string(),
        });
        Err(error)
    }

    async fn stream_once<F>(
        &self,
        messages: &[ProviderMessage],
        credential: Option<&str>,
        options: &crate::StreamOptions,
        cancel: CancellationToken,
        on_chunk: &mut F,
    ) -> Result<(), StreamAttemptError>
    where
        F: FnMut(String) -> Result<(), ProviderError>,
    {
        if matches!(
            self.profile.credential_strategy,
            CredentialStrategy::KeychainReference { .. }
        ) && credential.filter(|value| !value.is_empty()).is_none()
        {
            return Err(StreamAttemptError::Provider(
                ProviderError::MissingCredential,
            ));
        }
        let model = options
            .model_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(self.profile.model.as_str());
        let mut body = serde_json::json!({
            "model": model,
            "stream": true,
            "messages": messages,
        });
        if let Some(effort) = options
            .reasoning_effort
            .as_deref()
            .filter(|e| !e.is_empty())
        {
            body["reasoning_effort"] = serde_json::Value::String(effort.to_string());
        }
        let mut request = self
            .client
            .post(
                self.profile
                    .chat_completions_url()
                    .map_err(StreamAttemptError::Provider)?,
            )
            .json(&body);
        if let Some(credential) = credential {
            request = request.bearer_auth(credential);
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(StreamAttemptError::Provider(ProviderError::Cancelled)),
            response = request.send() => response
                .map_err(redact_request_error)
                .map_err(StreamAttemptError::Provider)?,
        };
        if !response.status().is_success() {
            return Err(StreamAttemptError::Provider(ProviderError::RequestFailed(
                format!("HTTP {}", response.status()),
            )));
        }
        let mut bytes = response.bytes_stream();
        let mut pending = String::new();
        let mut emitted_chunk = false;
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err(StreamAttemptError::Provider(ProviderError::Cancelled)),
                next = bytes.next() => next,
            };
            let Some(next) = next else { break };
            let next = next.map_err(redact_request_error).map_err(|error| {
                if emitted_chunk {
                    StreamAttemptError::AfterChunk(error)
                } else {
                    StreamAttemptError::Provider(error)
                }
            })?;
            pending.push_str(&String::from_utf8_lossy(&next));
            if pending.len() > MAX_SSE_EVENT_BYTES {
                let error = ProviderError::MalformedStream;
                return Err(if emitted_chunk {
                    StreamAttemptError::AfterChunk(error)
                } else {
                    StreamAttemptError::Provider(error)
                });
            }
            while let Some(boundary) = pending.find("\n\n") {
                let event = pending[..boundary].to_owned();
                pending.drain(..boundary + 2);
                match decode_sse_event(&event).map_err(|error| {
                    if emitted_chunk {
                        StreamAttemptError::AfterChunk(error)
                    } else {
                        StreamAttemptError::Provider(error)
                    }
                })? {
                    SseEvent::Done => return Ok(()),
                    SseEvent::Chunk(chunk) => {
                        on_chunk(chunk).map_err(StreamAttemptError::AfterChunk)?;
                        emitted_chunk = true;
                    }
                    SseEvent::Ignore => {}
                }
            }
        }
        let error = ProviderError::MalformedStream;
        Err(if emitted_chunk {
            StreamAttemptError::AfterChunk(error)
        } else {
            StreamAttemptError::Provider(error)
        })
    }

    fn set_health(&self, value: ProviderHealth) {
        if let Ok(mut health) = self.health.lock() {
            *health = value;
        }
    }
}

fn parse_openai_models_list(body: &[u8]) -> Result<Vec<crate::ModelCatalogEntry>, ProviderError> {
    use impetus_protocol::ModelCapabilityFlags;

    #[derive(Deserialize)]
    struct ModelsResponse {
        data: Vec<serde_json::Value>,
    }

    let parsed: ModelsResponse =
        serde_json::from_slice(body).map_err(|_| ProviderError::MalformedStream)?;

    let mut entries: Vec<crate::ModelCatalogEntry> = Vec::new();
    for raw in parsed.data {
        let Some(id) = raw
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
        else {
            continue;
        };

        let model_display_name = raw
            .get("name")
            .or_else(|| raw.get("display_name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let context_window = raw
            .get("context_length")
            .or_else(|| raw.get("context_window"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let reasoning_efforts =
            string_list_field(&raw, &["reasoning_efforts", "supported_reasoning_efforts"]);
        let default_reasoning_effort = raw
            .get("default_reasoning_effort")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let supported = string_list_field(&raw, &["supported_parameters", "supported_features"]);
        let mut capabilities = ModelCapabilityFlags {
            tools: supported.iter().any(|p| p == "tools" || p == "tool_choice"),
            reasoning: !reasoning_efforts.is_empty()
                || supported.iter().any(|p| {
                    p == "reasoning" || p == "reasoning_effort" || p == "include_reasoning"
                }),
            vision: supported.iter().any(|p| p == "vision" || p == "image")
                || modality_includes_image(&raw),
            context_window,
        };
        // Id-only / no metadata: keep reasoning false even if list empty (already false).
        if reasoning_efforts.is_empty() && !capabilities.reasoning {
            capabilities.reasoning = false;
        }

        let provider_options = extract_non_secret_extras(&raw);

        entries.push(crate::ModelCatalogEntry {
            model_id: id,
            model_display_name,
            reasoning_efforts,
            default_reasoning_effort,
            capabilities,
            provider_options,
        });
    }

    entries.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    entries.dedup_by(|a, b| a.model_id == b.model_id);
    if entries.is_empty() {
        return Err(ProviderError::RequestFailed("empty models list".into()));
    }
    Ok(entries)
}

fn string_list_field(raw: &serde_json::Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        if let Some(arr) = raw.get(*key).and_then(|v| v.as_array()) {
            return arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
        }
    }
    Vec::new()
}

fn modality_includes_image(raw: &serde_json::Value) -> bool {
    if let Some(mods) = raw
        .pointer("/architecture/input_modalities")
        .and_then(|v| v.as_array())
    {
        return mods.iter().any(|v| {
            v.as_str()
                .map(|s| s.eq_ignore_ascii_case("image"))
                .unwrap_or(false)
        });
    }
    if let Some(modality) = raw
        .pointer("/architecture/modality")
        .and_then(|v| v.as_str())
    {
        return modality.to_ascii_lowercase().contains("image");
    }
    false
}

/// Keep CloseRouter/OpenAI-compat extras as JSON; drop credentials-shaped keys.
fn extract_non_secret_extras(raw: &serde_json::Value) -> serde_json::Value {
    const SKIP: &[&str] = &[
        "id",
        "name",
        "display_name",
        "context_length",
        "context_window",
        "reasoning_efforts",
        "supported_reasoning_efforts",
        "default_reasoning_effort",
        "supported_parameters",
        "supported_features",
    ];
    const SECRETISH: &[&str] = &[
        "api_key",
        "token",
        "secret",
        "password",
        "authorization",
        "credential",
    ];
    let Some(obj) = raw.as_object() else {
        return serde_json::Value::Null;
    };
    let mut out = serde_json::Map::new();
    for (key, value) in obj {
        let lower = key.to_ascii_lowercase();
        if SKIP.contains(&key.as_str()) {
            continue;
        }
        if SECRETISH.iter().any(|s| lower.contains(s)) {
            continue;
        }
        out.insert(key.clone(), value.clone());
    }
    if out.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(out)
    }
}

enum StreamAttemptError {
    Provider(ProviderError),
    AfterChunk(ProviderError),
}

enum SseEvent {
    Done,
    Chunk(String),
    Ignore,
}

fn decode_sse_event(event: &str) -> Result<SseEvent, ProviderError> {
    let Some(data) = event.lines().find_map(|line| line.strip_prefix("data: ")) else {
        return Ok(SseEvent::Ignore);
    };
    if data == "[DONE]" {
        return Ok(SseEvent::Done);
    }
    #[derive(Deserialize)]
    struct Response {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        delta: Delta,
    }
    #[derive(Deserialize)]
    struct Delta {
        content: Option<String>,
    }
    let response: Response =
        serde_json::from_str(data).map_err(|_| ProviderError::MalformedStream)?;
    Ok(response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.delta.content)
        .map_or(SseEvent::Ignore, SseEvent::Chunk))
}

fn redact_request_error(error: reqwest::Error) -> ProviderError {
    ProviderError::RequestFailed(format!("{}", error.without_url()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_profile() -> ProviderProfile {
        ProviderProfile {
            id: "local".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            model: "test".into(),
            credential_strategy: CredentialStrategy::None,
            openai_http_api: OpenAiHttpApi::default(),
        }
    }

    #[test]
    fn profile_accepts_only_explicit_safe_endpoint_scopes() {
        assert!(local_profile().validate().is_ok());
        let remote = ProviderProfile {
            endpoint: "https://api.example.test".into(),
            credential_strategy: CredentialStrategy::KeychainReference {
                service: "impetus".into(),
                account: "test".into(),
            },
            ..local_profile()
        };
        assert!(remote.validate().is_ok());
        let unsafe_no_secret = ProviderProfile {
            endpoint: "https://api.example.test".into(),
            ..local_profile()
        };
        assert!(matches!(
            unsafe_no_secret.validate(),
            Err(ProviderError::InvalidProfile(_))
        ));
    }

    #[test]
    fn profile_rejects_raw_credential_and_unknown_configuration_fields() {
        for prohibited_field in ["api_key", "credential_bytes", "oauth_callback"] {
            let profile = format!(
                r#"{{"id":"local","endpoint":"http://127.0.0.1:11434","model":"test","credential_strategy":{{"kind":"none"}},"{prohibited_field}":"opaque-reference-only"}}"#,
            );
            assert!(serde_json::from_str::<ProviderProfile>(&profile).is_err());
        }
        let raw_token_in_strategy = r#"{
            "id":"remote",
            "endpoint":"https://api.example.test",
            "model":"test",
            "credential_strategy":{
                "kind":"keychain_reference",
            "service":"impetus",
                "account":"test",
                "token":"raw-secret"
            }
        }"#;
        assert!(serde_json::from_str::<ProviderProfile>(raw_token_in_strategy).is_err());
    }

    #[test]
    fn sse_decoder_never_returns_wire_data_or_empty_delta() {
        assert!(matches!(
            decode_sse_event("event: ping"),
            Ok(SseEvent::Ignore)
        ));
        assert!(
            matches!(decode_sse_event("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}"), Ok(SseEvent::Chunk(value)) if value == "hello")
        );
        assert!(matches!(
            decode_sse_event("data: [DONE]"),
            Ok(SseEvent::Done)
        ));
    }

    #[tokio::test]
    async fn missing_credential_is_not_retried_or_exposed() {
        let profile = ProviderProfile {
            endpoint: "https://api.example.test".into(),
            credential_strategy: CredentialStrategy::KeychainReference {
                service: "impetus".into(),
                account: "test".into(),
            },
            ..local_profile()
        };
        let provider = OpenAiCompatibleProvider::new(profile, RetryBudget::default()).unwrap();
        let error = provider
            .stream_user_message("hello", None, CancellationToken::new(), |_| Ok(()))
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::MissingCredential);
        assert!(!error.to_string().contains("hello"));
    }

    #[tokio::test]
    async fn cancelled_request_does_not_contact_or_mark_provider_healthy() {
        let provider =
            OpenAiCompatibleProvider::new(local_profile(), RetryBudget::default()).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            provider
                .stream_user_message("hello", None, cancel, |_| Ok(()))
                .await
                .unwrap_err(),
            ProviderError::Cancelled
        );
        assert_eq!(provider.health(), ProviderHealth::Unknown);
    }

    #[tokio::test]
    async fn streams_openai_sse_from_an_explicit_loopback_profile() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .starts_with("POST /v1/chat/completions HTTP/1.1")
            );
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\ndata: [DONE]\n\n",
                )
                .await
                .unwrap();
        });
        let provider = OpenAiCompatibleProvider::new(
            ProviderProfile {
                endpoint: format!("http://{address}"),
                ..local_profile()
            },
            RetryBudget::default(),
        )
        .unwrap();
        let mut chunks = Vec::new();
        provider
            .stream_user_message("test", None, CancellationToken::new(), |chunk| {
                chunks.push(chunk);
                Ok(())
            })
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(chunks.concat(), "hello world");
        assert_eq!(provider.health(), ProviderHealth::Healthy);
    }

    #[tokio::test]
    async fn streams_ordered_context_before_user_message() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..read]).unwrap();
            let body = request.split("\r\n\r\n").nth(1).expect("JSON body");
            let messages = serde_json::from_str::<serde_json::Value>(body).unwrap()["messages"]
                .as_array()
                .unwrap()
                .clone();
            assert_eq!(messages[0]["role"], "system");
            assert_eq!(messages[0]["content"], "workspace rules");
            assert_eq!(messages[1]["role"], "user");
            assert_eq!(messages[1]["content"], "question");
            stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: [DONE]\n\n").await.unwrap();
        });
        let provider = OpenAiCompatibleProvider::new(
            ProviderProfile {
                endpoint: format!("http://{address}"),
                ..local_profile()
            },
            RetryBudget::default(),
        )
        .unwrap();

        provider
            .stream_messages(
                &[
                    ProviderMessage::system("workspace rules"),
                    ProviderMessage::user("question"),
                ],
                None,
                &crate::StreamOptions::default(),
                CancellationToken::new(),
                |_| Ok(()),
            )
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn list_remote_models_discovers_ids_without_secrets() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..read]).unwrap();
            assert!(request.starts_with("GET /v1/models HTTP/1.1"));
            assert!(!request.to_lowercase().contains("authorization"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"data\":[{\"id\":\"gpt-test-a\"},{\"id\":\"gpt-test-b\"}]}",
                )
                .await
                .unwrap();
        });
        let provider = OpenAiCompatibleProvider::new(
            ProviderProfile {
                endpoint: format!("http://{address}"),
                ..local_profile()
            },
            RetryBudget::default(),
        )
        .unwrap();
        let models = provider.list_remote_models(None).await.unwrap();
        server.await.unwrap();
        assert_eq!(
            models
                .iter()
                .map(|m| m.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-test-a", "gpt-test-b"]
        );
        assert!(models[0].reasoning_efforts.is_empty());
        assert!(!models[0].capabilities.reasoning);
    }

    #[tokio::test]
    async fn list_remote_models_absent_stays_unknown_not_healthy() {
        let provider = OpenAiCompatibleProvider::new(
            ProviderProfile {
                endpoint: "http://127.0.0.1:1".into(),
                ..local_profile()
            },
            RetryBudget {
                max_attempts: 1,
                retry_delay: Duration::from_millis(1),
                request_timeout: Duration::from_millis(50),
            },
        )
        .unwrap();
        let err = provider.list_remote_models(None).await.unwrap_err();
        assert!(matches!(err, ProviderError::RequestFailed(_)));
        assert_eq!(provider.health(), ProviderHealth::Unknown);
    }

    #[tokio::test]
    async fn stream_uses_session_model_override_in_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..read]).unwrap();
            let body = request.split("\r\n\r\n").nth(1).expect("JSON body");
            let json: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(json["model"], "session-override-model");
            assert_eq!(json["reasoning_effort"], "high");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: [DONE]\n\n",
                )
                .await
                .unwrap();
        });
        let provider = OpenAiCompatibleProvider::new(
            ProviderProfile {
                endpoint: format!("http://{address}"),
                ..local_profile()
            },
            RetryBudget::default(),
        )
        .unwrap();
        let options = crate::StreamOptions {
            model_id: Some("session-override-model".into()),
            reasoning_effort: Some("high".into()),
        };
        provider
            .stream_messages(
                &[ProviderMessage::user("hi")],
                None,
                &options,
                CancellationToken::new(),
                |_| Ok(()),
            )
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[test]
    fn parse_models_list_filters_empty_ids() {
        let body = br#"{"data":[{"id":"a"},{"id":""},{"id":"b"}]}"#;
        let models = parse_openai_models_list(body).unwrap();
        assert_eq!(
            models
                .iter()
                .map(|m| m.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert!(models[0].reasoning_efforts.is_empty());
        assert!(!models[0].capabilities.reasoning);
    }

    #[test]
    fn parse_models_list_keeps_openrouter_metadata_without_inventing_efforts() {
        let body = br#"{
            "data":[{
                "id":"openai/gpt-4",
                "name":"GPT-4",
                "context_length":8192,
                "owned_by":"openai",
                "supported_parameters":["tools","temperature"],
                "architecture":{"modality":"text->text","input_modalities":["text"]}
            }]
        }"#;
        let models = parse_openai_models_list(body).unwrap();
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(m.model_id, "openai/gpt-4");
        assert_eq!(m.model_display_name.as_deref(), Some("GPT-4"));
        assert_eq!(m.capabilities.context_window, 8192);
        assert!(m.capabilities.tools);
        assert!(!m.capabilities.reasoning);
        assert!(m.reasoning_efforts.is_empty());
        assert_eq!(m.provider_options["owned_by"], "openai");
    }

    #[test]
    fn parse_models_list_uses_advertised_reasoning_efforts_only() {
        let body = br#"{
            "data":[{
                "id":"reasoner",
                "reasoning_efforts":["low","high"],
                "default_reasoning_effort":"low",
                "supported_parameters":["reasoning"]
            }]
        }"#;
        let models = parse_openai_models_list(body).unwrap();
        assert_eq!(models[0].reasoning_efforts, vec!["low", "high"]);
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("low"));
        assert!(models[0].capabilities.reasoning);
    }

    #[test]
    fn oauth_profile_requires_https_endpoint() {
        let profile = ProviderProfile {
            id: "oauth-test".into(),
            model: "gpt-4".into(),
            endpoint: "http://api.example.com".into(),
            credential_strategy: CredentialStrategy::SystemBrowserOAuth {
                authorization_url: "https://auth.example.com/oauth/authorize".into(),
                token_url: "https://auth.example.com/oauth/token".into(),
                client_id: "test-client".into(),
                keychain_service: "impetus".into(),
                keychain_account: "oauth-test".into(),
            },
            openai_http_api: Default::default(),
        };
        assert!(profile.validate().is_err());

        let valid_profile = ProviderProfile {
            endpoint: "https://api.example.com".into(),
            ..profile
        };
        assert!(valid_profile.validate().is_ok());
    }
}
