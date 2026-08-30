use std::{collections::BTreeMap, fmt::Write as _, sync::Arc, time::Instant};

use async_trait::async_trait;
use reqwest::header::CONTENT_TYPE;
use sha2::{Digest, Sha256};

use crate::DurableArtifactStore;

use super::{
    BingHtmlSearchBackend, CitationSource, DuckDuckGoSearchBackend, EgressPolicy, FetchRequest,
    FetchedPage, MAX_FETCH_URL_CHARS, MAX_SEARCH_QUERY_CHARS, SearchBackendPreference,
    SearchRequest, SearchResponse, SecureHttpClient, WebError, WebErrorKind, WebOutcome,
    WebToolObservationDetail, html::extract_body,
};

#[async_trait]
pub trait SearchBackend: Send + Sync {
    fn id(&self) -> &str;
    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, WebError>;
}

pub trait ExternalSearchBackend: SearchBackend {}
impl<T: SearchBackend + ?Sized> ExternalSearchBackend for T {}

#[async_trait]
pub trait WebSearchService: Send + Sync {
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, WebError>;
}

#[async_trait]
pub trait WebFetchService: Send + Sync {
    async fn fetch(&self, request: FetchRequest) -> Result<FetchedPage, WebError>;
}

pub trait WebResearchService: WebSearchService + WebFetchService {}
impl<T> WebResearchService for T where T: WebSearchService + WebFetchService + ?Sized {}

#[derive(Debug, Clone)]
pub struct ArtifactPolicy {
    pub persist_threshold_bytes: usize,
    pub persist_html_always: bool,
    pub persist_binary_always: bool,
}

impl Default for ArtifactPolicy {
    fn default() -> Self {
        Self {
            persist_threshold_bytes: 64 * 1024,
            persist_html_always: true,
            persist_binary_always: true,
        }
    }
}

pub struct WebResearchEngine {
    http: Arc<SecureHttpClient>,
    default_backend: Arc<dyn SearchBackend>,
    builtin_backends: BTreeMap<String, Arc<dyn SearchBackend>>,
    builtin_fallback_order: Vec<String>,
    external_backends: BTreeMap<String, Arc<dyn SearchBackend>>,
    external_fallback_order: Vec<String>,
    artifact_store: Option<Arc<DurableArtifactStore>>,
    artifact_policy: ArtifactPolicy,
}

impl std::fmt::Debug for WebResearchEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebResearchEngine")
            .field("default_backend", &self.default_backend.id())
            .field(
                "builtin_backends",
                &self.builtin_backends.keys().collect::<Vec<_>>(),
            )
            .field("builtin_fallback_order", &self.builtin_fallback_order)
            .field(
                "external_backends",
                &self.external_backends.keys().collect::<Vec<_>>(),
            )
            .field("external_fallback_order", &self.external_fallback_order)
            .field("artifact_store", &self.artifact_store.is_some())
            .field("artifact_policy", &self.artifact_policy)
            .finish()
    }
}

impl WebResearchEngine {
    pub fn production(egress: EgressPolicy) -> Self {
        Self::new(Arc::new(SecureHttpClient::production(egress)))
    }

    pub fn new(http: Arc<SecureHttpClient>) -> Self {
        let default_backend: Arc<dyn SearchBackend> =
            Arc::new(DuckDuckGoSearchBackend::new(http.clone()));
        let bing_backend: Arc<dyn SearchBackend> =
            Arc::new(BingHtmlSearchBackend::new(http.clone()));
        let mut builtin_backends = BTreeMap::new();
        builtin_backends.insert(default_backend.id().to_string(), default_backend.clone());
        builtin_backends.insert(bing_backend.id().to_string(), bing_backend);
        Self {
            http,
            default_backend,
            builtin_backends,
            builtin_fallback_order: vec!["bing_html".into()],
            external_backends: BTreeMap::new(),
            external_fallback_order: Vec::new(),
            artifact_store: None,
            artifact_policy: ArtifactPolicy::default(),
        }
    }

