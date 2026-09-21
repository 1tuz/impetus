//! Optional API search backends (Tavily/Exa) as replaceable modules (#264).
//!
//! Harness talks through existing [`SearchBackend`]: query → structured hits
//! (title / url / snippet labels). Concrete Tavily/Exa HTTP clients stay
//! **out of core** — no vendor crates as required deps. Credentials are
//! Keychain **labels** only (`service` + `account`); never raw tokens in
//! config, SQLite, logs, or tests.
//!
//! Out of scope: real HTTP to Tavily/Exa, billing, web UI, IPC/TUI intent.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::service::{SearchBackend, citation_id};
use super::{
    SearchAttempt, SearchHit, SearchRequest, SearchResponse, WebError, WebErrorKind, WebOutcome,
};

/// Reason when no optional API search backend is registered.
pub const ABSENT_API_SEARCH_REASON: &str =
    "no optional API search backend registered (Tavily/Exa modules optional)";

/// Reason when a labeled API module has no HTTP client yet (seam only).
pub const API_SEARCH_HTTP_NOT_IMPLEMENTED: &str =
    "optional API search HTTP client not implemented (seam only; no Tavily/Exa crates in core)";

/// Keychain reference labels only — never a raw API token.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApiSearchCredentialRef {
    pub service: String,
    pub account: String,
}

impl ApiSearchCredentialRef {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Result<Self, WebError> {
        let service = service.into();
        let account = account.into();
        if service.trim().is_empty() || account.trim().is_empty() {
            return Err(WebError::new(
                WebErrorKind::Configuration,
                "API search Keychain labels must be non-empty (service + account)",
            ));
        }
        Ok(Self { service, account })
    }
}

/// Which replaceable API search module this slot refers to (identity only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiSearchProviderKind {
    Tavily,
    Exa,
}

impl ApiSearchProviderKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Tavily => "tavily",
            Self::Exa => "exa",
        }
    }
}

/// Tavily/Exa module stub: holds Keychain labels; search fail-closed until a
/// real HTTP adapter is registered later (not a core dependency).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiSearchBackendModule {
    kind: ApiSearchProviderKind,
    credential: ApiSearchCredentialRef,
}

impl ApiSearchBackendModule {
    pub fn tavily(credential: ApiSearchCredentialRef) -> Self {
        Self {
            kind: ApiSearchProviderKind::Tavily,
            credential,
        }
    }

    pub fn exa(credential: ApiSearchCredentialRef) -> Self {
        Self {
            kind: ApiSearchProviderKind::Exa,
            credential,
        }
    }

    pub fn kind(&self) -> ApiSearchProviderKind {
        self.kind
    }

    /// Keychain labels only — callers must not treat these as secrets.
    pub fn credential_ref(&self) -> &ApiSearchCredentialRef {
        &self.credential
    }
}

#[async_trait]
impl SearchBackend for ApiSearchBackendModule {
    fn id(&self) -> &str {
        self.kind.id()
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, WebError> {
        let _ = request;
        // Labels are present; HTTP client is intentionally absent in this seam.
        let _labels = &self.credential;
        Err(WebError::new(
            WebErrorKind::BackendUnavailable,
            API_SEARCH_HTTP_NOT_IMPLEMENTED,
        ))
    }
}

/// Resolve API keys from Keychain labels (never log the secret).
pub trait ApiKeyResolver: Send + Sync {
    fn resolve(&self, service: &str, account: &str) -> Result<Option<String>, WebError>;
}

/// Test double: fixed map of (service, account) → key.
#[derive(Debug, Default, Clone)]
pub struct MapApiKeyResolver {
    keys: HashMap<(String, String), String>,
}

impl MapApiKeyResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_key(
        mut self,
        service: impl Into<String>,
        account: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        self.keys
            .insert((service.into(), account.into()), key.into());
        self
    }
}

impl ApiKeyResolver for MapApiKeyResolver {
    fn resolve(&self, service: &str, account: &str) -> Result<Option<String>, WebError> {
        Ok(self
            .keys
            .get(&(service.to_string(), account.to_string()))
            .cloned())
    }
}

/// Real HTTP Tavily/Exa client behind Keychain labels (#264 / #311).
pub struct HttpApiSearchBackend {
    module: ApiSearchBackendModule,
    resolver: Arc<dyn ApiKeyResolver>,
    /// Override base URL for tests (no network in unit tests when unset + no key).
    base_url_override: Option<String>,
}

