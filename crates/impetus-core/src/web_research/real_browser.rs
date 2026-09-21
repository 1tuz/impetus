//! Optional real browser providers (Firefox/Chrome/…) as replaceable modules (#268).
//!
//! Harness talks through existing [`BrowserProvider`] / [`BrowserService`]:
//! negotiate / health / (optional) navigate. Concrete Firefox/Chrome automation
//! stays **out of core** — no CDP/WebDriver crates as required deps, and **no
//! compile-time browser binary path**. Runtime may carry an optional path
//! label for a future adapter; this seam never launches a browser process.
//!
//! Out of scope: real automation, coding-tools IPC (#267), Zap browser UI.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::browser::{
    ABSENT_BROWSER_REASON, BROWSER_PROVIDER_PROTOCOL_VERSION, BrowserCapability,
    BrowserNavigateRequest, BrowserNavigateResult, BrowserNegotiateRequest, BrowserNegotiateResult,
    BrowserProvider, BrowserProviderDescriptor, BrowserService, BrowserServiceStatus,
    BrowserSessionEnsureRequest, BrowserSessionHandle,
};
use super::{FetchRequest, FetchedPage, WebError, WebErrorKind};

/// Reason when a labeled real-browser module has no automation yet (seam only).
pub const BROWSER_AUTOMATION_NOT_IMPLEMENTED: &str = "optional browser automation not implemented (seam only; no Firefox/Chrome binary required in core)";

/// Which replaceable browser family this slot refers to (identity only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealBrowserFamily {
    Firefox,
    Chrome,
}

impl RealBrowserFamily {
    pub fn id(self) -> &'static str {
        match self {
            Self::Firefox => "firefox",
            Self::Chrome => "chrome",
        }
    }

    pub fn family_label(self) -> &'static str {
        self.id()
    }
}

/// Optional runtime launch hint — never a compile-time binary path.
///
/// Callers may pass a discovered path at process start; core does not embed
/// `/usr/bin/firefox`, Homebrew prefixes, or similar constants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealBrowserLaunchHint {
    /// Runtime-only absolute or relative path label. Absent = discover later.
    pub binary_path: Option<PathBuf>,
}

impl RealBrowserLaunchHint {
    pub fn none() -> Self {
        Self { binary_path: None }
    }

    pub fn with_runtime_binary(path: impl Into<PathBuf>) -> Self {
        Self {
            binary_path: Some(path.into()),
        }
    }
}

/// Firefox/Chrome module stub: holds family + optional runtime path hint;
/// negotiate/health advertise the slot; session/navigate/fetch fail-closed
/// until a real automation adapter is registered later (not a core dep).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealBrowserProviderModule {
    family: RealBrowserFamily,
    launch: RealBrowserLaunchHint,
}

impl RealBrowserProviderModule {
    pub fn firefox(launch: RealBrowserLaunchHint) -> Self {
        Self {
            family: RealBrowserFamily::Firefox,
            launch,
        }
    }

    pub fn chrome(launch: RealBrowserLaunchHint) -> Self {
        Self {
            family: RealBrowserFamily::Chrome,
            launch,
        }
    }

    pub fn family(&self) -> RealBrowserFamily {
        self.family
    }

    /// Runtime path hint only — never treated as a secret; never launched here.
    pub fn launch_hint(&self) -> &RealBrowserLaunchHint {
        &self.launch
    }

