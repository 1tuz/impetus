use std::fmt::Write as _;

use regex::Regex;
use reqwest::Url;

use crate::{DurableArtifactStore, ToolCall, ToolEventOutcome, ToolObservation};

use super::{
    FetchRequest, FetchedPage, SearchRequest, SearchResponse, WebError, WebObservation, WebOutcome,
    WebResearchService,
};

const PREVIEW_CHAR_LIMIT: usize = 24_000;

/// Execute semantic web tools without exposing provider details to Agent Loop.
/// Returns `None` for non-web tools so ToolOrchestrator can keep its existing path.
pub async fn execute_web_tool(
    service: &dyn WebResearchService,
    tool_call: &ToolCall,
    artifacts: &DurableArtifactStore,
) -> Option<ToolObservation> {
    match tool_call.name.as_str() {
        "web_search" => Some(execute_search(service, tool_call, artifacts).await),
        "web_fetch" => Some(execute_fetch(service, tool_call, artifacts).await),
        _ => None,
    }
}

async fn execute_search(
    service: &dyn WebResearchService,
    tool_call: &ToolCall,
    artifacts: &DurableArtifactStore,
) -> ToolObservation {
    let request: SearchRequest = match serde_json::from_value(tool_call.arguments.clone()) {
        Ok(request) => request,
        Err(error) => {
            return failed_observation(tool_call, format!("invalid web_search arguments: {error}"));
        }
    };
    let arguments_summary = format!(
        "query={} chars, max_results={}",
        request.query.chars().count(),
        request.normalized_limit()
    );

    match service.search(request).await {
        Ok(response) => observation_from_search(tool_call, arguments_summary, response, artifacts),
        Err(error) => observation_from_error(tool_call, arguments_summary, error),
    }
}

async fn execute_fetch(
    service: &dyn WebResearchService,
    tool_call: &ToolCall,
    artifacts: &DurableArtifactStore,
) -> ToolObservation {
    let request: FetchRequest = match serde_json::from_value(tool_call.arguments.clone()) {
        Ok(request) => request,
        Err(error) => {
            return failed_observation(tool_call, format!("invalid web_fetch arguments: {error}"));
        }
    };
    let arguments_summary = format!("url={}", redact_url_for_observation(&request.url));

    match service.fetch(request).await {
        Ok(page) => observation_from_fetch(tool_call, arguments_summary, page, artifacts),
        Err(error) => observation_from_error(tool_call, arguments_summary, error),
    }
}

fn observation_from_search(
    tool_call: &ToolCall,
    arguments_summary: String,
    mut response: SearchResponse,
    artifacts: &DurableArtifactStore,
) -> ToolObservation {
    for hit in &mut response.hits {
        hit.url = redact_url_for_observation(&hit.url);
    }
    let query_chars = response.query.chars().count();
    response.query = format!("<redacted:{query_chars} chars>");
    let artifact = persist_json(artifacts, &WebObservation::Search(response.clone()));
    let mut preview = String::new();
    let _ = writeln!(
        &mut preview,
        "web_search: {:?} via {} ({} ms)",
        response.outcome, response.backend, response.elapsed_ms
    );
    for hit in &response.hits {
        let _ = writeln!(
            &mut preview,
            "\n[{}] {}\n{}\n{}",
            hit.citation_id, hit.title, hit.url, hit.snippet
        );
    }
    preview = truncate_preview(preview);

    ToolObservation {
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        arguments_summary,
        outcome: map_outcome(response.outcome),
        preview,
        artifact,
        error: if response.outcome == WebOutcome::Failed {
            response
                .attempts
                .last()
                .and_then(|attempt| attempt.detail.as_deref())
                .map(|detail| sanitize_urls_in_message(detail, true))
        } else {
            None
        },
    }
}

fn observation_from_fetch(
    tool_call: &ToolCall,
    arguments_summary: String,
    mut page: FetchedPage,
    artifacts: &DurableArtifactStore,
) -> ToolObservation {
    // Query-string credentials/signatures should not be copied into durable metadata.
    page.requested_url = redact_url_for_observation(&page.requested_url);
    page.final_url = redact_url_for_observation(&page.final_url);
    page.redirect_chain = page
        .redirect_chain
        .iter()
        .map(|url| redact_url_for_observation(url))
        .collect();
    page.citation.url = redact_url_for_observation(&page.citation.url);
    for link in &mut page.links {
        link.url = redact_url_for_observation(&link.url);
    }

    let artifact = persist_json(artifacts, &WebObservation::Fetch(page.clone()));
    let mut preview = String::new();
    let _ = writeln!(
        &mut preview,
        "web_fetch: {} [{}] ({} ms)",
        page.final_url, page.citation.citation_id, page.elapsed_ms
    );
    if let Some(title) = &page.title {
        let _ = writeln!(&mut preview, "title: {title}");
    }
    let _ = writeln!(&mut preview, "\n{}", page.text);
    preview = truncate_preview(preview);

    ToolObservation {
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        arguments_summary,
        outcome: map_outcome(page.outcome),
        preview,
        artifact,
        error: None,
    }
}

