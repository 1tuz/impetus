//! Bounded multi-step research loop: search → select → fetch → follow links.

use std::collections::HashSet;

use super::{
    CitationSource, FetchRequest, SearchRequest, WebError, WebErrorKind, WebFetchService,
    WebSearchService,
};

/// Configuration for bounded research.
#[derive(Debug, Clone)]
pub struct ResearchConfig {
    /// Maximum search depth (0 = search only, 1 = search + fetch, 2 = search + fetch + follow).
    pub max_depth: usize,
    /// Maximum total sources to fetch.
    pub max_sources: usize,
    /// Maximum links to follow per fetched page.
    pub max_links_per_page: usize,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_sources: 10,
            max_links_per_page: 3,
        }
    }
}

/// Result of a bounded research task.
#[derive(Debug, Clone)]
pub struct ResearchResult {
    /// Original search query.
    pub query: String,
    /// All citations collected (deduplicated).
    pub citations: Vec<CitationSource>,
    /// URLs visited (deduplicated).
    pub visited_urls: Vec<String>,
    /// Total fetch count.
    pub fetch_count: usize,
    /// Total elapsed time across all operations (milliseconds).
    pub total_elapsed_ms: u64,
}

/// Execute bounded multi-step research.
///
/// Steps:
/// 1. Search for query → get search hits
/// 2. If depth >= 1: fetch top N results
/// 3. If depth >= 2: follow selected links from fetched pages
///
/// Deduplicates URLs across all steps.
pub async fn research<S>(
    service: &S,
    query: String,
    config: ResearchConfig,
) -> Result<ResearchResult, WebError>
where
    S: WebSearchService + WebFetchService + ?Sized,
{
    if config.max_depth == 0 && config.max_sources == 0 {
        return Err(WebError::new(
            WebErrorKind::InvalidRequest,
            "research config must allow at least search (max_depth=0) or fetch (max_sources>0)",
        ));
    }

    let started = std::time::Instant::now();
    let mut visited = HashSet::new();
    let mut citations = Vec::new();
    let mut fetch_count = 0;
    let mut total_elapsed_ms = 0u64;

    // Step 1: Search
    let search_req = SearchRequest::new(query.clone());
    let search_resp = service.search(search_req).await?;
    total_elapsed_ms += search_resp.elapsed_ms;

    for hit in &search_resp.hits {
        if !visited.insert(normalize_url(&hit.url)) {
            continue;
        }
        citations.push(CitationSource {
            citation_id: hit.citation_id.clone(),
            url: hit.url.clone(),
            title: Some(hit.title.clone()),
            artifact: None,
        });
    }

    // Step 2: Fetch top results (depth >= 1)
    if config.max_depth >= 1 && config.max_sources > 0 {
        let fetch_limit = config.max_sources.min(search_resp.hits.len());
        for hit in search_resp.hits.iter().take(fetch_limit) {
            if fetch_count >= config.max_sources {
                break;
            }

            let fetch_req = FetchRequest::new(hit.url.clone());
            match service.fetch(fetch_req).await {
                Ok(page) => {
                    total_elapsed_ms += page.elapsed_ms;
                    fetch_count += 1;

                    // Update citation with artifact if persisted
                    if let Some(existing) = citations.iter_mut().find(|c| c.url == page.final_url) {
                        existing.artifact = page.citation.artifact.clone();
                        existing.title = page.citation.title.clone();
                    }

                    // Step 3: Follow links (depth >= 2)
                    if config.max_depth >= 2 && fetch_count < config.max_sources {
                        let links_to_follow = page
                            .links
                            .iter()
                            .filter(|link| {
                                !link.url.is_empty() && visited.insert(normalize_url(&link.url))
                            })
                            .take(config.max_links_per_page)
                            .collect::<Vec<_>>();

                        for link in links_to_follow {
                            if fetch_count >= config.max_sources {
                                break;
                            }

                            let follow_req = FetchRequest::new(link.url.clone());
                            match service.fetch(follow_req).await {
                                Ok(followed) => {
                                    total_elapsed_ms += followed.elapsed_ms;
                                    fetch_count += 1;
                                    citations.push(followed.citation.clone());
                                }
                                Err(_) => {
                                    // Silently skip failed follow links — they are not critical
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // Silently skip failed fetches — degraded results OK
                }
            }
        }
    }

    Ok(ResearchResult {
        query,
        citations,
        visited_urls: visited.into_iter().collect(),
        fetch_count,
        total_elapsed_ms: total_elapsed_ms
            .max(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
    })
}

/// Normalize URL for deduplication (strip fragment, lowercase scheme/host).
fn normalize_url(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        let mut normalized = parsed.clone();
        normalized.set_fragment(None);
        normalized.to_string().to_lowercase()
    } else {
        url.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_url() {
        assert_eq!(
            normalize_url("https://Example.com/Path#fragment"),
            "https://example.com/path"
        );
        assert_eq!(normalize_url("HTTP://EXAMPLE.COM/"), "http://example.com/");
    }

    #[test]
    fn test_config_default() {
        let cfg = ResearchConfig::default();
        assert_eq!(cfg.max_depth, 2);
        assert_eq!(cfg.max_sources, 10);
    }
}
