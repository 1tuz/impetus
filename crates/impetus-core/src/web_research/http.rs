use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{
    Response, StatusCode, Url,
    header::{ACCEPT, HeaderMap, LOCATION},
    redirect::Policy as RedirectPolicy,
};
use tokio::net::lookup_host;

use super::{EgressPolicy, WebError, WebErrorKind};

const DEFAULT_MAX_REDIRECTS: usize = 5;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[async_trait]
pub trait DnsResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, WebError>;
}

#[derive(Debug, Default)]
pub struct SystemDnsResolver;

#[async_trait]
impl DnsResolver for SystemDnsResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, WebError> {
        let mut addresses: Vec<_> = lookup_host((host, port))
            .await
            .map_err(|error| {
                WebError::new(
                    WebErrorKind::DnsResolutionFailed,
                    format!("DNS resolution failed for {host}: {error}"),
                )
            })?
            .collect();
        addresses.sort_by_key(|address| address.to_string());
        addresses.dedup();
        if addresses.is_empty() {
            return Err(WebError::new(
                WebErrorKind::DnsResolutionFailed,
                format!("DNS resolution returned no addresses for {host}"),
            ));
        }
        Ok(addresses)
    }
}

#[derive(Debug, Clone)]
pub struct PreparedGet {
    pub url: Url,
    pub resolved_addr: SocketAddr,
    pub max_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct PreparedPostForm {
    pub url: Url,
    pub resolved_addr: SocketAddr,
    pub max_bytes: usize,
    pub form: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct RawHttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub body_truncated: bool,
}

#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn get(&self, request: PreparedGet) -> Result<RawHttpResponse, WebError>;

    async fn post_form(&self, request: PreparedPostForm) -> Result<RawHttpResponse, WebError> {
        Err(WebError::new(
            WebErrorKind::Configuration,
            "HTTP transport does not implement form POST",
        )
        .with_url(request.url.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    connect_timeout: Duration,
    request_timeout: Duration,
    user_agent: String,
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            user_agent: DEFAULT_USER_AGENT.to_string(),
        }
    }
}

impl ReqwestTransport {
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    fn client_for(
        &self,
        url: &Url,
        resolved_addr: SocketAddr,
    ) -> Result<reqwest::Client, WebError> {
        let host = url.host_str().ok_or_else(|| {
            WebError::new(
                WebErrorKind::InvalidUrl,
                "request URL does not contain a host",
            )
            .with_url(url.to_string())
        })?;
        let mut builder = reqwest::Client::builder()
            .redirect(RedirectPolicy::none())
            .connect_timeout(self.connect_timeout)
            .timeout(self.request_timeout)
            .user_agent(&self.user_agent)
            .no_proxy();
        if host.parse::<IpAddr>().is_err() {
            builder = builder.resolve(host, resolved_addr);
        }
        builder.build().map_err(|error| {
            WebError::new(
                WebErrorKind::Transport,
                format!("failed to build HTTP client: {error}"),
            )
            .with_url(url.to_string())
        })
    }

    async fn finish_response(
        &self,
        response: Response,
        max_bytes: usize,
        url: &Url,
    ) -> Result<RawHttpResponse, WebError> {
        let status = response.status();
        let headers = response.headers().clone();
        if is_redirect_status(status) {
            return Ok(RawHttpResponse {
                status,
                headers,
                body: Vec::new(),
                body_truncated: false,
            });
        }
        let content_length_exceeds_limit = response
            .content_length()
            .is_some_and(|content_length| content_length > max_bytes as u64);
        let mut body = Vec::with_capacity(max_bytes.min(64 * 1024));
        let mut body_truncated = false;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                WebError::new(
                    WebErrorKind::Transport,
                    format!("failed while reading response body: {error}"),
                )
                .with_url(url.to_string())
            })?;
            let remaining = max_bytes.saturating_sub(body.len());
            if chunk.len() > remaining {
                body.extend_from_slice(&chunk[..remaining]);
                body_truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
            if body.len() == max_bytes {
                body_truncated = content_length_exceeds_limit;
                if body_truncated {
                    break;
                }
            }
        }
        Ok(RawHttpResponse {
            status,
            headers,
            body,
            body_truncated,
        })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn get(&self, request: PreparedGet) -> Result<RawHttpResponse, WebError> {
        let client = self.client_for(&request.url, request.resolved_addr)?;
        let response = client
            .get(request.url.clone())
            .header(
                ACCEPT,
                "text/html,application/xhtml+xml,application/json,text/plain;q=0.9,*/*;q=0.8",
            )
            .send()
            .await
            .map_err(|error| {
                WebError::new(WebErrorKind::Transport, format!("HTTP GET failed: {error}"))
                    .with_url(request.url.to_string())
            })?;
        self.finish_response(response, request.max_bytes, &request.url)
            .await
    }