    pub fn with_default_backend(mut self, backend: Arc<dyn SearchBackend>) -> Self {
        self.builtin_backends
            .insert(backend.id().to_string(), backend.clone());
        self.default_backend = backend;
        self
    }

    pub fn with_builtin_fallback(mut self, backend: Arc<dyn SearchBackend>) -> Self {
        let id = backend.id().to_string();
        self.builtin_backends.insert(id.clone(), backend);
        if id != self.default_backend.id() && !self.builtin_fallback_order.contains(&id) {
            self.builtin_fallback_order.push(id);
        }
        self
    }

    pub fn with_external_backend(
        mut self,
        backend: Arc<dyn SearchBackend>,
        use_as_automatic_fallback: bool,
    ) -> Self {
        let id = backend.id().to_string();
        self.external_backends.insert(id.clone(), backend);
        if use_as_automatic_fallback && !self.external_fallback_order.contains(&id) {
            self.external_fallback_order.push(id);
        }
        self
    }

    pub fn with_artifact_store(
        mut self,
        store: Arc<DurableArtifactStore>,
        policy: ArtifactPolicy,
    ) -> Self {
        self.artifact_store = Some(store);
        self.artifact_policy = policy;
        self
    }

    pub fn default_backend_id(&self) -> &str {
        self.default_backend.id()
    }

    pub fn builtin_backend_ids(&self) -> impl Iterator<Item = &str> {
        self.builtin_backends.keys().map(String::as_str)
    }

    pub fn external_backend_ids(&self) -> impl Iterator<Item = &str> {
        self.external_backends.keys().map(String::as_str)
    }

    /// Explicit diagnostics probe. This bypasses fallback short-circuiting and calls every
    /// configured backend once, so doctor can report per-backend reachability. It must never be
    /// used during normal startup because it performs outbound requests.
    pub async fn probe_search_backends(
        &self,
        request: SearchRequest,
    ) -> Vec<(String, Result<SearchResponse, WebError>)> {
        let mut ids = Vec::new();
        ids.push(self.default_backend.id().to_string());
        ids.extend(self.builtin_backends.keys().cloned());
        ids.extend(self.external_backends.keys().cloned());
        let mut unique = Vec::new();
        for id in ids {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }

        let mut results = Vec::with_capacity(unique.len());
        for id in unique {
            if let Some(backend) = self.backend_by_id(&id) {
                results.push((id, backend.search(&request).await));
            }
        }
        results
    }

    pub fn search_observation_detail(response: &SearchResponse) -> WebToolObservationDetail {
        WebToolObservationDetail {
            operation: "web_search".into(),
            outcome: response.outcome,
            backend: Some(response.backend.clone()),
            requested_url: None,
            final_url: None,
            query: Some(response.query.clone()),
            elapsed_ms: response.elapsed_ms,
            citations: response
                .hits
                .iter()
                .map(|hit| CitationSource {
                    citation_id: hit.citation_id.clone(),
                    url: hit.url.clone(),
                    title: Some(hit.title.clone()),
                    artifact: None,
                })
                .collect(),
            error_kind: None,
            error_message: None,
        }
    }

    pub fn fetch_observation_detail(page: &FetchedPage) -> WebToolObservationDetail {
        WebToolObservationDetail {
            operation: "web_fetch".into(),
            outcome: page.outcome,
            backend: None,
            requested_url: Some(page.requested_url.clone()),
            final_url: Some(page.final_url.clone()),
            query: None,
            elapsed_ms: page.elapsed_ms,
            citations: vec![page.citation.clone()],
            error_kind: None,
            error_message: None,
        }
    }

    pub fn error_observation_detail(
        operation: &str,
        error: &WebError,
        query: Option<String>,
        requested_url: Option<String>,
    ) -> WebToolObservationDetail {
        WebToolObservationDetail {
            operation: operation.into(),
            outcome: if error.blocked() {
                WebOutcome::Blocked
            } else {
                WebOutcome::Failed
            },
            backend: None,
            requested_url,
            final_url: error.url.clone(),
            query,
            elapsed_ms: 0,
            citations: Vec::new(),
            error_kind: Some(error.kind),
            error_message: Some(error.message.clone()),
        }
    }

