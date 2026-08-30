use std::{collections::HashSet, sync::Arc, time::Instant};

use async_trait::async_trait;
use reqwest::Url;
use scraper::{ElementRef, Html, Selector};

use super::{
    SafeSearch, SearchAttempt, SearchHit, SearchRequest, SearchResponse, SecureHttpClient,
    WebError, WebErrorKind, WebOutcome,
    html::normalize_whitespace,
    service::{SearchBackend, citation_id},
};

const HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const LITE_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";
const SEARCH_BODY_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DuckDuckGoSearchBackend {
    http: Arc<SecureHttpClient>,
}

impl DuckDuckGoSearchBackend {
    pub fn new(http: Arc<SecureHttpClient>) -> Self {
        Self { http }
    }

    async fn attempt(
        &self,
        endpoint: &str,
        request: &SearchRequest,
        parser: fn(&[u8], usize) -> Vec<SearchHit>,
    ) -> Result<(Vec<SearchHit>, SearchAttempt), WebError> {
        let url = Url::parse(endpoint).map_err(|error| {
            WebError::new(
                WebErrorKind::Configuration,
                format!("invalid DuckDuckGo endpoint: {error}"),
            )
        })?;
        let response = self
            .http
            .post_form(url.as_str(), build_search_form(request), SEARCH_BODY_LIMIT)
            .await?;
        if !response.status.is_success() {
            return Err(WebError::new(
                WebErrorKind::HttpStatus,
                format!(
                    "DuckDuckGo endpoint returned HTTP {}",
                    response.status.as_u16()
                ),
            )
            .with_url(response.final_url));
        }
        let hits = parser(&response.body, request.normalized_limit());
        if hits.is_empty() && detect_anti_bot_page(&response.body) {
            return Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                "DuckDuckGo returned an anti-bot/challenge page instead of search results",
            )
            .with_url(response.final_url));
        }
        let outcome = if hits.is_empty() {
            WebOutcome::NoResults
        } else {
            WebOutcome::Success
        };
        Ok((
            hits,
            SearchAttempt {
                backend: self.id().to_string(),
                endpoint: endpoint.to_string(),
                outcome,
                detail: None,
            },
        ))
    }
}

#[async_trait]
impl SearchBackend for DuckDuckGoSearchBackend {
    fn id(&self) -> &str {
        "duckduckgo"
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, WebError> {
        let started = Instant::now();
        let mut attempts = Vec::new();
        let mut last_error = None;

        for (endpoint, parser) in [
            (
                HTML_ENDPOINT,
                parse_html_results as fn(&[u8], usize) -> Vec<SearchHit>,
            ),
            (
                LITE_ENDPOINT,
                parse_lite_results as fn(&[u8], usize) -> Vec<SearchHit>,
            ),
        ] {
            match self.attempt(endpoint, request, parser).await {
                Ok((hits, attempt)) => {
                    let outcome = attempt.outcome;
                    attempts.push(attempt);
                    if !hits.is_empty() {
                        return Ok(SearchResponse {
                            outcome: WebOutcome::Success,
                            backend: self.id().to_string(),
                            query: request.query.clone(),
                            hits,
                            attempts,
                            elapsed_ms: elapsed_ms(started),
                        });
                    }
                    if outcome == WebOutcome::Blocked {
                        break;
                    }
                }
                Err(error) => {
                    attempts.push(SearchAttempt {
                        backend: self.id().to_string(),
                        endpoint: endpoint.to_string(),
                        outcome: if error.blocked() {
                            WebOutcome::Blocked
                        } else {
                            WebOutcome::Failed
                        },
                        detail: Some(error.message.clone()),
                    });
                    let blocked = error.blocked();
                    last_error = Some(error);
                    if blocked {
                        break;
                    }
                }
            }
        }

        let outcome = if attempts
            .iter()
            .any(|attempt| attempt.outcome == WebOutcome::Blocked)
        {
            WebOutcome::Blocked
        } else if attempts
            .iter()
            .all(|attempt| attempt.outcome == WebOutcome::NoResults)
        {
            WebOutcome::NoResults
        } else {
            WebOutcome::Failed
        };
        let mut response = SearchResponse {
            outcome,
            backend: self.id().to_string(),
            query: request.query.clone(),
            hits: Vec::new(),
            attempts,
            elapsed_ms: elapsed_ms(started),
        };

        // Transport failures are represented as a stable failed response so the engine can
        // deterministically try an explicitly configured external fallback. Keep the error
        // detail in attempts rather than triggering another retry loop in Agent Loop.
        if response.attempts.is_empty()
            && let Some(error) = last_error
        {
            response.attempts.push(SearchAttempt {
                backend: self.id().to_string(),
                endpoint: "duckduckgo".into(),
                outcome: WebOutcome::Failed,
                detail: Some(error.message),
            });
        }
        Ok(response)
    }
}

fn build_search_form(request: &SearchRequest) -> Vec<(String, String)> {
    let mut form = vec![("q".into(), request.query.trim().into())];
    if let Some(locale) = request
        .locale
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        form.push(("kl".into(), locale.into()));
    }
    match request.safe_search {
        SafeSearch::Strict => form.push(("kp".into(), "1".into())),
        SafeSearch::Moderate => form.push(("kp".into(), "-1".into())),
        SafeSearch::Off => form.push(("kp".into(), "-2".into())),
    }
    form
}