    async fn post_form(&self, request: PreparedPostForm) -> Result<RawHttpResponse, WebError> {
        let client = self.client_for(&request.url, request.resolved_addr)?;
        let response = client
            .post(request.url.clone())
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .form(&request.form)
            .send()
            .await
            .map_err(|error| {
                WebError::new(
                    WebErrorKind::Transport,
                    format!("HTTP form POST failed: {error}"),
                )
                .with_url(request.url.to_string())
            })?;
        self.finish_response(response, request.max_bytes, &request.url)
            .await
    }
}

#[derive(Debug, Clone)]
pub struct SecureHttpResponse {
    pub requested_url: String,
    pub final_url: String,
    pub redirect_chain: Vec<String>,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub body_truncated: bool,
}

#[derive(Clone)]
pub struct SecureHttpClient {
    transport: Arc<dyn HttpTransport>,
    resolver: Arc<dyn DnsResolver>,
    egress: EgressPolicy,
    max_redirects: usize,
}

impl std::fmt::Debug for SecureHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecureHttpClient")
            .field("egress", &self.egress)
            .field("max_redirects", &self.max_redirects)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
enum RequestMode {
    Get,
    PostForm(Vec<(String, String)>),
}

impl SecureHttpClient {
    pub fn production(egress: EgressPolicy) -> Self {
        Self::new(
            Arc::new(ReqwestTransport::default()),
            Arc::new(SystemDnsResolver),
            egress,
        )
    }

    pub fn new(
        transport: Arc<dyn HttpTransport>,
        resolver: Arc<dyn DnsResolver>,
        egress: EgressPolicy,
    ) -> Self {
        Self {
            transport,
            resolver,
            egress,
            max_redirects: DEFAULT_MAX_REDIRECTS,
        }
    }

    pub fn with_max_redirects(mut self, max_redirects: usize) -> Self {
        self.max_redirects = max_redirects;
        self
    }

    pub fn egress_policy(&self) -> &EgressPolicy {
        &self.egress
    }

    pub async fn get(
        &self,
        raw_url: &str,
        max_bytes: usize,
    ) -> Result<SecureHttpResponse, WebError> {
        self.execute(raw_url, max_bytes, RequestMode::Get).await
    }

    /// Form POST exists only for fixed semantic providers such as DuckDuckGo HTML/Lite.
    /// `web_fetch` itself remains GET-only; callers must not expose arbitrary POST as an agent tool.
    pub async fn post_form(
        &self,
        raw_url: &str,
        form: Vec<(String, String)>,
        max_bytes: usize,
    ) -> Result<SecureHttpResponse, WebError> {
        self.execute(raw_url, max_bytes, RequestMode::PostForm(form))
            .await
    }