    async fn search_automatic(&self, request: &SearchRequest) -> Result<SearchResponse, WebError> {
        let started = Instant::now();
        let mut attempts = Vec::new();
        let mut last_response = None;

        let mut backend_ids = Vec::with_capacity(
            1 + self.builtin_fallback_order.len() + self.external_fallback_order.len(),
        );
        backend_ids.push(self.default_backend.id().to_string());
        backend_ids.extend(self.builtin_fallback_order.iter().cloned());
        backend_ids.extend(self.external_fallback_order.iter().cloned());
        backend_ids.dedup();

        for backend_id in backend_ids {
            let Some(backend) = self.backend_by_id(&backend_id) else {
                continue;
            };

            match backend.search(request).await {
                Ok(mut response) => {
                    attempts.append(&mut response.attempts);
                    response.attempts = attempts.clone();
                    if response.outcome == WebOutcome::Success && !response.hits.is_empty() {
                        return Ok(response);
                    }
                    last_response = Some(response);
                }
                Err(error) => {
                    attempts.push(super::SearchAttempt {
                        backend: backend.id().to_string(),
                        endpoint: "backend".into(),
                        outcome: if error.blocked() {
                            WebOutcome::Blocked
                        } else {
                            WebOutcome::Failed
                        },
                        detail: Some(error.message.clone()),
                    });
                }
            }
        }

        if let Some(mut response) = last_response {
            response.attempts = attempts;
            return Ok(response);
        }
        if !attempts.is_empty() {
            let outcome = if attempts
                .iter()
                .any(|attempt| attempt.outcome == WebOutcome::Blocked)
            {
                WebOutcome::Blocked
            } else {
                WebOutcome::Failed
            };
            let backend = attempts
                .last()
                .map(|attempt| attempt.backend.clone())
                .unwrap_or_else(|| "web_search".into());
            return Ok(SearchResponse {
                outcome,
                backend,
                query: request.query.clone(),
                hits: Vec::new(),
                attempts,
                elapsed_ms: elapsed_ms(started),
            });
        }
        Err(WebError::new(
            WebErrorKind::BackendUnavailable,
            "no search backend is available",
        ))
    }

    fn backend_by_id(&self, id: &str) -> Option<Arc<dyn SearchBackend>> {
        if id == self.default_backend.id() {
            return Some(self.default_backend.clone());
        }
        self.builtin_backends
            .get(id)
            .or_else(|| self.external_backends.get(id))
            .cloned()
    }

    fn backend_by_preference(
        &self,
        preference: &SearchBackendPreference,
    ) -> Result<Arc<dyn SearchBackend>, WebError> {
        let id = match preference {
            SearchBackendPreference::Automatic => return Ok(self.default_backend.clone()),
            SearchBackendPreference::DuckDuckGo => "duckduckgo",
            SearchBackendPreference::BingHtml => "bing_html",
            SearchBackendPreference::External(id) => {
                return self.external_backends.get(id).cloned().ok_or_else(|| {
                    WebError::new(
                        WebErrorKind::BackendUnavailable,
                        format!("external search backend '{id}' is not registered"),
                    )
                });
            }
        };
        self.builtin_backends.get(id).cloned().ok_or_else(|| {
            WebError::new(
                WebErrorKind::BackendUnavailable,
                format!("built-in search backend '{id}' is not registered"),
            )
        })
    }