impl HttpApiSearchBackend {
    pub fn new(module: ApiSearchBackendModule, resolver: Arc<dyn ApiKeyResolver>) -> Self {
        Self {
            module,
            resolver,
            base_url_override: None,
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url_override = Some(url.into());
        self
    }

    fn resolve_key(&self) -> Result<String, WebError> {
        let cred = self.module.credential_ref();
        match self.resolver.resolve(&cred.service, &cred.account)? {
            Some(key) if !key.trim().is_empty() => Ok(key),
            _ => Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                "API search Keychain label unresolved (fail-closed; no network)",
            )),
        }
    }
}

#[async_trait]
impl SearchBackend for HttpApiSearchBackend {
    fn id(&self) -> &str {
        self.module.kind().id()
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, WebError> {
        let key = self.resolve_key()?;
        let started = Instant::now();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| {
                WebError::new(
                    WebErrorKind::BackendUnavailable,
                    format!("http client: {e}"),
                )
            })?;

        let (url, body) = match self.module.kind() {
            ApiSearchProviderKind::Tavily => {
                let base = self
                    .base_url_override
                    .clone()
                    .unwrap_or_else(|| "https://api.tavily.com/search".into());
                (
                    base,
                    serde_json::json!({
                        "api_key": key,
                        "query": request.query,
                        "max_results": request.normalized_limit(),
                    }),
                )
            }
            ApiSearchProviderKind::Exa => {
                let base = self
                    .base_url_override
                    .clone()
                    .unwrap_or_else(|| "https://api.exa.ai/search".into());
                (
                    base,
                    serde_json::json!({
                        "query": request.query,
                        "numResults": request.normalized_limit(),
                    }),
                )
            }
        };

        let mut req = client.post(&url).json(&body);
        if self.module.kind() == ApiSearchProviderKind::Exa {
            req = req.header("x-api-key", &key);
        }
        // Never log `key`.
        let response = req.send().await.map_err(|e| {
            WebError::new(
                WebErrorKind::BackendUnavailable,
                format!("api search http: {e}"),
            )
        })?;
        if !response.status().is_success() {
            return Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                format!("api search status {}", response.status()),
            ));
        }
        let value: serde_json::Value = response.json().await.map_err(|e| {
            WebError::new(
                WebErrorKind::BackendUnavailable,
                format!("api search json: {e}"),
            )
        })?;
        let hits = parse_api_hits(self.module.kind(), self.id(), &value);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let outcome = if hits.is_empty() {
            WebOutcome::NoResults
        } else {
            WebOutcome::Success
        };
        Ok(SearchResponse {
            outcome,
            backend: self.id().into(),
            query: request.query.clone(),
            hits,
            attempts: vec![SearchAttempt {
                backend: self.id().into(),
                endpoint: url,
                outcome,
                detail: None,
            }],
            elapsed_ms,
        })
    }
}

fn parse_api_hits(
    kind: ApiSearchProviderKind,
    backend_id: &str,
    value: &serde_json::Value,
) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    match kind {
        ApiSearchProviderKind::Tavily => {
            if let Some(results) = value.get("results").and_then(|v| v.as_array()) {
                for (i, item) in results.iter().enumerate() {
                    let title = item
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let url = item
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let snippet = item
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    hits.push(SearchHit {
                        rank: i + 1,
                        title,
                        url: url.clone(),
                        snippet,
                        backend: backend_id.into(),
                        citation_id: citation_id("tavily", &url),
                    });
                }
            }
        }
        ApiSearchProviderKind::Exa => {
            if let Some(results) = value.get("results").and_then(|v| v.as_array()) {
                for (i, item) in results.iter().enumerate() {
                    let title = item
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let url = item
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let snippet = item
                        .get("text")
                        .or_else(|| item.get("snippet"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    hits.push(SearchHit {
                        rank: i + 1,
                        title,
                        url: url.clone(),
                        snippet,
                        backend: backend_id.into(),
                        citation_id: citation_id("exa", &url),
                    });
                }
            }
        }
    }
    hits
}

/// Always-unavailable backend — proves optional path fail-closed without network.
#[derive(Debug, Default, Clone, Copy)]
pub struct AbsentApiSearchBackend;

#[async_trait]
impl SearchBackend for AbsentApiSearchBackend {
    fn id(&self) -> &str {
        "api_search_absent"
    }

    async fn search(&self, _request: &SearchRequest) -> Result<SearchResponse, WebError> {
        Err(WebError::new(
            WebErrorKind::BackendUnavailable,
            ABSENT_API_SEARCH_REASON,
        ))
    }
}

/// Optional slot: `None` → fail-closed absent; `Some` → delegated [`SearchBackend`].
pub struct OptionalApiSearchSlot {
    backend: Option<Arc<dyn SearchBackend>>,
}

impl OptionalApiSearchSlot {
    pub fn absent() -> Self {
        Self { backend: None }
    }

    pub fn with_backend(backend: Arc<dyn SearchBackend>) -> Self {
        Self {
            backend: Some(backend),
        }
    }

    pub fn has_backend(&self) -> bool {
        self.backend.is_some()
    }

    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, WebError> {
        match &self.backend {
            Some(backend) => backend.search(request).await,
            None => Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                ABSENT_API_SEARCH_REASON,
            )),
        }
    }
}

