use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
use reqwest::{Url, header::CONTENT_TYPE};
use serde::Deserialize;

use super::{
    SafeSearch, SearchAttempt, SearchHit, SearchRequest, SearchResponse, SecureHttpClient,
    WebError, WebErrorKind, WebOutcome,
    service::{SearchBackend, citation_id},
};

const SEARCH_BODY_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SearxngSearchBackend {
    id: String,
    base_url: Url,
    http: Arc<SecureHttpClient>,
}

impl SearxngSearchBackend {
    pub fn new(
        id: impl Into<String>,
        base_url: impl AsRef<str>,
        http: Arc<SecureHttpClient>,
    ) -> Result<Self, WebError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(WebError::new(
                WebErrorKind::Configuration,
                "SearXNG backend id must not be empty",
            ));
        }
        let mut base_url = Url::parse(base_url.as_ref()).map_err(|error| {
            WebError::new(
                WebErrorKind::Configuration,
                format!("invalid SearXNG base URL: {error}"),
            )
        })?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(WebError::new(
                WebErrorKind::UnsupportedScheme,
                "SearXNG base URL must use HTTP or HTTPS",
            )
            .with_url(base_url.to_string()));
        }
        base_url.set_query(None);
        base_url.set_fragment(None);
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        Ok(Self { id, base_url, http })
    }

    fn search_url(&self, request: &SearchRequest) -> Result<Url, WebError> {
        let mut url = self.base_url.join("search").map_err(|error| {
            WebError::new(
                WebErrorKind::Configuration,
                format!("failed to build SearXNG search URL: {error}"),
            )
        })?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("q", request.query.trim());
            pairs.append_pair("format", "json");
            if let Some(locale) = request
                .locale
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                pairs.append_pair("language", locale);
            }
            let safe = match request.safe_search {
                SafeSearch::Off => "0",
                SafeSearch::Moderate => "1",
                SafeSearch::Strict => "2",
            };
            pairs.append_pair("safesearch", safe);
        }
        Ok(url)
    }
}

#[derive(Debug, Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Debug, Deserialize)]
struct SearxngResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: Option<String>,
}

#[async_trait]
impl SearchBackend for SearxngSearchBackend {
    fn id(&self) -> &str {
        &self.id
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, WebError> {
        let started = Instant::now();
        let url = self.search_url(request)?;
        let response = self.http.get(url.as_str(), SEARCH_BODY_LIMIT).await?;
        if !response.status.is_success() {
            return Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                format!("SearXNG returned HTTP {}", response.status.as_u16()),
            )
            .with_url(response.final_url));
        }
        let content_type = response
            .headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type.is_empty() && !content_type.to_ascii_lowercase().contains("json") {
            return Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                "SearXNG JSON format is unavailable or disabled on this instance",
            )
            .with_url(response.final_url));
        }
        let parsed: SearxngResponse = serde_json::from_slice(&response.body).map_err(|error| {
            WebError::new(
                WebErrorKind::BackendUnavailable,
                format!("SearXNG returned invalid JSON: {error}"),
            )
            .with_url(response.final_url.clone())
        })?;
        let hits = map_results(parsed, request.normalized_limit(), self.id());
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
                endpoint: response.final_url,
                outcome,
                detail: None,
            }],
            elapsed_ms: elapsed_ms(started),
        })
    }
}

fn map_results(response: SearxngResponse, max_results: usize, backend: &str) -> Vec<SearchHit> {
    response
        .results
        .into_iter()
        .filter_map(|result| {
            let parsed = Url::parse(result.url.trim()).ok()?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return None;
            }
            let url = parsed.to_string();
            let title = if result.title.trim().is_empty() {
                url.clone()
            } else {
                result.title.trim().to_string()
            };
            Some((title, url, result.content.unwrap_or_default()))
        })
        .take(max_results)
        .enumerate()
        .map(|(index, (title, url, snippet))| SearchHit {
            rank: index + 1,
            title,
            citation_id: citation_id("search", &url),
            url,
            snippet: snippet.trim().to_string(),
            backend: backend.to_string(),
        })
        .collect()
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_json_results_and_drops_non_http_urls() {
        let response: SearxngResponse = serde_json::from_str(
            r#"{
              "results": [
                {"title":"A","url":"https://example.com/a","content":"alpha"},
                {"title":"bad","url":"javascript:alert(1)","content":"bad"},
                {"title":"","url":"https://example.org/b"}
              ]
            }"#,
        )
        .unwrap();
        let hits = map_results(response, 10, "searxng:test");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].backend, "searxng:test");
        assert_eq!(hits[1].title, "https://example.org/b");
    }
}