fn detect_anti_bot_page(body: &[u8]) -> bool {
    let source = String::from_utf8_lossy(body).to_ascii_lowercase();
    [
        "anomaly-modal",
        "anomaly.js",
        "captcha",
        "g-recaptcha",
        "are you a robot",
        "unusual traffic",
        "verify you are human",
        "challenge-platform",
        "cf-challenge",
    ]
    .iter()
    .any(|marker| source.contains(marker))
}

fn parse_html_results(body: &[u8], max_results: usize) -> Vec<SearchHit> {
    let source = String::from_utf8_lossy(body);
    let document = Html::parse_document(&source);
    let result_selector = Selector::parse(".result").expect("static selector is valid");
    let link_selector = Selector::parse("a.result__a").expect("static selector is valid");
    let snippet_selector = Selector::parse(".result__snippet").expect("static selector is valid");
    let mut hits = Vec::new();
    let mut seen = HashSet::new();

    for result in document.select(&result_selector) {
        let Some(link) = result.select(&link_selector).next() else {
            continue;
        };
        let snippet = result.select(&snippet_selector).next();
        let Some(hit) = hit_from_elements(&link, snippet.as_ref(), hits.len() + 1) else {
            continue;
        };
        if !seen.insert(hit.url.clone()) {
            continue;
        }
        hits.push(hit);
        if hits.len() >= max_results {
            break;
        }
    }
    hits
}

fn parse_lite_results(body: &[u8], max_results: usize) -> Vec<SearchHit> {
    let source = String::from_utf8_lossy(body);
    let document = Html::parse_document(&source);
    let link_selector = Selector::parse("a.result-link").expect("static selector is valid");
    let snippet_selector = Selector::parse("td.result-snippet").expect("static selector is valid");
    let snippets: Vec<_> = document.select(&snippet_selector).collect();
    let mut hits = Vec::new();
    let mut seen = HashSet::new();

    for (index, link) in document.select(&link_selector).enumerate() {
        let Some(hit) = hit_from_elements(&link, snippets.get(index), hits.len() + 1) else {
            continue;
        };
        if !seen.insert(hit.url.clone()) {
            continue;
        }
        hits.push(hit);
        if hits.len() >= max_results {
            break;
        }
    }
    hits
}

fn hit_from_elements(
    link: &ElementRef<'_>,
    snippet: Option<&ElementRef<'_>>,
    rank: usize,
) -> Option<SearchHit> {
    let href = link.value().attr("href")?;
    let url = unwrap_ddg_redirect(href)?;
    let title = normalize_whitespace(&link.text().collect::<Vec<_>>().join(" "));
    if title.is_empty() {
        return None;
    }
    let snippet = snippet
        .map(|element| normalize_whitespace(&element.text().collect::<Vec<_>>().join(" ")))
        .unwrap_or_default();
    Some(SearchHit {
        rank,
        title,
        citation_id: citation_id("search", &url),
        url,
        snippet,
        backend: "duckduckgo".into(),
    })
}

