use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{FetchRequest, FetchedPage, WebError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    Unavailable,
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

/// Optional Tier-2 backend contract. Concrete providers should be registered and lifecycle-managed
/// by Module Runtime so discovery, health, compatibility and isolation stay out of Agent Loop.
///
/// This intentionally models only the browser surface needed by web research. Rich interactive
/// operations (click/type/screenshot/session/page refs) can be added as capability-negotiated
/// provider methods without making Chromium/Playwright/Node mandatory core dependencies.
#[async_trait]
pub trait BrowserProvider: Send + Sync {
    fn browser_descriptor(&self) -> BrowserProviderDescriptor;
    async fn browser_status(&self) -> BrowserServiceStatus;
    async fn fetch_rendered(&self, request: FetchRequest) -> Result<FetchedPage, WebError>;
}

#[async_trait]
pub trait BrowserService: Send + Sync {
    async fn status(&self) -> BrowserServiceStatus;
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

    async fn fetch_rendered(&self, request: FetchRequest) -> Result<FetchedPage, WebError> {
        self.provider.fetch_rendered(request).await
    }
}
