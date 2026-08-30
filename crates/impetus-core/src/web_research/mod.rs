mod bing;
mod browser;
mod doctor;
mod duckduckgo;
mod html;
mod http;
mod searxng;
mod security;
mod service;
mod tool_adapter;
mod types;

pub use bing::BingHtmlSearchBackend;
pub use browser::{
    BrowserCapability, BrowserProvider, BrowserProviderDescriptor, BrowserService,
    BrowserServiceStatus, ProviderBackedBrowserService,
};
pub use doctor::{BackendDoctorStatus, SearchBackendDoctorEntry, WebDoctor, WebDoctorReport};
pub use duckduckgo::DuckDuckGoSearchBackend;
pub use http::{
    DnsResolver, HttpTransport, PreparedGet, PreparedPostForm, RawHttpResponse, ReqwestTransport,
    SecureHttpClient, SecureHttpResponse, SystemDnsResolver,
};
pub use searxng::SearxngSearchBackend;
pub use security::{AddressClass, EgressPolicy};
pub use service::{
    ArtifactPolicy, ExternalSearchBackend, SearchBackend, WebFetchService, WebResearchEngine,
    WebResearchService, WebSearchService,
};
pub use tool_adapter::{execute_web_tool, redact_url_for_observation};
pub use types::{
    CitationSource, DEFAULT_FETCH_MAX_BYTES, DEFAULT_FETCH_MAX_CHARS, DEFAULT_SEARCH_RESULTS,
    FetchBodyKind, FetchLink, FetchRequest, FetchedPage, MAX_FETCH_BYTES, MAX_FETCH_CHARS,
    MAX_FETCH_URL_CHARS, MAX_SEARCH_QUERY_CHARS, MAX_SEARCH_RESULTS, SafeSearch, SearchAttempt,
    SearchBackendPreference, SearchHit, SearchRequest, SearchResponse, WebCapability, WebError,
    WebErrorKind, WebObservation, WebOutcome, WebToolObservationDetail,
};