    async fn execute(
        &self,
        raw_url: &str,
        max_bytes: usize,
        mut mode: RequestMode,
    ) -> Result<SecureHttpResponse, WebError> {
        if max_bytes == 0 {
            return Err(WebError::new(
                WebErrorKind::InvalidRequest,
                "max_bytes must be greater than zero",
            )
            .with_url(raw_url));
        }
        let requested_url = raw_url.to_string();
        let mut current = self.egress.validate_url(raw_url)?;
        let mut redirect_chain = Vec::new();

        for hop in 0..=self.max_redirects {
            let resolved_addr = self.resolve_and_validate(&current).await?;
            let response = match &mode {
                RequestMode::Get => {
                    self.transport
                        .get(PreparedGet {
                            url: current.clone(),
                            resolved_addr,
                            max_bytes,
                        })
                        .await?
                }
                RequestMode::PostForm(form) => {
                    self.transport
                        .post_form(PreparedPostForm {
                            url: current.clone(),
                            resolved_addr,
                            max_bytes,
                            form: form.clone(),
                        })
                        .await?
                }
            };

            if !is_redirect_status(response.status) {
                return Ok(SecureHttpResponse {
                    requested_url,
                    final_url: current.to_string(),
                    redirect_chain,
                    status: response.status,
                    headers: response.headers,
                    body: response.body,
                    body_truncated: response.body_truncated,
                });
            }
            if hop == self.max_redirects {
                return Err(WebError::new(
                    WebErrorKind::RedirectLimit,
                    format!("redirect limit ({}) exceeded", self.max_redirects),
                )
                .with_url(current.to_string()));
            }

            let location = response
                .headers
                .get(LOCATION)
                .ok_or_else(|| {
                    WebError::new(
                        WebErrorKind::RedirectMissingLocation,
                        "redirect response does not contain a Location header",
                    )
                    .with_url(current.to_string())
                })?
                .to_str()
                .map_err(|_| {
                    WebError::new(
                        WebErrorKind::RedirectMissingLocation,
                        "redirect Location header is not valid UTF-8",
                    )
                    .with_url(current.to_string())
                })?;
            let next = current.join(location).map_err(|error| {
                WebError::new(
                    WebErrorKind::InvalidUrl,
                    format!("invalid redirect target: {error}"),
                )
                .with_url(current.to_string())
            })?;
            let next = self.egress.validate_url(next.as_str())?;
            redirect_chain.push(next.to_string());

            // Browser-compatible redirect semantics: 303 always becomes GET; 301/302 from
            // a form POST become GET; 307/308 preserve method and body.
            if matches!(&mode, RequestMode::PostForm(_))
                && matches!(
                    response.status,
                    StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER
                )
            {
                mode = RequestMode::Get;
            }
            current = next;
        }
        unreachable!("redirect loop exits by response or error")
    }

    async fn resolve_and_validate(&self, url: &Url) -> Result<SocketAddr, WebError> {
        let host = url.host_str().ok_or_else(|| {
            WebError::new(WebErrorKind::InvalidUrl, "URL does not contain a host")
                .with_url(url.to_string())
        })?;
        let port = url.port_or_known_default().ok_or_else(|| {
            WebError::new(WebErrorKind::PortBlocked, "URL has no usable port")
                .with_url(url.to_string())
        })?;
        let mut addresses = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, port)]
        } else {
            self.resolver.resolve(host, port).await.map_err(|error| {
                if error.url.is_none() {
                    error.with_url(url.to_string())
                } else {
                    error
                }
            })?
        };
        addresses.sort_by_key(|address| address.to_string());
        addresses.dedup();
        if addresses.is_empty() {
            return Err(WebError::new(
                WebErrorKind::DnsResolutionFailed,
                format!("DNS resolution returned no addresses for {host}"),
            )
            .with_url(url.to_string()));
        }
        for address in &addresses {
            self.egress.enforce_address(address.ip(), url.as_str())?;
        }
        Ok(addresses[0])
    }
}

