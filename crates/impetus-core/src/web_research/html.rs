use std::collections::HashSet;

use regex::Regex;
use reqwest::Url;
use scraper::{Html, Selector};

use super::{FetchBodyKind, FetchLink, WebError, WebErrorKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedPage {
    pub kind: FetchBodyKind,
    pub title: Option<String>,
    pub text: String,
    pub links: Vec<FetchLink>,
    pub truncated: bool,
}

pub(crate) fn extract_body(
    content_type: Option<&str>,
    body: &[u8],
    base_url: &str,
    max_chars: usize,
    include_links: bool,
    allow_binary_metadata: bool,
) -> Result<ExtractedPage, WebError> {
    let normalized_type = content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if is_html_type(&normalized_type) || (normalized_type.is_empty() && looks_like_html(body)) {
        return extract_html(body, base_url, max_chars, include_links);
    }

    if is_text_type(&normalized_type) || (normalized_type.is_empty() && looks_like_text(body)) {
        let text = String::from_utf8_lossy(body);
        let normalized = normalize_whitespace(&text);
        let (text, truncated) = truncate_chars(&normalized, max_chars);
        return Ok(ExtractedPage {
            kind: FetchBodyKind::Text,
            title: None,
            text,
            links: Vec::new(),
            truncated,
        });
    }

    if allow_binary_metadata {
        return Ok(ExtractedPage {
            kind: FetchBodyKind::Binary,
            title: None,
            text: String::new(),
            links: Vec::new(),
            truncated: false,
        });
    }

    Err(WebError::new(
        WebErrorKind::UnsupportedContentType,
        format!(
            "binary/unsupported content type '{}' is not readable text",
            if normalized_type.is_empty() {
                "unknown"
            } else {
                &normalized_type
            }
        ),
    )
    .with_url(base_url))
}

fn extract_html(
    body: &[u8],
    base_url: &str,
    max_chars: usize,
    include_links: bool,
) -> Result<ExtractedPage, WebError> {
    let source = String::from_utf8_lossy(body);
    let cleaned = strip_non_content_blocks(&source);
    let document = Html::parse_document(&cleaned);

    let title_selector = Selector::parse("title").expect("static title selector is valid");
    let title = document
        .select(&title_selector)
        .next()
        .map(|element| normalize_whitespace(&element.text().collect::<Vec<_>>().join(" ")))
        .filter(|title| !title.is_empty());

    let content_selector =
        Selector::parse("main, article, body").expect("static readable-content selector is valid");
    let text = document
        .select(&content_selector)
        .next()
        .map(|element| element.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| document.root_element().text().collect::<Vec<_>>().join(" "));
    let normalized = normalize_whitespace(&text);
    let (text, truncated) = truncate_chars(&normalized, max_chars);

    let links = if include_links {
        extract_links(&document, base_url)?
    } else {
        Vec::new()
    };

    Ok(ExtractedPage {
        kind: FetchBodyKind::Html,
        title,
        text,
        links,
        truncated,
    })
}

fn strip_non_content_blocks(source: &str) -> String {
    let mut cleaned = source.to_string();
    if let Ok(comment) = Regex::new(r"(?s)<!--.*?-->") {
        cleaned = comment.replace_all(&cleaned, " ").into_owned();
    }
    for tag in [
        "script", "style", "noscript", "svg", "iframe", "template", "nav", "aside", "form",
        "select", "dialog", "canvas",
    ] {
        let pattern = format!(r"(?is)<{tag}\b[^>]*>.*?</{tag}\s*>");
        if let Ok(regex) = Regex::new(&pattern) {
            cleaned = regex.replace_all(&cleaned, " ").into_owned();
        }
    }
    cleaned
}

fn extract_links(document: &Html, base_url: &str) -> Result<Vec<FetchLink>, WebError> {
    let base = Url::parse(base_url).map_err(|error| {
        WebError::new(
            WebErrorKind::InvalidUrl,
            format!("cannot resolve page links against invalid base URL: {error}"),
        )
        .with_url(base_url)
    })?;
    let selector = Selector::parse("a[href]").expect("static link selector is valid");
    let mut seen = HashSet::new();
    let mut links = Vec::new();

    for element in document.select(&selector) {
        let Some(href) = element.value().attr("href") else {
            continue;
        };
        let Ok(url) = base.join(href) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") {
            continue;
        }
        let url = url.to_string();
        if !seen.insert(url.clone()) {
            continue;
        }
        let text = normalize_whitespace(&element.text().collect::<Vec<_>>().join(" "));
        links.push(FetchLink { text, url });
        if links.len() >= 256 {
            break;
        }
    }

    Ok(links)
}