    fn persist_body_if_needed(
        &self,
        final_url: &str,
        body: &[u8],
        kind: super::FetchBodyKind,
    ) -> Result<Option<crate::DurableArtifactRef>, WebError> {
        let should_persist = body.len() >= self.artifact_policy.persist_threshold_bytes
            || (kind == super::FetchBodyKind::Html && self.artifact_policy.persist_html_always)
            || (kind == super::FetchBodyKind::Binary && self.artifact_policy.persist_binary_always);
        if !should_persist {
            return Ok(None);
        }
        let Some(store) = &self.artifact_store else {
            return Ok(None);
        };

        store.store(body).map(Some).map_err(|error| {
            WebError::new(
                WebErrorKind::ArtifactStore,
                format!("failed to persist fetched body: {error}"),
            )
            .with_url(final_url)
        })
    }
}

#[async_trait]
impl WebSearchService for WebResearchEngine {
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, WebError> {
        if request.query.trim().is_empty() {
            return Err(WebError::new(
                WebErrorKind::InvalidRequest,
                "search query must not be empty",
            ));
        }
        if request.query.chars().count() > MAX_SEARCH_QUERY_CHARS {
            return Err(WebError::new(
                WebErrorKind::InvalidRequest,
                format!("search query exceeds {MAX_SEARCH_QUERY_CHARS} characters"),
            ));
        }

        match &request.backend {
            SearchBackendPreference::Automatic => self.search_automatic(&request).await,
            preference => {
                self.backend_by_preference(preference)?
                    .search(&request)
                    .await
            }
        }
    }
}

