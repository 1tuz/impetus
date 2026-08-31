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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    pub id: String,
    pub endpoint: String,
    pub model: String,
    pub credential_strategy: CredentialStrategy,
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
                .stream_once(messages, credential, cancel.clone(), &mut on_chunk)
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
        let body = serde_json::json!({
            "model": self.profile.model,
            "stream": true,
            "messages": messages,
        });
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
                CancellationToken::new(),
                |_| Ok(()),
            )
            .await
            .unwrap();
        server.await.unwrap();
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
        };
        assert!(profile.validate().is_err());

        let valid_profile = ProviderProfile {
            endpoint: "https://api.example.com".into(),
            ..profile
        };
        assert!(valid_profile.validate().is_ok());
    }
}