pub(crate) fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate_chars(input: &str, max_chars: usize) -> (String, bool) {
    if max_chars == 0 {
        return (String::new(), !input.is_empty());
    }
    let mut chars = input.chars();
    let mut output = String::with_capacity(input.len().min(max_chars));
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return (output, false);
        };
        output.push(ch);
    }
    let truncated = chars.next().is_some();
    (output, truncated)
}

fn is_html_type(content_type: &str) -> bool {
    matches!(content_type, "text/html" | "application/xhtml+xml")
}

fn is_text_type(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || matches!(
            content_type,
            "application/json"
                | "application/ld+json"
                | "application/xml"
                | "application/rss+xml"
                | "application/atom+xml"
        )
}

fn looks_like_html(body: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(&body[..body.len().min(512)]).to_ascii_lowercase();
    prefix.contains("<!doctype html") || prefix.contains("<html") || prefix.contains("<body")
}

fn looks_like_text(body: &[u8]) -> bool {
    if body.is_empty() {
        return true;
    }
    let sample = &body[..body.len().min(1024)];
    let suspicious = sample
        .iter()
        .filter(|byte| **byte == 0 || (**byte < 0x09) || (**byte > 0x0d && **byte < 0x20))
        .count();
    suspicious * 20 < sample.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_text_and_absolute_links() {
        let html = br#"
            <html><head><title> Example Page </title></head>
            <body><main><h1>Hello</h1><p>Readable text.</p>
            <a href="/docs"> Docs </a><a href="mailto:a@example.com">Mail</a></main></body></html>
        "#;
        let page = extract_body(
            Some("text/html; charset=utf-8"),
            html,
            "https://example.com/start",
            1000,
            true,
            false,
        )
        .unwrap();
        assert_eq!(page.title.as_deref(), Some("Example Page"));
        assert!(page.text.contains("Hello Readable text."));
        assert_eq!(page.links.len(), 1);
        assert_eq!(page.links[0].url, "https://example.com/docs");
    }

    #[test]
    fn removes_script_style_and_navigation_text() {
        let html = br#"
            <html><head><title>Title</title><style>.x{}</style></head>
            <body><nav>Menu Noise</nav><main><p>Keep me</p><script>secret()</script></main></body></html>
        "#;
        let page = extract_body(
            Some("text/html"),
            html,
            "https://example.com/",
            1000,
            false,
            false,
        )
        .unwrap();
        assert_eq!(page.text, "Keep me");
    }

    #[test]
    fn truncation_is_character_deterministic() {
        let (text, truncated) = truncate_chars("абвгд", 3);
        assert_eq!(text, "абв");
        assert!(truncated);
        let (text, truncated) = truncate_chars("abc", 3);
        assert_eq!(text, "abc");
        assert!(!truncated);
    }

    #[test]
    fn binary_is_explicit() {
        let error = extract_body(
            Some("application/octet-stream"),
            &[0, 1, 2, 3],
            "https://example.com/file.bin",
            100,
            false,
            false,
        )
        .unwrap_err();
        assert_eq!(error.kind, WebErrorKind::UnsupportedContentType);

        let page = extract_body(
            Some("application/octet-stream"),
            &[0, 1, 2, 3],
            "https://example.com/file.bin",
            100,
            false,
            true,
        )
        .unwrap();
        assert_eq!(page.kind, FetchBodyKind::Binary);
    }
}