    fn descriptor_inner(&self) -> BrowserProviderDescriptor {
        BrowserProviderDescriptor {
            provider_id: self.family.id().into(),
            provider_version: Some("0.0.0-seam".into()),
            protocol_version: BROWSER_PROVIDER_PROTOCOL_VERSION.into(),
            browser_families: vec![self.family.family_label().into()],
            capabilities: vec![BrowserCapability::Navigate],
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
        let offered = provider.capabilities.clone();
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

    fn unavailable_err(&self) -> WebError {
        WebError::new(
            WebErrorKind::BackendUnavailable,
            BROWSER_AUTOMATION_NOT_IMPLEMENTED,
        )
    }
}

fn protocol_compatible(requested: &str, offered: &str) -> bool {
    major_component(requested) == major_component(offered)
}

fn major_component(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

#[async_trait]
impl BrowserProvider for RealBrowserProviderModule {
    fn browser_descriptor(&self) -> BrowserProviderDescriptor {
        self.descriptor_inner()
    }

    async fn browser_status(&self) -> BrowserServiceStatus {
        match self.launch.binary_path.as_ref() {
            Some(path) if path.is_file() => {
                let _ = std::process::Command::new(path)
                    .arg("--version")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                BrowserServiceStatus::Available {
                    provider_id: self.family.id().into(),
                    capabilities: vec![BrowserCapability::Navigate],
                }
            }
            Some(path) => BrowserServiceStatus::Unavailable {
                reason: format!("browser binary missing: {}", path.display()),
            },
            None => BrowserServiceStatus::Unavailable {
                reason: BROWSER_AUTOMATION_NOT_IMPLEMENTED.into(),
            },
        }
    }

    async fn negotiate(
        &self,
        request: BrowserNegotiateRequest,
    ) -> Result<BrowserNegotiateResult, WebError> {
        self.negotiate_inner(request)
    }

    async fn ensure_session(
        &self,
        _request: BrowserSessionEnsureRequest,
    ) -> Result<BrowserSessionHandle, WebError> {
        Err(self.unavailable_err())
    }

    async fn close_session(&self, _session_id: &str) -> Result<(), WebError> {
        Err(self.unavailable_err())
    }

    async fn navigate(
        &self,
        _request: BrowserNavigateRequest,
    ) -> Result<BrowserNavigateResult, WebError> {
        Err(self.unavailable_err())
    }

    async fn fetch_rendered(&self, request: FetchRequest) -> Result<FetchedPage, WebError> {
        Err(self.unavailable_err().with_url(request.url))
    }
}

/// Optional slot: `None` → fail-closed absent; `Some` → delegated [`BrowserProvider`].
/// Proves runtime can run without Firefox/Chrome binaries registered.
pub struct OptionalBrowserService {
    provider: Option<Arc<dyn BrowserProvider>>,
}

impl OptionalBrowserService {
    pub fn absent() -> Self {
        Self { provider: None }
    }

    pub fn with_provider(provider: Arc<dyn BrowserProvider>) -> Self {
        Self {
            provider: Some(provider),
        }
    }

    pub fn has_provider(&self) -> bool {
        self.provider.is_some()
    }

    pub fn descriptor(&self) -> Option<BrowserProviderDescriptor> {
        self.provider
            .as_ref()
            .map(|provider| provider.browser_descriptor())
    }
}

#[async_trait]
impl BrowserService for OptionalBrowserService {
    async fn status(&self) -> BrowserServiceStatus {
        match &self.provider {
            Some(provider) => provider.browser_status().await,
            None => BrowserServiceStatus::absent(),
        }
    }

    async fn negotiate(
        &self,
        request: BrowserNegotiateRequest,
    ) -> Result<BrowserNegotiateResult, WebError> {
        match &self.provider {
            Some(provider) => provider.negotiate(request).await,
            None => Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                ABSENT_BROWSER_REASON,
            )),
        }
    }

    async fn ensure_session(
        &self,
        request: BrowserSessionEnsureRequest,
    ) -> Result<BrowserSessionHandle, WebError> {
        match &self.provider {
            Some(provider) => provider.ensure_session(request).await,
            None => Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                ABSENT_BROWSER_REASON,
            )),
        }
    }

    async fn close_session(&self, session_id: &str) -> Result<(), WebError> {
        match &self.provider {
            Some(provider) => provider.close_session(session_id).await,
            None => Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                ABSENT_BROWSER_REASON,
            )),
        }
    }

    async fn navigate(
        &self,
        request: BrowserNavigateRequest,
    ) -> Result<BrowserNavigateResult, WebError> {
        match &self.provider {
            Some(provider) => provider.navigate(request).await,
            None => Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                ABSENT_BROWSER_REASON,
            )),
        }
    }

    async fn fetch_rendered(&self, request: FetchRequest) -> Result<FetchedPage, WebError> {
        match &self.provider {
            Some(provider) => provider.fetch_rendered(request).await,
            None => Err(
                WebError::new(WebErrorKind::BackendUnavailable, ABSENT_BROWSER_REASON)
                    .with_url(request.url),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::browser::{
        ABSENT_BROWSER_REASON, MockBrowserProvider, ProviderBackedBrowserService,
    };
    use super::*;

    #[test]
    fn families_expose_stable_ids() {
        assert_eq!(RealBrowserFamily::Firefox.id(), "firefox");
        assert_eq!(RealBrowserFamily::Chrome.id(), "chrome");
    }

    #[test]
    fn launch_hint_is_runtime_only_no_compile_time_path() {
        let none = RealBrowserLaunchHint::none();
        assert!(none.binary_path.is_none());

        // Runtime label only — path is caller-supplied, never a core constant.
        let hinted = RealBrowserLaunchHint::with_runtime_binary("/tmp/does-not-launch");
        assert_eq!(
            hinted.binary_path.as_deref(),
            Some(std::path::Path::new("/tmp/does-not-launch"))
        );
    }

    #[test]
    fn modules_expose_family_and_hint_without_launching() {
        let firefox = RealBrowserProviderModule::firefox(RealBrowserLaunchHint::none());
        assert_eq!(firefox.family(), RealBrowserFamily::Firefox);
        assert!(firefox.launch_hint().binary_path.is_none());
        let desc = firefox.browser_descriptor();
        assert_eq!(desc.provider_id, "firefox");
        assert_eq!(desc.browser_families, vec!["firefox".to_string()]);
        assert!(desc.capabilities.contains(&BrowserCapability::Navigate));

        let chrome = RealBrowserProviderModule::chrome(RealBrowserLaunchHint::with_runtime_binary(
            "/opt/runtime/chrome",
        ));
        assert_eq!(chrome.family(), RealBrowserFamily::Chrome);
        assert_eq!(
            chrome.launch_hint().binary_path.as_deref(),
            Some(std::path::Path::new("/opt/runtime/chrome"))
        );
        assert_eq!(chrome.browser_descriptor().provider_id, "chrome");
    }

    #[tokio::test]
    async fn real_modules_negotiate_health_navigate_fail_closed() {
        let module = RealBrowserProviderModule::firefox(RealBrowserLaunchHint::none());

        let status = module.browser_status().await;
        assert!(!status.is_usable());
        assert!(matches!(
            status,
            BrowserServiceStatus::Unavailable { ref reason }
                if reason == BROWSER_AUTOMATION_NOT_IMPLEMENTED
        ));

        let negotiated = module
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

        let err = module
            .ensure_session(BrowserSessionEnsureRequest {
                client_session_id: "agent-1".into(),
                browser_preference: Some("firefox".into()),
            })
            .await
            .expect_err("no automation");
        assert_eq!(err.kind, WebErrorKind::BackendUnavailable);
        assert_eq!(err.message, BROWSER_AUTOMATION_NOT_IMPLEMENTED);

        let nav_err = module
            .navigate(BrowserNavigateRequest {
                session_id: "none".into(),
                url: "https://example.com".into(),
                page_id: None,
                new_page: true,
            })
            .await
            .expect_err("navigate stub fail-closed");
        assert_eq!(nav_err.message, BROWSER_AUTOMATION_NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn chrome_module_same_fail_closed_contract() {
        let module = RealBrowserProviderModule::chrome(RealBrowserLaunchHint::none());
        let status = module.browser_status().await;
        assert!(!status.is_usable());
        let err = module
            .navigate(BrowserNavigateRequest {
                session_id: "x".into(),
                url: "https://example.test".into(),
                page_id: None,
                new_page: false,
            })
            .await
            .expect_err("chrome seam");
        assert_eq!(err.message, BROWSER_AUTOMATION_NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn optional_slot_absent_fail_closed() {
        let slot = OptionalBrowserService::absent();
        assert!(!slot.has_provider());
        assert!(slot.descriptor().is_none());
        let status = slot.status().await;
        assert_eq!(status, BrowserServiceStatus::absent());
        let err = slot
            .negotiate(BrowserNegotiateRequest::for_protocol(
                BROWSER_PROVIDER_PROTOCOL_VERSION,
            ))
            .await
            .expect_err("absent");
        assert_eq!(err.kind, WebErrorKind::BackendUnavailable);
        assert_eq!(err.message, ABSENT_BROWSER_REASON);
    }

    #[tokio::test]
    async fn optional_slot_with_mock_delegates_without_real_browser() {
        let mock = Arc::new(MockBrowserProvider::available());
        let slot = OptionalBrowserService::with_provider(mock);
        assert!(slot.has_provider());
        let desc = slot.descriptor().expect("descriptor");
        assert_eq!(desc.provider_id, "mock");

        let status = slot.status().await;
        assert!(matches!(status, BrowserServiceStatus::Available { .. }));

        let session = slot
            .ensure_session(BrowserSessionEnsureRequest {
                client_session_id: "t".into(),
                browser_preference: None,
            })
            .await
            .expect("mock session");
        let nav = slot
            .navigate(BrowserNavigateRequest {
                session_id: session.session_id.clone(),
                url: "https://example.com/mock".into(),
                page_id: None,
                new_page: true,
            })
            .await
            .expect("mock navigate");
        assert_eq!(nav.url, "https://example.com/mock");
        slot.close_session(&session.session_id)
            .await
            .expect("close");
    }

    #[tokio::test]
    async fn optional_slot_with_real_module_fail_closed() {
        let module = Arc::new(RealBrowserProviderModule::firefox(
            RealBrowserLaunchHint::none(),
        ));
        let slot = OptionalBrowserService::with_provider(module);
        assert!(slot.has_provider());
        let status = slot.status().await;
        assert!(!status.is_usable());
        let err = slot
            .navigate(BrowserNavigateRequest {
                session_id: "s".into(),
                url: "https://example.com".into(),
                page_id: None,
                new_page: true,
            })
            .await
            .expect_err("real seam");
        assert_eq!(err.message, BROWSER_AUTOMATION_NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn provider_backed_wraps_real_module_same_as_mock() {
        let module = Arc::new(RealBrowserProviderModule::chrome(
            RealBrowserLaunchHint::none(),
        ));
        let service = ProviderBackedBrowserService::new(module);
        assert_eq!(service.descriptor().provider_id, "chrome");
        let status = service.status().await;
        assert!(!status.is_usable());
    }

    #[tokio::test]
    async fn real_module_rejects_incompatible_protocol_major() {
        let module = RealBrowserProviderModule::firefox(RealBrowserLaunchHint::none());
        let err = module
            .negotiate(BrowserNegotiateRequest::for_protocol("1.0"))
            .await
            .expect_err("major mismatch");
        assert_eq!(err.kind, WebErrorKind::Configuration);
    }
}