/// In-memory mock for unit tests — no network, no secrets, no vendor crates.
#[derive(Debug, Default, Clone)]
pub struct MockSearchBackend {
    id: String,
    /// query → hits as (title, url, snippet) labels
    hits_by_query: HashMap<String, Vec<(String, String, String)>>,
}

impl MockSearchBackend {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            hits_by_query: HashMap::new(),
        }
    }

    pub fn with_hits(
        mut self,
        query: impl Into<String>,
        hits: impl IntoIterator<Item = (String, String, String)>,
    ) -> Self {
        self.hits_by_query
            .insert(query.into(), hits.into_iter().collect());
        self
    }
}

#[async_trait]
impl SearchBackend for MockSearchBackend {
    fn id(&self) -> &str {
        &self.id
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, WebError> {
        let started = Instant::now();
        let seeded = self
            .hits_by_query
            .get(&request.query)
            .cloned()
            .unwrap_or_default();
        let limit = request.normalized_limit();
        let hits: Vec<SearchHit> = seeded
            .into_iter()
            .take(limit)
            .enumerate()
            .map(|(idx, (title, url, snippet))| SearchHit {
                rank: idx + 1,
                title,
                url: url.clone(),
                snippet,
                backend: self.id.clone(),
                citation_id: citation_id("search", &url),
            })
            .collect();
        let outcome = if hits.is_empty() {
            WebOutcome::NoResults
        } else {
            WebOutcome::Success
        };
        Ok(SearchResponse {
            outcome,
            backend: self.id.clone(),
            query: request.query.clone(),
            hits,
            attempts: vec![SearchAttempt {
                backend: self.id.clone(),
                endpoint: format!("mock://{}", self.id),
                outcome,
                detail: None,
            }],
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_ref_rejects_empty_labels() {
        assert!(ApiSearchCredentialRef::new("", "account").is_err());
        assert!(ApiSearchCredentialRef::new("impetus.search.tavily", "").is_err());
        let ok = ApiSearchCredentialRef::new("impetus.search.tavily", "api-key").expect("labels");
        assert_eq!(ok.service, "impetus.search.tavily");
        assert_eq!(ok.account, "api-key");
    }

    #[test]
    fn modules_expose_kind_ids_and_labels_only() {
        let cred = ApiSearchCredentialRef::new("impetus.search.tavily", "api-key").unwrap();
        let tavily = ApiSearchBackendModule::tavily(cred.clone());
        assert_eq!(tavily.id(), "tavily");
        assert_eq!(tavily.kind(), ApiSearchProviderKind::Tavily);
        assert_eq!(tavily.credential_ref(), &cred);

        let exa_cred = ApiSearchCredentialRef::new("impetus.search.exa", "api-key").unwrap();
        let exa = ApiSearchBackendModule::exa(exa_cred.clone());
        assert_eq!(exa.id(), "exa");
        assert_eq!(exa.kind(), ApiSearchProviderKind::Exa);
        assert_eq!(exa.credential_ref().service, "impetus.search.exa");
    }

    #[tokio::test]
    async fn mock_backend_returns_structured_hits_without_network() {
        let backend = MockSearchBackend::new("mock_api").with_hits(
            "rust async",
            [
                (
                    "Tokio tutorial".into(),
                    "https://docs.rs/tokio".into(),
                    "async runtime for Rust".into(),
                ),
                (
                    "Async book".into(),
                    "https://rust-lang.github.io/async-book/".into(),
                    "async/await guide".into(),
                ),
            ],
        );

        let response = backend
            .search(&SearchRequest::new("rust async"))
            .await
            .expect("mock search");
        assert_eq!(response.outcome, WebOutcome::Success);
        assert_eq!(response.backend, "mock_api");
        assert_eq!(response.hits.len(), 2);
        assert_eq!(response.hits[0].title, "Tokio tutorial");
        assert_eq!(response.hits[0].url, "https://docs.rs/tokio");
        assert_eq!(response.hits[0].snippet, "async runtime for Rust");
        assert_eq!(response.hits[0].rank, 1);
        assert!(!response.hits[0].citation_id.is_empty());
        assert_eq!(response.hits[1].title, "Async book");
    }

    #[tokio::test]
    async fn mock_unknown_query_is_no_results() {
        let backend = MockSearchBackend::new("mock_api");
        let response = backend
            .search(&SearchRequest::new("missing"))
            .await
            .expect("mock");
        assert_eq!(response.outcome, WebOutcome::NoResults);
        assert!(response.hits.is_empty());
    }

    #[tokio::test]
    async fn absent_backend_fail_closed() {
        let backend = AbsentApiSearchBackend;
        let err = backend
            .search(&SearchRequest::new("q"))
            .await
            .expect_err("absent");
        assert_eq!(err.kind, WebErrorKind::BackendUnavailable);
        assert_eq!(err.message, ABSENT_API_SEARCH_REASON);
    }

    #[tokio::test]
    async fn optional_slot_absent_fail_closed() {
        let slot = OptionalApiSearchSlot::absent();
        assert!(!slot.has_backend());
        let err = slot
            .search(&SearchRequest::new("q"))
            .await
            .expect_err("no backend");
        assert_eq!(err.kind, WebErrorKind::BackendUnavailable);
        assert_eq!(err.message, ABSENT_API_SEARCH_REASON);
    }

    #[tokio::test]
    async fn optional_slot_with_mock_delegates() {
        let mock = Arc::new(MockSearchBackend::new("mock_api").with_hits(
            "hello",
            [("Hi".into(), "https://example.com/".into(), "snippet".into())],
        ));
        let slot = OptionalApiSearchSlot::with_backend(mock);
        assert!(slot.has_backend());
        let response = slot
            .search(&SearchRequest::new("hello"))
            .await
            .expect("delegated");
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].title, "Hi");
    }

    #[tokio::test]
    async fn labeled_api_modules_fail_closed_without_http() {
        let tavily = ApiSearchBackendModule::tavily(
            ApiSearchCredentialRef::new("impetus.search.tavily", "api-key").unwrap(),
        );
        let err = tavily
            .search(&SearchRequest::new("q"))
            .await
            .expect_err("no http");
        assert_eq!(err.kind, WebErrorKind::BackendUnavailable);
        assert_eq!(err.message, API_SEARCH_HTTP_NOT_IMPLEMENTED);

        let exa = ApiSearchBackendModule::exa(
            ApiSearchCredentialRef::new("impetus.search.exa", "api-key").unwrap(),
        );
        let err = exa
            .search(&SearchRequest::new("q"))
            .await
            .expect_err("no http");
        assert_eq!(err.message, API_SEARCH_HTTP_NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn http_backend_fail_closed_without_key() {
        let module = ApiSearchBackendModule::tavily(
            ApiSearchCredentialRef::new("impetus.search.tavily", "api-key").unwrap(),
        );
        let backend = HttpApiSearchBackend::new(module, Arc::new(MapApiKeyResolver::new()));
        let err = backend
            .search(&SearchRequest::new("q"))
            .await
            .expect_err("no key");
        assert_eq!(err.kind, WebErrorKind::BackendUnavailable);
        assert!(err.message.contains("unresolved"));
    }

    #[tokio::test]
    async fn engine_registers_mock_as_external_without_vendor_deps() {
        use super::super::{
            EgressPolicy, SearchBackendPreference, SecureHttpClient, WebResearchEngine,
            WebSearchService,
        };

        let http = Arc::new(SecureHttpClient::production(EgressPolicy::default()));
        let mock = Arc::new(MockSearchBackend::new("tavily_mock").with_hits(
            "impetus",
            [(
                "Impetus".into(),
                "https://example.test/impetus".into(),
                "harness".into(),
            )],
        ));
        let engine = WebResearchEngine::new(http).with_external_backend(mock, false);
        assert!(engine.external_backend_ids().any(|id| id == "tavily_mock"));

        let mut request = SearchRequest::new("impetus");
        request.backend = SearchBackendPreference::External("tavily_mock".into());
        let response = engine.search(request).await.expect("external mock");
        assert_eq!(response.backend, "tavily_mock");
        assert_eq!(response.hits[0].title, "Impetus");
    }
}