fn is_redirect_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use std::{
        collections::{HashMap, VecDeque},
        sync::Mutex,
    };

    use reqwest::header::HeaderValue;

    use super::*;

    #[derive(Default)]
    struct MockDns {
        answers: HashMap<String, Vec<SocketAddr>>,
    }

    #[async_trait]
    impl DnsResolver for MockDns {
        async fn resolve(&self, host: &str, _port: u16) -> Result<Vec<SocketAddr>, WebError> {
            self.answers.get(host).cloned().ok_or_else(|| {
                WebError::new(WebErrorKind::DnsResolutionFailed, "missing mock DNS answer")
            })
        }
    }

    #[derive(Default)]
    struct MockTransport {
        responses: Mutex<VecDeque<RawHttpResponse>>,
        requests: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl HttpTransport for MockTransport {
        async fn get(&self, request: PreparedGet) -> Result<RawHttpResponse, WebError> {
            self.requests
                .lock()
                .unwrap()
                .push(format!("GET {}", request.url));
            self.next_response()
        }

        async fn post_form(&self, request: PreparedPostForm) -> Result<RawHttpResponse, WebError> {
            self.requests
                .lock()
                .unwrap()
                .push(format!("POST {}", request.url));
            self.next_response()
        }
    }

    impl MockTransport {
        fn next_response(&self) -> Result<RawHttpResponse, WebError> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| WebError::new(WebErrorKind::Transport, "missing mock response"))
        }
    }

    fn response(status: StatusCode, location: Option<&str>, body: &[u8]) -> RawHttpResponse {
        let mut headers = HeaderMap::new();
        if let Some(location) = location {
            headers.insert(LOCATION, HeaderValue::from_str(location).unwrap());
        }
        RawHttpResponse {
            status,
            headers,
            body: body.to_vec(),
            body_truncated: false,
        }
    }

    #[tokio::test]
    async fn revalidates_dns_after_redirect() {
        let transport = Arc::new(MockTransport::default());
        transport.responses.lock().unwrap().extend([
            response(
                StatusCode::FOUND,
                Some("https://private.example/secret"),
                b"",
            ),
            response(StatusCode::OK, None, b"should never be reached"),
        ]);
        let mut dns = MockDns::default();
        dns.answers.insert(
            "public.example".into(),
            vec!["93.184.216.34:443".parse().unwrap()],
        );
        dns.answers.insert(
            "private.example".into(),
            vec!["127.0.0.1:443".parse().unwrap()],
        );
        let client =
            SecureHttpClient::new(transport.clone(), Arc::new(dns), EgressPolicy::default());
        let error = client
            .get("https://public.example/start", 1024)
            .await
            .unwrap_err();
        assert_eq!(error.kind, WebErrorKind::AddressBlocked);
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn blocks_mixed_public_and_private_dns_answers() {
        let transport = Arc::new(MockTransport::default());
        let mut dns = MockDns::default();
        dns.answers.insert(
            "mixed.example".into(),
            vec![
                "93.184.216.34:443".parse().unwrap(),
                "10.0.0.2:443".parse().unwrap(),
            ],
        );
        let client =
            SecureHttpClient::new(transport.clone(), Arc::new(dns), EgressPolicy::default());
        let error = client
            .get("https://mixed.example/", 1024)
            .await
            .unwrap_err();
        assert_eq!(error.kind, WebErrorKind::AddressBlocked);
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_302_becomes_get_and_rechecks_redirect_host() {
        let transport = Arc::new(MockTransport::default());
        transport.responses.lock().unwrap().extend([
            response(StatusCode::FOUND, Some("https://next.example/results"), b""),
            response(StatusCode::OK, None, b"ok"),
        ]);
        let mut dns = MockDns::default();
        dns.answers.insert(
            "search.example".into(),
            vec!["93.184.216.34:443".parse().unwrap()],
        );
        dns.answers.insert(
            "next.example".into(),
            vec!["93.184.216.35:443".parse().unwrap()],
        );
        let client =
            SecureHttpClient::new(transport.clone(), Arc::new(dns), EgressPolicy::default());
        let response = client
            .post_form(
                "https://search.example/query",
                vec![("q".into(), "rust".into())],
                1024,
            )
            .await
            .unwrap();
        assert_eq!(response.final_url, "https://next.example/results");
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], "POST https://search.example/query");
        assert_eq!(requests[1], "GET https://next.example/results");
    }
}
