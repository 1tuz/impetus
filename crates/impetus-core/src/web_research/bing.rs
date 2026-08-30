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

const BING_ENDPOINT: &str = "https://www.bing.com/search";
const SEARCH_BODY_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct BingHtmlSearchBackend {
    http: Arc<SecureHttpClient>,
}

impl BingHtmlSearchBackend {
    pub fn new(http: Arc<SecureHttpClient>) -> Self {
        Self { http }
    }

    fn request_url(&self, request: &SearchRequest) -> Result<Url, WebError> {
        let mut url = Url::parse(BING_ENDPOINT).map_err(|error| {
            WebError::new(
                WebErrorKind::Configuration,
                format!("invalid Bing HTML endpoint: {error}"),
            )
        })?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("q", request.query.trim());
            if let Some(locale) = request
                .locale
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                pairs.append_pair("mkt", locale);
            }
            let safe = match request.safe_search {
                SafeSearch::Off => "off",
                SafeSearch::Moderate => "moderate",
                SafeSearch::Strict => "strict",
            };
            pairs.append_pair("adlt", safe);
        }
        Ok(url)
    }
}

#[async_trait]
impl SearchBackend for BingHtmlSearchBackend {
    fn id(&self) -> &str {
        "bing_html"
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, WebError> {
        let started = Instant::now();
        let url = self.request_url(request)?;
        let response = self.http.get(url.as_str(), SEARCH_BODY_LIMIT).await?;
        if !response.status.is_success() {
            return Err(WebError::new(
                WebErrorKind::HttpStatus,
                format!("Bing HTML returned HTTP {}", response.status.as_u16()),
            )
            .with_url(response.final_url));
        }

        let hits = parse_bing_results(&response.body, request.normalized_limit());
        if hits.is_empty() && detect_anti_bot_page(&response.body) {
            return Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                "Bing returned an anti-bot/challenge page instead of search results",
            )
            .with_url(response.final_url));
        }
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
                endpoint: BING_ENDPOINT.into(),
                outcome,
                detail: None,
            }],
            elapsed_ms: elapsed_ms(started),
        })
    }
}

fn parse_bing_results(body: &[u8], max_results: usize) -> Vec<SearchHit> {
    let source = String::from_utf8_lossy(body);
    let document = Html::parse_document(&source);
    let block_selector = Selector::parse("li.b_algo").expect("static selector is valid");
    let link_selector = Selector::parse("h2 a").expect("static selector is valid");
    let snippet_selector = Selector::parse(".b_caption p").expect("static selector is valid");
    let mut hits = Vec::new();
    let mut seen = HashSet::new();

    for block in document.select(&block_selector) {
        let Some(link) = block.select(&link_selector).next() else {
            continue;
        };
        let Some(url) = clean_result_url(&link) else {
            continue;
        };
        if !seen.insert(url.clone()) {
            continue;
        }
        let title = normalize_whitespace(&link.text().collect::<Vec<_>>().join(" "));
        if title.is_empty() {
            continue;
        }
        let snippet = block
            .select(&snippet_selector)
            .next()
            .map(|element| normalize_whitespace(&element.text().collect::<Vec<_>>().join(" ")))
            .unwrap_or_default();
        hits.push(SearchHit {
            rank: hits.len() + 1,
            title,
            citation_id: citation_id("search", &url),
            url,
            snippet,
            backend: "bing_html".into(),
        });
        if hits.len() >= max_results {
            break;
        }
    }
    hits
}

fn clean_result_url(link: &ElementRef<'_>) -> Option<String> {
    let href = link.value().attr("href")?;
    let url = Url::parse(href).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if url
        .host_str()
        .is_some_and(|host| host == "bing.com" || host.ends_with(".bing.com"))
    {
        return None;
    }
    Some(url.to_string())
}

fn detect_anti_bot_page(body: &[u8]) -> bool {
    let source = String::from_utf8_lossy(body).to_ascii_lowercase();
    [
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

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bing_html_results() {
        let html = br#"
            <ol id="b_results">
              <li class="b_algo">
                <h2><a href="https://example.com/a">First result</a></h2>
                <div class="b_caption"><p>Useful snippet.</p></div>
              </li>
              <li class="b_algo">
                <h2><a href="https://example.org/b">Second result</a></h2>
              </li>
            </ol>
        "#;
        let hits = parse_bing_results(html, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "First result");
        assert_eq!(hits[0].snippet, "Useful snippet.");
        assert_eq!(hits[1].rank, 2);
    }

    #[test]
    fn ignores_bing_navigation_links() {
        let html = br#"
            <li class="b_algo">
              <h2><a href="https://www.bing.com/search?q=other">Navigation</a></h2>
            </li>
        "#;
        assert!(parse_bing_results(html, 10).is_empty());
    }
}
