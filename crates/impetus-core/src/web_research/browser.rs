use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{
    CitationSource, FetchBodyKind, FetchRequest, FetchedPage, WebError, WebErrorKind, WebOutcome,
};

/// Semantic protocol version aligned with the audited JCode draft (`0.1`).
/// See `docs/BROWSER_PROVIDER_PROTOCOL.md`.
pub const BROWSER_PROVIDER_PROTOCOL_VERSION: &str = "0.1";

pub const ABSENT_BROWSER_REASON: &str = "no browser provider registered (optional track)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCapability {
    Navigate,
    Snapshot,
    RenderedReadableText,
    RenderedLinks,
    Screenshot,
    Click,
    Type,
    Wait,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserProviderDescriptor {
    pub provider_id: String,
    pub provider_version: Option<String>,
    pub protocol_version: String,
    pub browser_families: Vec<String>,
    pub capabilities: Vec<BrowserCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BrowserServiceStatus {
    Unavailable {
        reason: String,
    },
    Degraded {
        reason: String,
    },
    Misconfigured {
        reason: String,
    },
    Available {
        provider_id: String,
        capabilities: Vec<BrowserCapability>,
    },
}

impl BrowserServiceStatus {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    pub fn absent() -> Self {
        Self::unavailable(ABSENT_BROWSER_REASON)
    }

    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Available { .. } | Self::Degraded { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNegotiateRequest {
    pub protocol_version: String,
    #[serde(default)]
    pub required_capabilities: Vec<BrowserCapability>,
    #[serde(default)]
    pub optional_capabilities: Vec<BrowserCapability>,
}

impl BrowserNegotiateRequest {
    pub fn for_protocol(protocol_version: impl Into<String>) -> Self {
        Self {
            protocol_version: protocol_version.into(),
            required_capabilities: Vec::new(),
            optional_capabilities: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNegotiateResult {
    pub protocol_version: String,
    pub compatible: bool,
    pub provider: BrowserProviderDescriptor,
    pub granted_capabilities: Vec<BrowserCapability>,
    pub missing_required: Vec<BrowserCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSessionEnsureRequest {
    pub client_session_id: String,
    #[serde(default)]
    pub browser_preference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSessionHandle {
    pub session_id: String,
    pub browser_family: Option<String>,
    pub default_page_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNavigateRequest {
    pub session_id: String,
    pub url: String,
    #[serde(default)]
    pub page_id: Option<String>,
    #[serde(default)]
    pub new_page: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNavigateResult {
    pub page_id: String,
    pub url: String,
    pub title: Option<String>,
}

/// Optional Tier-2 backend contract. Concrete providers should be registered and lifecycle-managed
/// by Module Runtime so discovery, health, compatibility and isolation stay out of Agent Loop.
///
/// Negotiation / health / session ops follow the JCode Browser Provider Protocol reference
/// (`docs/BROWSER_PROVIDER_PROTOCOL.md`) without mandating Chromium/Playwright/Node.
#[async_trait]
pub trait BrowserProvider: Send + Sync {
    fn browser_descriptor(&self) -> BrowserProviderDescriptor;
    async fn browser_status(&self) -> BrowserServiceStatus;
    async fn negotiate(
        &self,
        request: BrowserNegotiateRequest,
    ) -> Result<BrowserNegotiateResult, WebError>;
    async fn ensure_session(
        &self,
        request: BrowserSessionEnsureRequest,
    ) -> Result<BrowserSessionHandle, WebError>;
    async fn close_session(&self, session_id: &str) -> Result<(), WebError>;
    async fn navigate(
        &self,
        request: BrowserNavigateRequest,
    ) -> Result<BrowserNavigateResult, WebError>;
    async fn fetch_rendered(&self, request: FetchRequest) -> Result<FetchedPage, WebError>;
}

#[async_trait]
pub trait BrowserService: Send + Sync {
    async fn status(&self) -> BrowserServiceStatus;
    async fn negotiate(
        &self,
        request: BrowserNegotiateRequest,
    ) -> Result<BrowserNegotiateResult, WebError>;
    async fn ensure_session(
        &self,
        request: BrowserSessionEnsureRequest,
    ) -> Result<BrowserSessionHandle, WebError>;
    async fn close_session(&self, session_id: &str) -> Result<(), WebError>;
    async fn navigate(
        &self,
        request: BrowserNavigateRequest,
    ) -> Result<BrowserNavigateResult, WebError>;
    async fn fetch_rendered(&self, request: FetchRequest) -> Result<FetchedPage, WebError>;
}

/// Thin facade used by web research. Agent Loop sees `BrowserService`, never a concrete bridge.
pub struct ProviderBackedBrowserService {
    provider: Arc<dyn BrowserProvider>,
}

impl ProviderBackedBrowserService {
    pub fn new(provider: Arc<dyn BrowserProvider>) -> Self {
        Self { provider }
    }

    pub fn descriptor(&self) -> BrowserProviderDescriptor {
        self.provider.browser_descriptor()
    }
}

#[async_trait]
impl BrowserService for ProviderBackedBrowserService {
    async fn status(&self) -> BrowserServiceStatus {
        self.provider.browser_status().await
    }

    async fn negotiate(
        &self,
        request: BrowserNegotiateRequest,
    ) -> Result<BrowserNegotiateResult, WebError> {
        self.provider.negotiate(request).await
    }

    async fn ensure_session(
        &self,
        request: BrowserSessionEnsureRequest,
    ) -> Result<BrowserSessionHandle, WebError> {
        self.provider.ensure_session(request).await
    }

    async fn close_session(&self, session_id: &str) -> Result<(), WebError> {
        self.provider.close_session(session_id).await
    }

    async fn navigate(
        &self,
        request: BrowserNavigateRequest,
    ) -> Result<BrowserNavigateResult, WebError> {
        self.provider.navigate(request).await
    }

    async fn fetch_rendered(&self, request: FetchRequest) -> Result<FetchedPage, WebError> {
        self.provider.fetch_rendered(request).await
    }
}

/// Default production browser surface when no optional provider is registered.
pub struct AbsentBrowserService;

#[async_trait]
impl BrowserService for AbsentBrowserService {
    async fn status(&self) -> BrowserServiceStatus {
        BrowserServiceStatus::absent()
    }

    async fn negotiate(
        &self,
        _request: BrowserNegotiateRequest,
    ) -> Result<BrowserNegotiateResult, WebError> {
        Err(WebError::new(
            WebErrorKind::BackendUnavailable,
            ABSENT_BROWSER_REASON,
        ))
    }

    async fn ensure_session(
        &self,
        _request: BrowserSessionEnsureRequest,
    ) -> Result<BrowserSessionHandle, WebError> {
        Err(WebError::new(
            WebErrorKind::BackendUnavailable,
            ABSENT_BROWSER_REASON,
        ))
    }

    async fn close_session(&self, _session_id: &str) -> Result<(), WebError> {
        Err(WebError::new(
            WebErrorKind::BackendUnavailable,
            ABSENT_BROWSER_REASON,
        ))
    }

    async fn navigate(
        &self,
        _request: BrowserNavigateRequest,
    ) -> Result<BrowserNavigateResult, WebError> {
        Err(WebError::new(
            WebErrorKind::BackendUnavailable,
            ABSENT_BROWSER_REASON,
        ))
    }

    async fn fetch_rendered(&self, request: FetchRequest) -> Result<FetchedPage, WebError> {
        Err(
            WebError::new(WebErrorKind::BackendUnavailable, ABSENT_BROWSER_REASON)
                .with_url(request.url),
        )
    }
}

/// CI / unit mock: no browser binary. Proves negotiate → session → navigate → close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockBrowserMode {
    Available,
    Degraded,
    Unavailable,
    Misconfigured,
}

#[derive(Debug)]
struct MockSessionState {
    page_counter: u64,
    pages: HashMap<String, String>,
}

#[derive(Debug)]
pub struct MockBrowserProvider {
    mode: MockBrowserMode,
    sessions: Mutex<HashMap<String, MockSessionState>>,
}

impl MockBrowserProvider {
    pub fn new(mode: MockBrowserMode) -> Self {
        Self {
            mode,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn available() -> Self {
        Self::new(MockBrowserMode::Available)
    }

    fn descriptor_inner(&self) -> BrowserProviderDescriptor {
        BrowserProviderDescriptor {
            provider_id: "mock".into(),
            provider_version: Some("0.1.0".into()),
            protocol_version: BROWSER_PROVIDER_PROTOCOL_VERSION.into(),
            browser_families: vec!["mock".into()],
            capabilities: vec![
                BrowserCapability::Navigate,
                BrowserCapability::RenderedReadableText,
                BrowserCapability::RenderedLinks,
            ],
        }
    }

    fn negotiate_inner(
        &self,
        request: BrowserNegotiateRequest,
    ) -> Result<BrowserNegotiateResult, WebError> {
        if !protocol_compatible(&request.protocol_version, BROWSER_PROVIDER_PROTOCOL_VERSION) {
            return Err(WebError::new(
                WebErrorKind::Configuration,
                format!(
                    "incompatible browser protocol version: requested {}, provider {}",
                    request.protocol_version, BROWSER_PROVIDER_PROTOCOL_VERSION
                ),
            ));
        }

        let provider = self.descriptor_inner();
        let offered: Vec<BrowserCapability> = provider.capabilities.clone();
        let missing_required: Vec<BrowserCapability> = request
            .required_capabilities
            .iter()
            .copied()
            .filter(|cap| !offered.contains(cap))
            .collect();
        let mut granted: Vec<BrowserCapability> = request
            .required_capabilities
            .iter()
            .copied()
            .filter(|cap| offered.contains(cap))
            .collect();
        for cap in &request.optional_capabilities {
            if offered.contains(cap) && !granted.contains(cap) {
                granted.push(*cap);
            }
        }
        // When caller asks for nothing specific, grant the provider's advertised set.
        if request.required_capabilities.is_empty() && request.optional_capabilities.is_empty() {
            granted = offered;
        }

        Ok(BrowserNegotiateResult {
            protocol_version: BROWSER_PROVIDER_PROTOCOL_VERSION.into(),
            compatible: missing_required.is_empty(),
            provider,
            granted_capabilities: granted,
            missing_required,
        })
    }
}

fn protocol_compatible(requested: &str, offered: &str) -> bool {
    major_component(requested) == major_component(offered)
}

fn major_component(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

#[async_trait]
impl BrowserProvider for MockBrowserProvider {
    fn browser_descriptor(&self) -> BrowserProviderDescriptor {
        self.descriptor_inner()
    }

    async fn browser_status(&self) -> BrowserServiceStatus {
        match self.mode {
            MockBrowserMode::Available => BrowserServiceStatus::Available {
                provider_id: "mock".into(),
                capabilities: self.descriptor_inner().capabilities,
            },
            MockBrowserMode::Degraded => BrowserServiceStatus::Degraded {
                reason: "mock provider running in degraded mode".into(),
            },
            MockBrowserMode::Unavailable => BrowserServiceStatus::Unavailable {
                reason: "mock provider forced unavailable".into(),
            },
            MockBrowserMode::Misconfigured => BrowserServiceStatus::Misconfigured {
                reason: "mock provider forced misconfigured".into(),
            },
        }
    }

    async fn negotiate(
        &self,
        request: BrowserNegotiateRequest,
    ) -> Result<BrowserNegotiateResult, WebError> {
        match self.mode {
            MockBrowserMode::Unavailable => Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                "mock provider forced unavailable",
            )),
            MockBrowserMode::Misconfigured => Err(WebError::new(
                WebErrorKind::Configuration,
                "mock provider forced misconfigured",
            )),
            MockBrowserMode::Available | MockBrowserMode::Degraded => self.negotiate_inner(request),
        }
    }

    async fn ensure_session(
        &self,
        request: BrowserSessionEnsureRequest,
    ) -> Result<BrowserSessionHandle, WebError> {
        if !matches!(
            self.mode,
            MockBrowserMode::Available | MockBrowserMode::Degraded
        ) {
            return Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                "mock provider cannot open sessions in current mode",
            ));
        }

        let session_id = format!("mock-sess-{}", request.client_session_id);
        let mut sessions = self.sessions.lock().map_err(|_| {
            WebError::new(
                WebErrorKind::BackendUnavailable,
                "mock session lock poisoned",
            )
        })?;
        sessions
            .entry(session_id.clone())
            .or_insert_with(|| MockSessionState {
                page_counter: 0,
                pages: HashMap::new(),
            });

        Ok(BrowserSessionHandle {
            session_id,
            browser_family: Some("mock".into()),
            default_page_id: Some("page_0".into()),
        })
    }

    async fn close_session(&self, session_id: &str) -> Result<(), WebError> {
        let mut sessions = self.sessions.lock().map_err(|_| {
            WebError::new(
                WebErrorKind::BackendUnavailable,
                "mock session lock poisoned",
            )
        })?;
        if sessions.remove(session_id).is_none() {
            return Err(WebError::new(
                WebErrorKind::InvalidRequest,
                format!("unknown browser session: {session_id}"),
            ));
        }
        Ok(())
    }

    async fn navigate(
        &self,
        request: BrowserNavigateRequest,
    ) -> Result<BrowserNavigateResult, WebError> {
        if !matches!(
            self.mode,
            MockBrowserMode::Available | MockBrowserMode::Degraded
        ) {
            return Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                "mock provider cannot navigate in current mode",
            ));
        }

        let mut sessions = self.sessions.lock().map_err(|_| {
            WebError::new(
                WebErrorKind::BackendUnavailable,
                "mock session lock poisoned",
            )
        })?;
        let state = sessions.get_mut(&request.session_id).ok_or_else(|| {
            WebError::new(
                WebErrorKind::InvalidRequest,
                format!("unknown browser session: {}", request.session_id),
            )
        })?;

        let page_id = if let Some(existing) = request.page_id.filter(|_| !request.new_page) {
            existing
        } else {
            state.page_counter += 1;
            format!("page_{}", state.page_counter)
        };
        state.pages.insert(page_id.clone(), request.url.clone());

        Ok(BrowserNavigateResult {
            page_id,
            url: request.url,
            title: Some("Mock Page".into()),
        })
    }

    async fn fetch_rendered(&self, request: FetchRequest) -> Result<FetchedPage, WebError> {
        if !matches!(
            self.mode,
            MockBrowserMode::Available | MockBrowserMode::Degraded
        ) {
            return Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                "mock provider cannot fetch in current mode",
            )
            .with_url(request.url));
        }

        let url = request.url.clone();
        Ok(FetchedPage {
            outcome: WebOutcome::Success,
            requested_url: url.clone(),
            final_url: url.clone(),
            redirect_chain: vec![url.clone()],
            status_code: 200,
            content_type: Some("text/html; charset=utf-8".into()),
            body_kind: FetchBodyKind::Html,
            title: Some("Mock Page".into()),
            text: format!("mock rendered text for {url}"),
            links: Vec::new(),
            truncated: false,
            citation: CitationSource {
                citation_id: "mock-cite-1".into(),
                url: url.clone(),
                title: Some("Mock Page".into()),
                artifact: None,
            },
            fetched_unix_ms: 0,
            content_sha256: "mock".into(),
            elapsed_ms: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn absent_service_reports_unavailable() {
        let service = AbsentBrowserService;
        let status = service.status().await;
        assert_eq!(status, BrowserServiceStatus::absent());
        assert!(!status.is_usable());
        let err = service
            .negotiate(BrowserNegotiateRequest::for_protocol(
                BROWSER_PROVIDER_PROTOCOL_VERSION,
            ))
            .await
            .expect_err("absent negotiate");
        assert_eq!(err.kind, WebErrorKind::BackendUnavailable);
    }

    #[tokio::test]
    async fn mock_negotiate_session_navigate_close() {
        let provider = Arc::new(MockBrowserProvider::available());
        let service = ProviderBackedBrowserService::new(provider);

        let status = service.status().await;
        assert!(matches!(status, BrowserServiceStatus::Available { .. }));

        let negotiated = service
            .negotiate(BrowserNegotiateRequest {
                protocol_version: BROWSER_PROVIDER_PROTOCOL_VERSION.into(),
                required_capabilities: vec![BrowserCapability::Navigate],
                optional_capabilities: vec![BrowserCapability::Screenshot],
            })
            .await
            .expect("negotiate");
        assert!(negotiated.compatible);
        assert!(
            negotiated
                .granted_capabilities
                .contains(&BrowserCapability::Navigate)
        );
        assert!(
            !negotiated
                .granted_capabilities
                .contains(&BrowserCapability::Screenshot)
        );

        let session = service
            .ensure_session(BrowserSessionEnsureRequest {
                client_session_id: "agent-1".into(),
                browser_preference: Some("auto".into()),
            })
            .await
            .expect("ensure");
        assert!(session.session_id.contains("agent-1"));

        let nav = service
            .navigate(BrowserNavigateRequest {
                session_id: session.session_id.clone(),
                url: "https://example.com".into(),
                page_id: None,
                new_page: true,
            })
            .await
            .expect("navigate");
        assert_eq!(nav.url, "https://example.com");
        assert!(!nav.page_id.is_empty());

        service
            .close_session(&session.session_id)
            .await
            .expect("close");
    }

    #[tokio::test]
    async fn mock_degraded_status_still_negotiates() {
        let provider = Arc::new(MockBrowserProvider::new(MockBrowserMode::Degraded));
        let status = provider.browser_status().await;
        assert!(matches!(
            status,
            BrowserServiceStatus::Degraded { reason } if reason.contains("degraded")
        ));
        let result = provider
            .negotiate(BrowserNegotiateRequest::for_protocol(
                BROWSER_PROVIDER_PROTOCOL_VERSION,
            ))
            .await
            .expect("degraded negotiate");
        assert!(result.compatible);
    }

    #[tokio::test]
    async fn mock_rejects_incompatible_protocol_major() {
        let provider = MockBrowserProvider::available();
        let err = provider
            .negotiate(BrowserNegotiateRequest::for_protocol("1.0"))
            .await
            .expect_err("major mismatch");
        assert_eq!(err.kind, WebErrorKind::Configuration);
    }

    #[tokio::test]
    async fn mock_missing_required_capability_marks_incompatible() {
        let provider = MockBrowserProvider::available();
        let result = provider
            .negotiate(BrowserNegotiateRequest {
                protocol_version: BROWSER_PROVIDER_PROTOCOL_VERSION.into(),
                required_capabilities: vec![BrowserCapability::Click],
                optional_capabilities: Vec::new(),
            })
            .await
            .expect("negotiate");
        assert!(!result.compatible);
        assert_eq!(result.missing_required, vec![BrowserCapability::Click]);
    }
}