#[async_trait]
impl WebFetchService for WebResearchEngine {
    async fn fetch(&self, request: FetchRequest) -> Result<FetchedPage, WebError> {
        if request.url.trim().is_empty() {
            return Err(WebError::new(
                WebErrorKind::InvalidRequest,
                "fetch URL must not be empty",
            ));
        }
        if request.url.chars().count() > MAX_FETCH_URL_CHARS {
            return Err(WebError::new(
                WebErrorKind::InvalidRequest,
                format!("fetch URL exceeds {MAX_FETCH_URL_CHARS} characters"),
            ));
        }
        if request.max_bytes == 0 || request.max_chars == 0 {
            return Err(WebError::new(
                WebErrorKind::InvalidRequest,
                "fetch max_bytes and max_chars must be greater than zero",
            ));
        }

        let started = Instant::now();
        let max_bytes = request.bounded_max_bytes();
        let max_chars = request.bounded_max_chars();
        let response = self.http.get(&request.url, max_bytes).await?;
        if !response.status.is_success() {
            return Err(WebError::new(
                WebErrorKind::HttpStatus,
                format!("HTTP fetch returned status {}", response.status.as_u16()),
            )
            .with_url(response.final_url));
        }

        let content_type = response
            .headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let extracted = extract_body(
            content_type.as_deref(),
            &response.body,
            &response.final_url,
            max_chars,
            request.include_links,
            request.allow_binary_metadata,
        )?;

        let content_sha256 = sha256_hex(&response.body);
        let artifact =
            self.persist_body_if_needed(&response.final_url, &response.body, extracted.kind)?;
        let citation_id = citation_id("fetch", &response.final_url);

        Ok(FetchedPage {
            outcome: WebOutcome::Success,
            requested_url: response.requested_url,
            final_url: response.final_url.clone(),
            redirect_chain: response.redirect_chain,
            status_code: response.status.as_u16(),
            content_type,
            body_kind: extracted.kind,
            title: extracted.title.clone(),
            text: extracted.text,
            links: extracted.links,
            truncated: extracted.truncated || response.body_truncated,
            citation: CitationSource {
                citation_id,
                url: response.final_url,
                title: extracted.title,
                artifact,
            },
            fetched_unix_ms: now_unix_ms(),
            content_sha256,
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        })
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub(crate) fn citation_id(namespace: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut suffix = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        let _ = write!(&mut suffix, "{byte:02x}");
    }
    format!("web-{suffix}")
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::web_research::{EgressPolicy, SearchAttempt, SearchHit};

    struct FixedBackend {
        id: &'static str,
        outcome: WebOutcome,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SearchBackend for FixedBackend {
        fn id(&self) -> &str {
            self.id
        }

        async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, WebError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let hits = if self.outcome == WebOutcome::Success {
                vec![SearchHit {
                    rank: 1,
                    title: "Example".into(),
                    url: "https://example.com/".into(),
                    snippet: "example".into(),
                    backend: self.id.into(),
                    citation_id: citation_id("search", "https://example.com/"),
                }]
            } else {
                Vec::new()
            };
            Ok(SearchResponse {
                outcome: self.outcome,
                backend: self.id.into(),
                query: request.query.clone(),
                hits,
                attempts: vec![SearchAttempt {
                    backend: self.id.into(),
                    endpoint: self.id.into(),
                    outcome: self.outcome,
                    detail: None,
                }],
                elapsed_ms: 0,
            })
        }
    }

    #[tokio::test]
    async fn automatic_search_falls_back_from_duckduckgo_to_bing_html() {
        let ddg_calls = Arc::new(AtomicUsize::new(0));
        let bing_calls = Arc::new(AtomicUsize::new(0));
        let http = Arc::new(SecureHttpClient::production(EgressPolicy::default()));
        let engine = WebResearchEngine::new(http)
            .with_default_backend(Arc::new(FixedBackend {
                id: "duckduckgo",
                outcome: WebOutcome::Failed,
                calls: ddg_calls.clone(),
            }))
            .with_builtin_fallback(Arc::new(FixedBackend {
                id: "bing_html",
                outcome: WebOutcome::Success,
                calls: bing_calls.clone(),
            }));

        let response = engine.search(SearchRequest::new("rust")).await.unwrap();
        assert_eq!(response.outcome, WebOutcome::Success);
        assert_eq!(response.backend, "bing_html");
        assert_eq!(response.attempts.len(), 2);
        assert_eq!(ddg_calls.load(Ordering::SeqCst), 1);
        assert_eq!(bing_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fetch_persists_bounded_raw_body_when_artifact_policy_requires_it() {
        use std::{net::SocketAddr, sync::Mutex};

        use reqwest::{
            StatusCode,
            header::{CONTENT_TYPE, HeaderMap, HeaderValue},
        };

        use crate::web_research::{DnsResolver, HttpTransport, PreparedGet, RawHttpResponse};

        struct Dns;

        #[async_trait]
        impl DnsResolver for Dns {
            async fn resolve(&self, _host: &str, port: u16) -> Result<Vec<SocketAddr>, WebError> {
                Ok(vec![SocketAddr::new(
                    "93.184.216.34".parse().unwrap(),
                    port,
                )])
            }
        }

        struct Transport(Mutex<Option<RawHttpResponse>>);

        #[async_trait]
        impl HttpTransport for Transport {
            async fn get(&self, _request: PreparedGet) -> Result<RawHttpResponse, WebError> {
                self.0
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| WebError::new(WebErrorKind::Transport, "missing mock response"))
            }
        }

        let body =
            b"<html><head><title>Example</title></head><body><main>Hello web</main></body></html>"
                .to_vec();
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        let transport = Arc::new(Transport(Mutex::new(Some(RawHttpResponse {
            status: StatusCode::OK,
            headers,
            body: body.clone(),
            body_truncated: false,
        }))));
        let http = Arc::new(SecureHttpClient::new(
            transport,
            Arc::new(Dns),
            EgressPolicy::default(),
        ));
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(DurableArtifactStore::open(temp.path()).unwrap());
        let engine = WebResearchEngine::new(http).with_artifact_store(
            store.clone(),
            ArtifactPolicy {
                persist_threshold_bytes: usize::MAX,
                persist_html_always: true,
                persist_binary_always: true,
            },
        );

        let page = engine
            .fetch(FetchRequest::new("https://example.com/page"))
            .await
            .unwrap();
        assert_eq!(page.title.as_deref(), Some("Example"));
        assert_eq!(page.text, "Hello web");
        let artifact = page.citation.artifact.expect("raw body artifact");
        assert_eq!(store.read(&artifact.id).unwrap(), body);
    }
}