fn unwrap_ddg_redirect(href: &str) -> Option<String> {
    let parsed = if href.starts_with("//") {
        Url::parse(&format!("https:{href}")).ok()?
    } else if href.starts_with('/') {
        Url::parse("https://duckduckgo.com").ok()?.join(href).ok()?
    } else {
        Url::parse(href).ok()?
    };

    let target = if parsed
        .host_str()
        .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
        && parsed.path().starts_with("/l/")
    {
        parsed
            .query_pairs()
            .find(|(key, _)| key == "uddg")
            .map(|(_, value)| value.into_owned())?
    } else {
        parsed.to_string()
    };

    let target = Url::parse(&target).ok()?;
    if !matches!(target.scheme(), "http" | "https") {
        return None;
    }
    Some(target.to_string())
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[test]
    fn maps_official_duckduckgo_safe_search_parameters() {
        let mut request = SearchRequest::new("rust");
        request.safe_search = SafeSearch::Strict;
        assert!(build_search_form(&request).contains(&("kp".into(), "1".into())));
        request.safe_search = SafeSearch::Moderate;
        assert!(build_search_form(&request).contains(&("kp".into(), "-1".into())));
        request.safe_search = SafeSearch::Off;
        assert!(build_search_form(&request).contains(&("kp".into(), "-2".into())));
    }

    #[test]
    fn parses_html_endpoint_fixture() {
        let html = include_bytes!("../../tests/fixtures/web/ddg_html.html");
        let hits = parse_html_results(html, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Rust Programming Language");
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
        assert_eq!(hits[0].rank, 1);
        assert_eq!(hits[1].url, "https://doc.rust-lang.org/book/");
    }

    #[test]
    fn parses_lite_endpoint_fixture() {
        let html = include_bytes!("../../tests/fixtures/web/ddg_lite.html");
        let hits = parse_lite_results(html, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Rust Programming Language");
        assert_eq!(hits[0].snippet, "A language empowering everyone.");
    }

    #[tokio::test]
    async fn falls_back_from_html_challenge_to_lite_results() {
        use crate::web_research::{
            DnsResolver, EgressPolicy, HttpTransport, PreparedGet, PreparedPostForm,
            RawHttpResponse,
        };
        use reqwest::{StatusCode, header::HeaderMap};
        use std::{collections::VecDeque, net::SocketAddr, sync::Mutex};

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

        struct Transport(Mutex<VecDeque<RawHttpResponse>>);
        #[async_trait]
        impl HttpTransport for Transport {
            async fn get(&self, _request: PreparedGet) -> Result<RawHttpResponse, WebError> {
                panic!("DuckDuckGo backend must use form POST");
            }
            async fn post_form(
                &self,
                _request: PreparedPostForm,
            ) -> Result<RawHttpResponse, WebError> {
                self.0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| WebError::new(WebErrorKind::Transport, "missing response"))
            }
        }

        let challenge = RawHttpResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: b"<html><div id='anomaly-modal'>verify</div></html>".to_vec(),
            body_truncated: false,
        };
        let lite = RawHttpResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: include_bytes!("../../tests/fixtures/web/ddg_lite.html").to_vec(),
            body_truncated: false,
        };
        let transport = Arc::new(Transport(Mutex::new(VecDeque::from([challenge, lite]))));
        let http = Arc::new(SecureHttpClient::new(
            transport,
            Arc::new(Dns),
            EgressPolicy::default(),
        ));
        let backend = DuckDuckGoSearchBackend::new(http);
        let response = backend.search(&SearchRequest::new("rust")).await.unwrap();
        assert_eq!(response.outcome, WebOutcome::Success);
        assert_eq!(response.hits.len(), 2);
        assert_eq!(response.attempts.len(), 2);
        assert_eq!(response.attempts[0].outcome, WebOutcome::Failed);
        assert_eq!(response.attempts[1].outcome, WebOutcome::Success);
    }

    #[test]
    fn unwraps_redirect_and_rejects_non_http_targets() {
        let wrapped = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs&rut=abc";
        assert_eq!(
            unwrap_ddg_redirect(wrapped).as_deref(),
            Some("https://example.com/docs")
        );
        assert!(unwrap_ddg_redirect("javascript:alert(1)").is_none());
    }
}
