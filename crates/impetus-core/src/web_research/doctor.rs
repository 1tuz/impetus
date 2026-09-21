use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    BrowserService, BrowserServiceStatus, SearchRequest, WebOutcome, WebResearchEngine,
    WebResearchService,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BackendDoctorStatus {
    BuiltIn,
    Configured,
    Reachable,
    Unavailable { reason: String },
    Misconfigured { reason: String },
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchBackendDoctorEntry {
    pub id: String,
    pub status: BackendDoctorStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebDoctorReport {
    pub search_backends: Vec<SearchBackendDoctorEntry>,
    pub browser: BrowserServiceStatus,
    pub live_probe_performed: bool,
    pub notes: Vec<String>,
}

pub struct WebDoctor;

impl WebDoctor {
    /// Offline-safe inspection. No network requests and no secret values are included.
    pub fn inspect(engine: &WebResearchEngine, browser: BrowserServiceStatus) -> WebDoctorReport {
        let mut search_backends = engine
            .builtin_backend_ids()
            .map(|id| SearchBackendDoctorEntry {
                id: id.to_string(),
                status: BackendDoctorStatus::BuiltIn,
            })
            .collect::<Vec<_>>();
        search_backends.extend(
            engine
                .external_backend_ids()
                .map(|id| SearchBackendDoctorEntry {
                    id: id.to_string(),
                    status: BackendDoctorStatus::Configured,
                }),
        );

        WebDoctorReport {
            search_backends,
            browser,
            live_probe_performed: false,
            notes: vec![
                "offline inspection only; run the explicit live probe to test outbound search"
                    .into(),
            ],
        }
    }

    /// Preferred live probe when the concrete engine is available: probe every configured
    /// backend once instead of stopping when the normal fallback chain finds a working result.
    ///
    /// Browser status defaults to honest absent/unavailable unless a provider is supplied.
    pub async fn probe_engine(engine: &WebResearchEngine) -> WebDoctorReport {
        Self::probe_engine_with_browser(engine, &super::AbsentBrowserService).await
    }

    pub async fn probe_engine_with_browser(
        engine: &WebResearchEngine,
        browser: &dyn BrowserService,
    ) -> WebDoctorReport {
        let mut report = WebDoctorReport {
            search_backends: Vec::new(),
            browser: browser.status().await,
            live_probe_performed: true,
            notes: Vec::new(),
        };
        let primary = engine.default_backend_id().to_string();
        let mut primary_healthy = false;
        let mut fallback_healthy = false;

        for (id, result) in engine
            .probe_search_backends(SearchRequest::new("Example Domain"))
            .await
        {
            let status = match result {
                Ok(response) => match response.outcome {
                    WebOutcome::Success | WebOutcome::NoResults => {
                        if id == primary {
                            primary_healthy = true;
                        } else {
                            fallback_healthy = true;
                        }
                        BackendDoctorStatus::Reachable
                    }
                    WebOutcome::Blocked => BackendDoctorStatus::Unavailable {
                        reason: response
                            .attempts
                            .last()
                            .and_then(|attempt| attempt.detail.clone())
                            .unwrap_or_else(|| "outbound search was blocked".into()),
                    },
                    WebOutcome::Failed => BackendDoctorStatus::Failed {
                        reason: response
                            .attempts
                            .last()
                            .and_then(|attempt| attempt.detail.clone())
                            .unwrap_or_else(|| "search backend probe failed".into()),
                    },
                },
                Err(error) => {
                    if error.blocked() {
                        BackendDoctorStatus::Unavailable {
                            reason: error.message,
                        }
                    } else {
                        BackendDoctorStatus::Failed {
                            reason: error.message,
                        }
                    }
                }
            };
            report
                .search_backends
                .push(SearchBackendDoctorEntry { id, status });
        }

        if !primary_healthy && fallback_healthy {
            report.notes.push(
                "DEGRADED - primary search backend is unavailable, but a fallback is reachable"
                    .into(),
            );
        }
        report
    }

    /// Explicit live probe for `impetus doctor --probe-network` style wiring.
    /// Use this generic contract path only when the concrete engine is unavailable; unlike
    /// `probe_engine`, it observes the normal fallback chain and may not contact every backend.
    pub async fn probe_search(service: &dyn WebResearchService) -> WebDoctorReport {
        Self::probe_search_with_browser(service, &super::AbsentBrowserService).await
    }

    pub async fn probe_search_with_browser(
        service: &dyn WebResearchService,
        browser: &dyn BrowserService,
    ) -> WebDoctorReport {
        let mut report = WebDoctorReport {
            search_backends: Vec::new(),
            browser: browser.status().await,
            live_probe_performed: true,
            notes: Vec::new(),
        };

        let request = SearchRequest::new("Example Domain");
        match service.search(request).await {
            Ok(response) => {
                let mut by_backend: BTreeMap<String, BackendDoctorStatus> = BTreeMap::new();
                let mut fallback_used = false;
                for attempt in &response.attempts {
                    let status = match attempt.outcome {
                        WebOutcome::Success | WebOutcome::NoResults => {
                            BackendDoctorStatus::Reachable
                        }
                        WebOutcome::Blocked => BackendDoctorStatus::Unavailable {
                            reason: attempt.detail.clone().unwrap_or_else(|| {
                                "outbound search was blocked by policy or network egress".into()
                            }),
                        },
                        WebOutcome::Failed => BackendDoctorStatus::Failed {
                            reason: attempt
                                .detail
                                .clone()
                                .unwrap_or_else(|| "search backend probe failed".into()),
                        },
                    };
                    let entry = by_backend
                        .entry(attempt.backend.clone())
                        .or_insert_with(|| status.clone());
                    if matches!(status, BackendDoctorStatus::Reachable) {
                        *entry = BackendDoctorStatus::Reachable;
                    }
                    if attempt.backend != response.backend
                        && !matches!(attempt.outcome, WebOutcome::Success | WebOutcome::NoResults)
                    {
                        fallback_used = true;
                    }
                }
                if by_backend.is_empty() {
                    by_backend.insert(
                        response.backend.clone(),
                        match response.outcome {
                            WebOutcome::Success | WebOutcome::NoResults => {
                                BackendDoctorStatus::Reachable
                            }
                            WebOutcome::Blocked => BackendDoctorStatus::Unavailable {
                                reason: "outbound search was blocked by policy or network egress"
                                    .into(),
                            },
                            WebOutcome::Failed => BackendDoctorStatus::Failed {
                                reason: "search backend probe failed".into(),
                            },
                        },
                    );
                }
                report.search_backends.extend(
                    by_backend
                        .into_iter()
                        .map(|(id, status)| SearchBackendDoctorEntry { id, status }),
                );
                if fallback_used && response.outcome == WebOutcome::Success {
                    report.notes.push(
                        "DEGRADED - primary search path failed, but a fallback backend is available"
                            .into(),
                    );
                }
            }
            Err(error) => {
                report.search_backends.push(SearchBackendDoctorEntry {
                    id: "web_search".into(),
                    status: if error.blocked() {
                        BackendDoctorStatus::Unavailable {
                            reason: error.message,
                        }
                    } else {
                        BackendDoctorStatus::Failed {
                            reason: error.message,
                        }
                    },
                });
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_research::{
        BrowserCapability, BrowserServiceStatus, EgressPolicy, MockBrowserMode,
        MockBrowserProvider, ProviderBackedBrowserService, WebResearchEngine,
    };
    use std::sync::Arc;

    #[test]
    fn inspect_reports_absent_browser_honestly() {
        let engine = WebResearchEngine::production(EgressPolicy::default());
        let report = WebDoctor::inspect(&engine, BrowserServiceStatus::absent());
        assert_eq!(report.browser, BrowserServiceStatus::absent());
        assert!(!report.live_probe_performed);
    }

    #[tokio::test]
    async fn probe_engine_with_mock_degraded_browser() {
        let engine = WebResearchEngine::production(EgressPolicy::default());
        let provider = Arc::new(MockBrowserProvider::new(MockBrowserMode::Degraded));
        let browser = ProviderBackedBrowserService::new(provider);
        let report = WebDoctor::probe_engine_with_browser(&engine, &browser).await;
        assert!(matches!(
            report.browser,
            BrowserServiceStatus::Degraded { .. }
        ));
        assert!(report.live_probe_performed);
    }

    #[tokio::test]
    async fn probe_engine_defaults_to_absent_browser() {
        let engine = WebResearchEngine::production(EgressPolicy::default());
        let report = WebDoctor::probe_engine(&engine).await;
        assert_eq!(report.browser, BrowserServiceStatus::absent());
    }

    #[test]
    fn available_status_carries_capabilities() {
        let status = BrowserServiceStatus::Available {
            provider_id: "mock".into(),
            capabilities: vec![BrowserCapability::Navigate],
        };
        assert!(status.is_usable());
    }
}