fn observation_from_error(
    tool_call: &ToolCall,
    arguments_summary: String,
    mut error: WebError,
) -> ToolObservation {
    if let Some(url) = error.url.take() {
        error.url = Some(if tool_call.name == "web_search" {
            redact_search_url(&url)
        } else {
            redact_url_for_observation(&url)
        });
    }
    let sanitized_message =
        sanitize_urls_in_message(&error.message, tool_call.name == "web_search");
    error.message = sanitized_message;
    ToolObservation {
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        arguments_summary,
        outcome: if error.blocked() {
            ToolEventOutcome::Denied
        } else {
            ToolEventOutcome::Error
        },
        preview: String::new(),
        artifact: None,
        error: Some(error.to_string()),
    }
}

fn failed_observation(tool_call: &ToolCall, error: String) -> ToolObservation {
    ToolObservation {
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        arguments_summary: "invalid web tool arguments".into(),
        outcome: ToolEventOutcome::Error,
        preview: String::new(),
        artifact: None,
        error: Some(error),
    }
}

fn persist_json<T: serde::Serialize>(
    artifacts: &DurableArtifactStore,
    value: &T,
) -> Option<crate::DurableArtifactRef> {
    serde_json::to_vec(value)
        .ok()
        .and_then(|bytes| artifacts.store(&bytes).ok())
}

fn map_outcome(outcome: WebOutcome) -> ToolEventOutcome {
    match outcome {
        WebOutcome::Success | WebOutcome::NoResults => ToolEventOutcome::Success,
        WebOutcome::Blocked => ToolEventOutcome::Denied,
        WebOutcome::Failed => ToolEventOutcome::Error,
    }
}

fn truncate_preview(input: String) -> String {
    let mut chars = input.chars();
    let mut output = String::with_capacity(input.len().min(PREVIEW_CHAR_LIMIT));
    for _ in 0..PREVIEW_CHAR_LIMIT {
        let Some(ch) = chars.next() else {
            return output;
        };
        output.push(ch);
    }
    if chars.next().is_some() {
        output.push_str("\n...[preview truncated; full result is stored as an artifact]");
    }
    output
}

fn redact_search_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return "<invalid-url>".into();
    };
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn sanitize_urls_in_message(message: &str, strip_all_query: bool) -> String {
    let Ok(regex) = Regex::new(r"https?://[^\s)\]}>]+") else {
        return message.to_string();
    };
    regex
        .replace_all(message, |captures: &regex::Captures<'_>| {
            if strip_all_query {
                redact_search_url(&captures[0])
            } else {
                redact_url_for_observation(&captures[0])
            }
        })
        .into_owned()
}

pub fn redact_url_for_observation(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return "<invalid-url>".into();
    };
    if url.query().is_none() {
        return url.to_string();
    }

    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| {
            let normalized = key.to_ascii_lowercase().replace('-', "_");
            let sensitive = [
                "token",
                "access_token",
                "api_key",
                "apikey",
                "key",
                "secret",
                "signature",
                "sig",
                "auth",
                "authorization",
                "password",
                "passwd",
                "x_amz_signature",
                "x_amz_credential",
            ]
            .iter()
            .any(|candidate| normalized == *candidate || normalized.ends_with(candidate));
            (
                key.into_owned(),
                if sensitive {
                    "<redacted>".into()
                } else {
                    value.into_owned()
                },
            )
        })
        .collect();

    url.set_query(None);
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(&key, &value);
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::web_research::{SearchAttempt, WebErrorKind, WebFetchService, WebSearchService};

    #[test]
    fn redacts_sensitive_query_values_but_keeps_navigation_parameters() {
        let redacted = redact_url_for_observation(
            "https://example.com/file?page=2&token=abc&X-Amz-Signature=deadbeef",
        );
        assert!(redacted.contains("page=2"));
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("deadbeef"));
        assert!(redacted.contains("%3Credacted%3E"));
    }

    struct SearchOnlyService;

    #[async_trait]
    impl WebSearchService for SearchOnlyService {
        async fn search(&self, request: SearchRequest) -> Result<SearchResponse, WebError> {
            Ok(SearchResponse {
                outcome: WebOutcome::NoResults,
                backend: "mock".into(),
                query: request.query,
                hits: Vec::new(),
                attempts: vec![SearchAttempt {
                    backend: "mock".into(),
                    endpoint: "mock".into(),
                    outcome: WebOutcome::NoResults,
                    detail: None,
                }],
                elapsed_ms: 1,
            })
        }
    }

    #[async_trait]
    impl WebFetchService for SearchOnlyService {
        async fn fetch(&self, _request: FetchRequest) -> Result<FetchedPage, WebError> {
            Err(WebError::new(
                WebErrorKind::BackendUnavailable,
                "fetch is not implemented in this mock",
            ))
        }
    }

    #[tokio::test]
    async fn durable_search_artifact_redacts_the_raw_query_field() {
        let temp = tempfile::tempdir().unwrap();
        let artifacts = DurableArtifactStore::open(temp.path()).unwrap();
        let call = ToolCall {
            id: "tool-1".into(),
            name: "web_search".into(),
            arguments: serde_json::json!({"query": "secret-query-value"}),
        };

        let observation = execute_web_tool(&SearchOnlyService, &call, &artifacts)
            .await
            .expect("web observation");
        let artifact = observation.artifact.expect("structured result artifact");
        let bytes = artifacts.read(&artifact.id).unwrap();
        let stored: WebObservation = serde_json::from_slice(&bytes).unwrap();
        let WebObservation::Search(response) = stored else {
            panic!("expected search observation");
        };
        assert_eq!(response.query, "<redacted:18 chars>");
    }
}
