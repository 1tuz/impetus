use std::fmt;

use serde::{Deserialize, Serialize};

use crate::DurableArtifactRef;

pub const DEFAULT_SEARCH_RESULTS: usize = 8;
pub const MAX_SEARCH_RESULTS: usize = 20;
pub const MAX_SEARCH_QUERY_CHARS: usize = 4_096;
pub const DEFAULT_FETCH_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_FETCH_BYTES: usize = 5 * 1024 * 1024;
pub const DEFAULT_FETCH_MAX_CHARS: usize = 120_000;
pub const MAX_FETCH_CHARS: usize = 200_000;
pub const MAX_FETCH_URL_CHARS: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SafeSearch {
    Off,
    #[default]
    Moderate,
    Strict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum SearchBackendPreference {
    #[default]
    Automatic,
    DuckDuckGo,
    BingHtml,
    External(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_search_results")]
    pub max_results: usize,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub safe_search: SafeSearch,
    #[serde(default)]
    pub backend: SearchBackendPreference,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            max_results: DEFAULT_SEARCH_RESULTS,
            locale: None,
            safe_search: SafeSearch::Moderate,
            backend: SearchBackendPreference::Automatic,
        }
    }

    pub fn normalized_limit(&self) -> usize {
        self.max_results.clamp(1, MAX_SEARCH_RESULTS)
    }
}

fn default_search_results() -> usize {
    DEFAULT_SEARCH_RESULTS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub rank: usize,
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub backend: String,
    pub citation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebCapability {
    Search,
    Read,
    PrivateRead,
    Download,
    Browser,
    Submit,
    Upload,
}

impl WebCapability {
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Search | Self::Read | Self::PrivateRead)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebOutcome {
    Success,
    NoResults,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchAttempt {
    pub backend: String,
    pub endpoint: String,
    pub outcome: WebOutcome,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub outcome: WebOutcome,
    pub backend: String,
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub attempts: Vec<SearchAttempt>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchRequest {
    pub url: String,
    #[serde(default = "default_fetch_max_bytes")]
    pub max_bytes: usize,
    #[serde(default = "default_fetch_max_chars")]
    pub max_chars: usize,
    #[serde(default = "default_true")]
    pub include_links: bool,
    #[serde(default)]
    pub allow_binary_metadata: bool,
}

impl FetchRequest {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            max_bytes: DEFAULT_FETCH_MAX_BYTES,
            max_chars: DEFAULT_FETCH_MAX_CHARS,
            include_links: true,
            allow_binary_metadata: false,
        }
    }

    pub fn bounded_max_bytes(&self) -> usize {
        self.max_bytes.clamp(1, MAX_FETCH_BYTES)
    }

    pub fn bounded_max_chars(&self) -> usize {
        self.max_chars.clamp(1, MAX_FETCH_CHARS)
    }
}

fn default_fetch_max_bytes() -> usize {
    DEFAULT_FETCH_MAX_BYTES
}

fn default_fetch_max_chars() -> usize {
    DEFAULT_FETCH_MAX_CHARS
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchBodyKind {
    Html,
    Text,
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchLink {
    pub text: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationSource {
    pub citation_id: String,
    pub url: String,
    pub title: Option<String>,
    pub artifact: Option<DurableArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchedPage {
    pub outcome: WebOutcome,
    pub requested_url: String,
    pub final_url: String,
    pub redirect_chain: Vec<String>,
    pub status_code: u16,
    pub content_type: Option<String>,
    pub body_kind: FetchBodyKind,
    pub title: Option<String>,
    pub text: String,
    pub links: Vec<FetchLink>,
    pub truncated: bool,
    pub citation: CitationSource,
    pub fetched_unix_ms: u64,
    pub content_sha256: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum WebObservation {
    Search(SearchResponse),
    Fetch(FetchedPage),
    Detail(WebToolObservationDetail),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebToolObservationDetail {
    pub operation: String,
    pub outcome: WebOutcome,
    pub backend: Option<String>,
    pub requested_url: Option<String>,
    pub final_url: Option<String>,
    pub query: Option<String>,
    pub elapsed_ms: u64,
    pub citations: Vec<CitationSource>,
    pub error_kind: Option<WebErrorKind>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebErrorKind {
    InvalidRequest,
    InvalidUrl,
    UnsupportedScheme,
    CredentialsInUrl,
    PortBlocked,
    HostBlocked,
    DnsResolutionFailed,
    AddressBlocked,
    RedirectLimit,
    RedirectMissingLocation,
    Transport,
    HttpStatus,
    BodyTooLarge,
    UnsupportedContentType,
    BackendUnavailable,
    ArtifactStore,
    Configuration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebError {
    pub kind: WebErrorKind,
    pub message: String,
    pub url: Option<String>,
}

impl WebError {
    pub fn new(kind: WebErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            url: None,
        }
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn blocked(&self) -> bool {
        matches!(
            self.kind,
            WebErrorKind::UnsupportedScheme
                | WebErrorKind::CredentialsInUrl
                | WebErrorKind::PortBlocked
                | WebErrorKind::HostBlocked
                | WebErrorKind::AddressBlocked
        )
    }
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(url) = &self.url {
            write!(f, "{} ({url})", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for WebError {}
