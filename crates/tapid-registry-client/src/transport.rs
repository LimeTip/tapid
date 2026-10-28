use crate::TransportError;
use std::{io::Read, time::Duration};
use url::Url;

pub(crate) const STANDARD_METADATA_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const STANDARD_ARTIFACT_MAX_RESPONSE_BYTES: usize = 512 * 1024 * 1024;
const MAX_GET_ATTEMPTS: u32 = 3;
const MAX_REDIRECT_HOPS: usize = 10;
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);
const STANDARD_ALLOWED_ORIGINS: [&str; 3] = [
    "https://registry.npmjs.org",
    "https://jsr.io",
    "https://npm.jsr.io",
];

fn should_retry_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

fn retry_delay(completed_attempts: u32) -> Duration {
    INITIAL_RETRY_DELAY * (1 << completed_attempts.saturating_sub(1))
}

fn retry_after_delay(status: u16, headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    retry_after_delay_at(status, headers, std::time::SystemTime::now())
}

fn retry_after_delay_at(
    status: u16,
    headers: &reqwest::header::HeaderMap,
    now: std::time::SystemTime,
) -> Option<Duration> {
    if status != 429 {
        return None;
    }
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds.min(60)));
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(
        retry_at
            .duration_since(now)
            .unwrap_or(Duration::ZERO)
            .min(Duration::from_secs(60)),
    )
}

fn request_error_class(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection"
    } else if error.is_redirect() {
        "redirect policy"
    } else if error.is_body() {
        "response body"
    } else if error.is_decode() {
        "response decode"
    } else if error.is_request() {
        "request"
    } else {
        "transport"
    }
}

pub(crate) fn read_bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>, TransportError> {
    let mut body = Vec::new();
    let take_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    reader
        .take(take_limit)
        .read_to_end(&mut body)
        .map_err(|_| TransportError::Http("response body read failed".into()))?;
    if body.len() > limit {
        return Err(TransportError::TooLarge { limit });
    }
    Ok(body)
}

enum AttemptOutcome {
    Complete(HttpResponse),
    Retry(String, Option<Duration>),
    Fatal(TransportError),
}

fn execute_bounded_get(
    mut operation: impl FnMut() -> AttemptOutcome,
    mut wait: impl FnMut(u32, Option<Duration>),
) -> Result<HttpResponse, TransportError> {
    for attempt in 1..=MAX_GET_ATTEMPTS {
        match operation() {
            AttemptOutcome::Complete(response) => return Ok(response),
            AttemptOutcome::Fatal(error) => return Err(error),
            AttemptOutcome::Retry(_reason, delay) if attempt < MAX_GET_ATTEMPTS => {
                wait(attempt, delay)
            }
            AttemptOutcome::Retry(reason, _) => {
                let message = if let Some((class, detail)) = reason.split_once(": ") {
                    format!("{class} after {attempt} attempts: {detail}")
                } else {
                    format!("{reason} after {attempt} attempts")
                };
                return Err(TransportError::Http(message));
            }
        }
    }
    unreachable!("positive retry bound")
}

/// A deliberately small boundary: production uses HTTPS, tests can use a local server.
pub trait HttpTransport: Send + Sync {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError>;

    /// Fetches a resource with an explicit response media type.
    ///
    /// Test and compatibility transports may use the ordinary response when they
    /// do not support content negotiation.
    fn get_with_accept(&self, url: &str, _accept: &str) -> Result<HttpResponse, TransportError> {
        self.get(url)
    }
}

impl<T: HttpTransport + ?Sized> HttpTransport for &T {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        (**self).get(url)
    }

    fn get_with_accept(&self, url: &str, accept: &str) -> Result<HttpResponse, TransportError> {
        (**self).get_with_accept(url, accept)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    /// HTTP status code returned by the bounded request.
    pub status: u16,
    /// Parsed response Content-Type header when it is valid UTF-8.
    pub content_type: Option<String>,
    /// Complete response bytes, bounded by the transport instance's configured limit.
    pub body: Vec<u8>,
}

/// HTTPS transport with exact-origin credentials, an allow-list and bounded responses.
pub struct HttpsTransport {
    client: reqwest::blocking::Client,
    allowed_origins: Vec<Origin>,
    credentials: Vec<(Origin, reqwest::header::HeaderValue)>,
    max_response_bytes: usize,
}

/// Explicit registry network settings. No ambient proxy configuration is consulted.
#[derive(Clone, Default)]
pub struct NetworkOptions {
    roots: Vec<reqwest::Certificate>,
    proxy: Option<reqwest::Proxy>,
}

impl NetworkOptions {
    /// Snapshots an optional unauthenticated HTTP CONNECT proxy and additional PEM roots.
    /// Accepts at most 16 bundles, 1 MiB each, 4 MiB total and 128 certificates.
    /// Bundle syntax is checked here; X.509 validity is checked when building transport.
    /// This performs no filesystem, environment, or network access.
    pub fn new(proxy: Option<&str>, ca_bundles: &[Vec<u8>]) -> Result<Self, TransportError> {
        const MAX_CA_FILE_BYTES: usize = 1024 * 1024;
        if ca_bundles.len() > 16
            || ca_bundles
                .iter()
                .any(|bundle| bundle.len() > MAX_CA_FILE_BYTES)
            || ca_bundles.iter().map(Vec::len).sum::<usize>() > 4 * MAX_CA_FILE_BYTES
        {
            return Err(TransportError::InvalidResponse(
                "registry CA input limit exceeded".into(),
            ));
        }
        let mut roots = Vec::new();
        for bundle in ca_bundles {
            validate_pem_bundle(bundle)?;
            roots.extend(reqwest::Certificate::from_pem_bundle(bundle).map_err(|_| {
                TransportError::InvalidResponse("invalid registry CA bundle".into())
            })?);
            if roots.len() > 128 {
                return Err(TransportError::InvalidResponse(
                    "registry CA certificate limit exceeded".into(),
                ));
            }
        }
        let proxy = proxy.map(parse_proxy).transpose()?;
        Ok(Self { roots, proxy })
    }
}

fn parse_proxy(raw: &str) -> Result<reqwest::Proxy, TransportError> {
    let invalid = || TransportError::InvalidResponse("invalid registry HTTP CONNECT proxy".into());
    let url = Url::parse(raw).map_err(|_| invalid())?;
    let port = raw
        .strip_prefix("http://")
        .and_then(|authority| authority.trim_end_matches('/').rsplit_once(':'))
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .filter(|port| *port != 0);
    if url.scheme() != "http"
        || url.host_str().is_none()
        || port.is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || raw
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || raw.contains('\\')
        || raw.contains('@')
    {
        return Err(invalid());
    }
    reqwest::Proxy::https(url.as_str()).map_err(|_| invalid())
}

fn validate_pem_bundle(bundle: &[u8]) -> Result<(), TransportError> {
    let invalid = || TransportError::InvalidResponse("invalid registry CA bundle".into());
    let mut remaining = std::str::from_utf8(bundle).map_err(|_| invalid())?.trim();
    if remaining.is_empty() {
        return Err(invalid());
    }
    while !remaining.is_empty() {
        remaining = remaining
            .strip_prefix("-----BEGIN CERTIFICATE-----")
            .ok_or_else(invalid)?;
        let (body, rest) = remaining
            .split_once("-----END CERTIFICATE-----")
            .ok_or_else(invalid)?;
        if body.trim().is_empty()
            || !body
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+/=\r\n \t".contains(&byte))
        {
            return Err(invalid());
        }
        remaining = rest.trim();
    }
    Ok(())
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}
impl Origin {
    fn parse(s: &str) -> Result<Self, TransportError> {
        let url =
            Url::parse(s).map_err(|_| TransportError::InvalidUrl("registry origin".into()))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(TransportError::OriginNotAllowed("registry origin".into()));
        }
        Ok(Self {
            scheme: url.scheme().into(),
            host: url.host_str().unwrap_or_default().to_ascii_lowercase(),
            port: url.port_or_known_default(),
        })
    }
    fn of(url: &Url) -> Self {
        Self {
            scheme: url.scheme().into(),
            host: url.host_str().unwrap_or_default().to_ascii_lowercase(),
            port: url.port_or_known_default(),
        }
    }
}

pub(crate) fn request_url_is_safe(raw: &str, url: &Url) -> bool {
    raw == url.as_str() && raw.parse::<tapid_core::ArtifactUrl>().is_ok()
}

fn redirect_is_allowed(
    previous: &Url,
    next: &Url,
    allowed: &[Origin],
    previous_count: usize,
) -> bool {
    previous_count <= MAX_REDIRECT_HOPS
        && request_url_is_safe(next.as_str(), next)
        && Origin::of(previous) == Origin::of(next)
        && allowed.contains(&Origin::of(next))
}

fn exact_origin_redirect_policy(allowed: Vec<Origin>) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().last().is_some_and(|previous| {
            redirect_is_allowed(previous, attempt.url(), &allowed, attempt.previous().len())
        }) {
            attempt.follow()
        } else {
            attempt.error("redirect rejected by exact-origin policy")
        }
    })
}

impl HttpsTransport {
    /// Creates a credential-free HTTPS transport with exact-origin, timeout, redirect,
    /// and response-size controls.
    pub fn new<I, S>(
        allowed_origins: I,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, TransportError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::new_authenticated(
            allowed_origins,
            std::iter::empty::<(String, String)>(),
            timeout,
            max_response_bytes,
        )
    }

    /// Creates a bounded HTTPS transport with bearer credentials bound to exact origins.
    /// Credentials are attached only to requests for their configured origin; redirects
    /// remain restricted to that same origin.
    pub fn new_authenticated<I, S, C, O>(
        allowed_origins: I,
        credentials: C,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, TransportError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: IntoIterator<Item = (O, String)>,
        O: AsRef<str>,
    {
        Self::new_authenticated_with_network(
            allowed_origins,
            credentials,
            timeout,
            max_response_bytes,
            &NetworkOptions::default(),
        )
    }

    /// Creates an authenticated transport using an explicit proxy and additional roots.
    /// Ambient proxy and bypass variables are ignored, including for direct transport.
    pub fn new_authenticated_with_network<I, S, C, O>(
        allowed_origins: I,
        credentials: C,
        timeout: Duration,
        max_response_bytes: usize,
        network: &NetworkOptions,
    ) -> Result<Self, TransportError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: IntoIterator<Item = (O, String)>,
        O: AsRef<str>,
    {
        let allowed_origins = allowed_origins
            .into_iter()
            .map(|s| Origin::parse(s.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        if allowed_origins.is_empty() || max_response_bytes == 0 {
            return Err(TransportError::InvalidResponse(
                "non-empty origins and positive response limit required".into(),
            ));
        }
        let credentials = credentials
            .into_iter()
            .map(|(origin, token)| {
                let origin = Origin::parse(origin.as_ref())?;
                if !allowed_origins.contains(&origin) || token.is_empty() {
                    return Err(TransportError::OriginNotAllowed("credential origin".into()));
                }
                let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| {
                        TransportError::InvalidResponse("invalid registry credential".into())
                    })?;
                Ok((origin, value))
            })
            .collect::<Result<Vec<_>, TransportError>>()?;
        let policy = exact_origin_redirect_policy(allowed_origins.clone());
        let mut builder = reqwest::blocking::Client::builder()
            .no_proxy()
            .user_agent("tapid/0.0.2")
            .timeout(timeout)
            .redirect(policy);
        for root in &network.roots {
            builder = builder.add_root_certificate(root.clone());
        }
        if let Some(proxy) = &network.proxy {
            builder = builder.proxy(proxy.clone());
        }
        let client = builder
            .build()
            .map_err(|_| TransportError::Http("cannot initialize registry TLS transport".into()))?;
        Ok(Self {
            client,
            allowed_origins,
            credentials,
            max_response_bytes,
        })
    }
    /// Creates metadata transport with additional registry origins and exact-origin credentials.
    pub fn authenticated_metadata(
        extra_origins: impl IntoIterator<Item = String>,
        credentials: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, TransportError> {
        Self::authenticated_metadata_with_network(
            extra_origins,
            credentials,
            &NetworkOptions::default(),
        )
    }

    /// Creates standard bounded metadata transport with explicit network settings.
    pub fn authenticated_metadata_with_network(
        extra_origins: impl IntoIterator<Item = String>,
        credentials: impl IntoIterator<Item = (String, String)>,
        network: &NetworkOptions,
    ) -> Result<Self, TransportError> {
        let origins = STANDARD_ALLOWED_ORIGINS
            .into_iter()
            .map(str::to_owned)
            .chain(extra_origins)
            .collect::<Vec<_>>();
        Self::new_authenticated_with_network(
            origins,
            credentials,
            Duration::from_secs(20),
            STANDARD_METADATA_MAX_RESPONSE_BYTES,
            network,
        )
    }

    /// Creates artifact transport with additional registry origins and exact-origin credentials.
    pub fn authenticated_artifact(
        extra_origins: impl IntoIterator<Item = String>,
        credentials: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, TransportError> {
        Self::authenticated_artifact_with_network(
            extra_origins,
            credentials,
            &NetworkOptions::default(),
        )
    }

    /// Creates standard bounded artifact transport with explicit network settings.
    pub fn authenticated_artifact_with_network(
        extra_origins: impl IntoIterator<Item = String>,
        credentials: impl IntoIterator<Item = (String, String)>,
        network: &NetworkOptions,
    ) -> Result<Self, TransportError> {
        let origins = STANDARD_ALLOWED_ORIGINS
            .into_iter()
            .map(str::to_owned)
            .chain(extra_origins)
            .collect::<Vec<_>>();
        Self::new_authenticated_with_network(
            origins,
            credentials,
            Duration::from_secs(20),
            STANDARD_ARTIFACT_MAX_RESPONSE_BYTES,
            network,
        )
    }

    /// Creates the bounded transport used for registry metadata.
    pub fn standard() -> Result<Self, TransportError> {
        Self::new(
            STANDARD_ALLOWED_ORIGINS,
            Duration::from_secs(20),
            STANDARD_METADATA_MAX_RESPONSE_BYTES,
        )
    }

    /// Creates the separately bounded transport used for package archives.
    pub fn standard_artifact() -> Result<Self, TransportError> {
        Self::new(
            STANDARD_ALLOWED_ORIGINS,
            Duration::from_secs(20),
            STANDARD_ARTIFACT_MAX_RESPONSE_BYTES,
        )
    }

    fn authorization_for(&self, url: &Url) -> Option<&reqwest::header::HeaderValue> {
        let origin = Origin::of(url);
        self.credentials
            .iter()
            .find(|(configured, _)| configured == &origin)
            .map(|(_, value)| value)
    }

    fn get_internal(
        &self,
        url: &str,
        accept: Option<&str>,
    ) -> Result<HttpResponse, TransportError> {
        let parsed =
            Url::parse(url).map_err(|_| TransportError::InvalidUrl("registry request".into()))?;
        if !request_url_is_safe(url, &parsed)
            || !self
                .allowed_origins
                .iter()
                .any(|origin| *origin == Origin::of(&parsed))
        {
            return Err(TransportError::OriginNotAllowed("registry request".into()));
        }
        let accept = accept
            .map(reqwest::header::HeaderValue::from_str)
            .transpose()
            .map_err(|_| TransportError::InvalidResponse("invalid Accept header value".into()))?;
        execute_bounded_get(
            || {
                let mut request = self.client.get(parsed.clone());
                if let Some(credential) = self.authorization_for(&parsed) {
                    request = request.header(reqwest::header::AUTHORIZATION, credential);
                }
                if let Some(value) = &accept {
                    request = request.header(reqwest::header::ACCEPT, value);
                }
                let response = match request.send() {
                    Ok(response) => response,
                    Err(error)
                        if !error.is_redirect()
                            && (error.is_timeout() || error.is_connect() || error.is_request()) =>
                    {
                        let class = request_error_class(&error);
                        return AttemptOutcome::Retry(format!("{class} failure"), None);
                    }
                    Err(error) => {
                        let class = request_error_class(&error);
                        return AttemptOutcome::Fatal(TransportError::Http(format!(
                            "{class} failure"
                        )));
                    }
                };
                let status = response.status().as_u16();
                if should_retry_status(status) {
                    return AttemptOutcome::Retry(
                        format!("HTTP status {status}"),
                        retry_after_delay(status, response.headers()),
                    );
                }
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let body = match read_bounded(response, self.max_response_bytes) {
                    Ok(body) => body,
                    Err(TransportError::Http(error)) => {
                        return AttemptOutcome::Retry(
                            format!("response read failure: {error}"),
                            None,
                        );
                    }
                    Err(error) => return AttemptOutcome::Fatal(error),
                };
                AttemptOutcome::Complete(HttpResponse {
                    status,
                    content_type,
                    body,
                })
            },
            |attempt, requested| {
                std::thread::sleep(requested.unwrap_or_else(|| retry_delay(attempt)))
            },
        )
    }
}
impl HttpTransport for HttpsTransport {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        self.get_internal(url, None)
    }

    fn get_with_accept(&self, url: &str, accept: &str) -> Result<HttpResponse, TransportError> {
        self.get_internal(url, Some(accept))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NpmRegistry;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::Arc,
        thread,
    };

    #[test]
    fn custom_ca_works_through_production_transport_construction() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("https://{}", listener.local_addr().unwrap());
        let server = serve_one_tls_response(
            listener,
            local_tls_server_config(),
            "200 OK",
            String::new(),
            b"trusted".to_vec(),
        );
        let network = NetworkOptions::new(
            None,
            &[include_bytes!("../tests/fixtures/local-test-ca.pem").to_vec()],
        )
        .unwrap();
        let transport = HttpsTransport::new_authenticated_with_network(
            [origin.clone()],
            [(origin.clone(), "fixture-secret".into())],
            Duration::from_secs(2),
            4096,
            &network,
        )
        .unwrap();
        assert_eq!(
            transport.get(&format!("{origin}/package")).unwrap().body,
            b"trusted"
        );
        assert_eq!(
            server.join().unwrap().authorization.as_deref(),
            Some("Bearer fixture-secret")
        );
    }

    #[test]
    fn ca_inputs_are_strict_bounded_and_errors_are_redacted() {
        let ca = include_bytes!("../tests/fixtures/local-test-ca.pem").to_vec();
        let mut trailing = ca.clone();
        trailing.extend_from_slice(b"secret-sentinel");
        for bundles in [
            vec![vec![]],
            vec![b"secret-sentinel".to_vec()],
            vec![trailing],
            vec![ca.clone(); 17],
            vec![vec![b' '; 1024 * 1024 + 1]],
            vec![ca.repeat(129)],
        ] {
            let error = NetworkOptions::new(None, &bundles)
                .err()
                .expect("invalid bundle must fail");
            assert!(!format!("{error:?} {error}").contains("secret-sentinel"));
        }
        assert!(NetworkOptions::new(None, &[]).is_ok());
        assert!(NetworkOptions::new(None, &vec![ca.clone(); 16]).is_ok());
        assert!(NetworkOptions::new(None, &[ca.repeat(128)]).is_ok());
        let mut padded = ca;
        padded.resize(1024 * 1024, b' ');
        assert!(NetworkOptions::new(None, &vec![padded.clone(); 4]).is_ok());
        assert!(NetworkOptions::new(None, &vec![padded; 5]).is_err());
    }

    #[test]
    fn proxy_configuration_rejects_unsupported_or_credentialed_urls() {
        for proxy in [
            "https://proxy.example:8080",
            "socks5://proxy.example:1080",
            "http://proxy.example",
            "http://proxy.example:0",
            "http://secret-sentinel@proxy.example:8080",
            "http://proxy.example:8080/path",
            "http://proxy.example:8080?secret-sentinel",
            "http://proxy.example:8080#secret-sentinel",
            "http://proxy.example:8080\n",
        ] {
            let error = NetworkOptions::new(Some(proxy), &[])
                .err()
                .expect("unsupported proxy must fail");
            assert!(!format!("{error:?} {error}").contains("secret-sentinel"));
        }
        for proxy in [
            "http://proxy.example:8080",
            "http://proxy.example:80",
            "http://[::1]:8080/",
        ] {
            assert!(NetworkOptions::new(Some(proxy), &[]).is_ok());
        }
    }

    #[test]
    fn rejected_urls_and_transport_failures_do_not_echo_supplied_secrets() {
        let error = HttpsTransport::new(
            ["https://secret-sentinel@example.test"],
            Duration::from_millis(50),
            1024,
        )
        .err()
        .unwrap();
        assert!(!format!("{error:?} {error}").contains("secret-sentinel"));
        let transport =
            HttpsTransport::new(["https://127.0.0.1:9"], Duration::from_millis(50), 1024).unwrap();
        for url in [
            "https://secret-sentinel@127.0.0.1:9/",
            "https://127.0.0.1:9/secret-sentinel",
            "secret-sentinel",
        ] {
            let error = transport.get(url).unwrap_err();
            assert!(!format!("{error:?} {error}").contains("secret-sentinel"));
        }
    }

    #[test]
    fn connect_proxy_keeps_registry_authorization_inside_tls() {
        let registry = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = registry.local_addr().unwrap();
        let origin = format!("https://{address}");
        let server = serve_one_tls_response(
            registry,
            local_tls_server_config(),
            "200 OK",
            String::new(),
            b"tunneled".to_vec(),
        );
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let proxy_server = thread::spawn(move || {
            proxy.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let (mut client, _) = loop {
                match proxy.accept() {
                    Ok(socket) => break socket,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("proxy accept failed: {error}"),
                }
            };
            client.set_nonblocking(false).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            client
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                assert!(headers.len() < 8192);
                let mut byte = [0];
                client.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let mut upstream =
                std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
            upstream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            upstream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
            let mut client_read = client.try_clone().unwrap();
            let mut upstream_write = upstream.try_clone().unwrap();
            let outgoing = thread::spawn(move || {
                let _ = std::io::copy(&mut client_read, &mut upstream_write);
                let _ = upstream_write.shutdown(std::net::Shutdown::Write);
            });
            let _ = std::io::copy(&mut upstream, &mut client);
            let _ = client.shutdown(std::net::Shutdown::Both);
            outgoing.join().unwrap();
            String::from_utf8(headers).unwrap()
        });
        let network = NetworkOptions::new(
            Some(&proxy_url),
            &[include_bytes!("../tests/fixtures/local-test-ca.pem").to_vec()],
        )
        .unwrap();
        let transport = HttpsTransport::new_authenticated_with_network(
            [origin.clone()],
            [(origin.clone(), "fixture-secret".into())],
            Duration::from_secs(2),
            4096,
            &network,
        )
        .unwrap();
        assert_eq!(
            transport.get(&format!("{origin}/package")).unwrap().body,
            b"tunneled"
        );
        drop(transport);
        let connect = proxy_server.join().unwrap();
        assert!(connect.starts_with(&format!("CONNECT {address} HTTP/1.1\r\n")));
        assert!(!connect.to_ascii_lowercase().contains("authorization"));
        assert!(!connect.contains("fixture-secret"));
        assert_eq!(
            server.join().unwrap().authorization.as_deref(),
            Some("Bearer fixture-secret")
        );
    }

    #[test]
    fn proxy_authentication_failure_is_bounded_and_redacted() {
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let server = thread::spawn(move || {
            proxy.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut attempts = 0;
            while attempts < 3 {
                let (mut socket, _) = match proxy.accept() {
                    Ok(socket) => socket,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("proxy failure fixture accept failed: {error}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 8192);
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(!request.to_ascii_lowercase().contains("authorization"));
                assert!(!request.contains("fixture-secret"));
                socket.write_all(b"HTTP/1.1 407 secret-sentinel\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                attempts += 1;
            }
            attempts
        });
        let network = NetworkOptions::new(Some(&proxy_url), &[]).unwrap();
        let origin = "https://registry.example.test";
        let transport = HttpsTransport::new_authenticated_with_network(
            [origin],
            [(origin, "fixture-secret".into())],
            Duration::from_secs(1),
            1024,
            &network,
        )
        .unwrap();
        let error = transport.get(&format!("{origin}/package")).unwrap_err();
        let rendered = format!("{error:?} {error}");
        assert!(rendered.contains("3 attempts"));
        assert!(!rendered.contains("secret-sentinel"));
        assert!(!rendered.contains("fixture-secret"));
        assert_eq!(server.join().unwrap(), 3);
    }

    fn local_tls_server_config() -> Arc<rustls::ServerConfig> {
        // Public certificate/key fixture for local TLS tests only.
        let certificate = rustls::pki_types::CertificateDer::from(
            include_bytes!("../tests/fixtures/local-test-cert.der").to_vec(),
        );
        let private_key =
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                include_bytes!("../tests/fixtures/local-test-key.der").to_vec(),
            ));
        Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], private_key)
            .unwrap(),
        )
    }

    #[derive(Debug)]
    struct CapturedHeaders {
        authorization: Option<String>,
        accept: Option<String>,
    }

    fn serve_one_tls_response(
        listener: TcpListener,
        config: Arc<rustls::ServerConfig>,
        status: &'static str,
        extra_headers: String,
        body: Vec<u8>,
    ) -> thread::JoinHandle<CapturedHeaders> {
        thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let (socket, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("local TLS test server failed to accept request: {error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let connection = rustls::ServerConnection::new(config).unwrap();
            let mut stream = rustls::StreamOwned::new(connection, socket);
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            loop {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            let header = |name: &str| {
                request
                    .lines()
                    .find(|line| {
                        line.split_once(':')
                            .is_some_and(|(header, _)| header.eq_ignore_ascii_case(name))
                    })
                    .and_then(|line| line.split_once(':'))
                    .map(|(_, value)| value.trim().to_owned())
            };
            let captured = CapturedHeaders {
                authorization: header("authorization"),
                accept: header("accept"),
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            captured
        })
    }

    fn local_authenticated_transport(
        origins: &[String],
        credentials: Vec<(String, String)>,
    ) -> HttpsTransport {
        let network = NetworkOptions::new(
            None,
            &[include_bytes!("../tests/fixtures/local-test-ca.pem").to_vec()],
        )
        .unwrap();
        HttpsTransport::new_authenticated_with_network(
            origins,
            credentials,
            Duration::from_secs(2),
            4096,
            &network,
        )
        .unwrap()
    }

    #[test]
    fn ambient_proxy_and_bypass_variables_are_ignored_in_isolated_processes() {
        for (test, bypass) in [
            (
                "custom_ca_works_through_production_transport_construction",
                "",
            ),
            ("connect_proxy_keeps_registry_authorization_inside_tls", "*"),
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                &format!("transport::tests::{test}"),
                "--nocapture",
            ]);
            for variable in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
            ] {
                command.env(variable, "http://secret-sentinel@127.0.0.1:9");
            }
            command.env("NO_PROXY", bypass).env("no_proxy", bypass);
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-sentinel"));
        }
    }

    #[test]
    fn invalid_der_ca_fails_closed_without_echoing_input() {
        let network = NetworkOptions::new(
            None,
            &[b"-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n".to_vec()],
        )
        .unwrap();
        let error = HttpsTransport::new_authenticated_with_network(
            ["https://registry.example.test"],
            std::iter::empty::<(String, String)>(),
            Duration::from_secs(1),
            1024,
            &network,
        )
        .err()
        .expect("invalid X.509 root must fail");
        assert!(!format!("{error:?} {error}").contains("AQID"));
    }

    #[test]
    fn untrusted_and_hostname_mismatched_certificates_fail_closed() {
        for (bind, trust_ca) in [("127.0.0.1:0", false), ("[::1]:0", true)] {
            let listener = TcpListener::bind(bind).unwrap();
            let origin = format!("https://{}", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                listener.set_nonblocking(true).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let (mut socket, _) = loop {
                    match listener.accept() {
                        Ok(socket) => break socket,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("TLS rejection fixture accept failed: {error}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut connection =
                    rustls::ServerConnection::new(local_tls_server_config()).unwrap();
                // A TLS alert proves the request reached certificate verification.
                connection.complete_io(&mut socket).unwrap_err().kind()
            });
            let bundles = if trust_ca {
                vec![include_bytes!("../tests/fixtures/local-test-ca.pem").to_vec()]
            } else {
                vec![]
            };
            let network = NetworkOptions::new(None, &bundles).unwrap();
            let transport = HttpsTransport::new_authenticated_with_network(
                [origin.clone()],
                [(origin.clone(), "fixture-secret".into())],
                Duration::from_secs(1),
                1024,
                &network,
            )
            .unwrap();
            let error = transport.get(&format!("{origin}/package")).unwrap_err();
            assert!(!format!("{error:?} {error}").contains("fixture-secret"));
            assert_eq!(server.join().unwrap(), std::io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn authenticated_https_request_sends_token_to_local_private_registry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("https://{address}");
        let packument = serde_json::json!({
            "name": "private-package",
            "versions": {
                "1.0.0": {
                    "name": "private-package",
                    "version": "1.0.0",
                    "dist": {
                        "tarball": format!("{origin}/private-package-1.0.0.tgz"),
                        "integrity": "sha512-tH3+Kn/2Ov5s20Oa2RA4rncViMGbN3RSoFjn0u4mmF9j/iGEDUlR4zsFuvu8dWM73cKf2LLQpkI5jICwZtdydA=="
                    }
                }
            }
        })
        .to_string()
        .into_bytes();
        let server = serve_one_tls_response(
            listener,
            local_tls_server_config(),
            "200 OK",
            "Content-Type: application/json\r\n".into(),
            packument,
        );
        let transport = local_authenticated_transport(
            std::slice::from_ref(&origin),
            vec![(origin.clone(), "fixture-private-token".into())],
        );
        let registry = NpmRegistry::new(&transport, origin.parse().unwrap());

        let artifacts = registry.fetch("private-package").unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].identity.name.to_string(), "private-package");
        assert!(artifacts[0].integrity.is_some());
        assert_eq!(
            artifacts[0].artifact_url,
            format!("{origin}/private-package-1.0.0.tgz")
        );
        let headers = server.join().unwrap();
        assert_eq!(
            headers.authorization.as_deref(),
            Some("Bearer fixture-private-token")
        );
        assert_eq!(
            headers.accept.as_deref(),
            Some("application/vnd.npm.install-v1+json")
        );
    }

    #[test]
    fn authenticated_redirect_to_another_local_origin_is_rejected_before_contacting_it() {
        let source_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let source_address = source_listener.local_addr().unwrap();
        let source_origin = format!("https://{source_address}");
        let destination_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        destination_listener.set_nonblocking(true).unwrap();
        let destination_address = destination_listener.local_addr().unwrap();
        let destination_origin = format!("https://{destination_address}");
        let source_server = serve_one_tls_response(
            source_listener,
            local_tls_server_config(),
            "302 Found",
            format!("Location: {destination_origin}/steal\r\n"),
            Vec::new(),
        );
        let transport = local_authenticated_transport(
            &[source_origin.clone(), destination_origin],
            vec![(source_origin.clone(), "fixture-private-token".into())],
        );

        let error = transport
            .get(&format!("{source_origin}/package"))
            .unwrap_err();

        assert!(matches!(error, TransportError::Http(message) if message.contains("redirect")));
        assert_eq!(
            source_server.join().unwrap().authorization.as_deref(),
            Some("Bearer fixture-private-token")
        );
        assert!(matches!(
            destination_listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn authorization_header_is_selected_only_for_exact_origin() {
        let transport = HttpsTransport::new_authenticated(
            ["https://private.example", "https://public.example"],
            [("https://private.example", "opaque-token".to_owned())],
            Duration::from_secs(1),
            1024,
        )
        .unwrap();
        let private = Url::parse("https://private.example/package").unwrap();
        let other_port = Url::parse("https://private.example:444/package").unwrap();
        let public = Url::parse("https://public.example/package").unwrap();
        assert_eq!(
            transport
                .authorization_for(&private)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer opaque-token")
        );
        assert!(transport.authorization_for(&other_port).is_none());
        assert!(transport.authorization_for(&public).is_none());
    }

    #[test]
    fn credentials_are_bound_to_exact_origin_and_invalid_values_are_rejected() {
        let transport = HttpsTransport::new_authenticated(
            ["https://private.example", "https://public.example"],
            [("https://private.example", "opaque-token".to_owned())],
            Duration::from_secs(1),
            1024,
        )
        .unwrap();
        let private = Origin::parse("https://private.example").unwrap();
        let public = Origin::parse("https://public.example").unwrap();
        assert!(
            transport
                .credentials
                .iter()
                .any(|(origin, _)| origin == &private)
        );
        assert!(
            !transport
                .credentials
                .iter()
                .any(|(origin, _)| origin == &public)
        );
        assert!(
            HttpsTransport::new_authenticated(
                ["https://private.example"],
                [("https://private.example", "bad\nvalue".to_owned())],
                Duration::from_secs(1),
                1024,
            )
            .is_err()
        );
        assert!(
            HttpsTransport::new_authenticated(
                ["https://private.example"],
                [("https://other.example", "opaque-token".to_owned())],
                Duration::from_secs(1),
                1024,
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_configured_origins_are_rejected() {
        for origin in [
            "https://user:pass@registry.example.test",
            "https://registry.example.test/path",
            "https://registry.example.test?token=value",
            "https://registry.example.test#fragment",
        ] {
            assert!(HttpsTransport::new([origin], Duration::from_secs(1), 1024).is_err());
        }
    }

    #[test]
    fn unsafe_request_urls_fail_before_network() {
        let transport =
            HttpsTransport::new(["https://127.0.0.1:9"], Duration::from_millis(50), 1024).unwrap();
        for url in [
            "https://user:pass@127.0.0.1:9/archive.tgz",
            "https://127.0.0.1:9/archive.tgz?token=value",
            "https://127.0.0.1:9/archive.tgz#fragment",
        ] {
            assert!(matches!(
                transport.get(url),
                Err(TransportError::OriginNotAllowed(_))
            ));
        }
    }

    #[test]
    fn transient_connection_failures_are_retried_three_times() {
        use std::{net::TcpListener, sync::mpsc, thread};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut accepted = 0;
            loop {
                match listener.accept() {
                    Ok((_stream, _)) => accepted += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop_rx.recv_timeout(Duration::from_millis(5)).is_ok() {
                            break;
                        }
                    }
                    Err(error) => panic!("local retry server failed: {error}"),
                }
            }
            accepted
        });
        let transport = HttpsTransport::new(
            [format!("https://{address}")],
            Duration::from_millis(200),
            1024,
        )
        .unwrap();

        let error = transport
            .get(&format!("https://{address}/archive.tgz"))
            .unwrap_err();

        stop_tx.send(()).unwrap();
        assert!((1..=3).contains(&server.join().unwrap()));
        assert!(matches!(error, TransportError::Http(message) if message.contains("3 attempts")));
    }

    #[test]
    fn timeout_failures_are_retried_and_classified() {
        use std::{net::TcpListener, sync::mpsc, thread};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut accepted = 0;
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        accepted += 1;
                        thread::spawn(move || {
                            let _stream = stream;
                            thread::sleep(Duration::from_millis(200));
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop_rx.recv_timeout(Duration::from_millis(5)).is_ok() {
                            break;
                        }
                    }
                    Err(error) => panic!("local timeout server failed: {error}"),
                }
            }
            accepted
        });
        let transport = HttpsTransport::new(
            [format!("https://{address}")],
            Duration::from_millis(30),
            1024,
        )
        .unwrap();

        let error = transport
            .get(&format!("https://{address}/archive.tgz"))
            .unwrap_err();

        stop_tx.send(()).unwrap();
        assert!((1..=3).contains(&server.join().unwrap()));
        assert!(
            matches!(error, TransportError::Http(message) if message.contains("after 3 attempts"))
        );
    }

    #[test]
    fn retry_runner_returns_success_after_transient_failures() {
        let mut attempts = 0;
        let response = execute_bounded_get(
            || {
                attempts += 1;
                if attempts < 3 {
                    AttemptOutcome::Retry("connection failure".into(), None)
                } else {
                    AttemptOutcome::Complete(HttpResponse {
                        status: 200,
                        content_type: None,
                        body: b"ok".to_vec(),
                    })
                }
            },
            |_, _| {},
        )
        .unwrap();

        assert_eq!(attempts, 3);
        assert_eq!(response.status, 200);
    }

    #[test]
    fn retry_runner_stops_immediately_on_permanent_failures() {
        let mut attempts = 0;
        let result = execute_bounded_get(
            || {
                attempts += 1;
                AttemptOutcome::Fatal(TransportError::OriginNotAllowed("blocked".into()))
            },
            |_, _| {},
        );

        assert_eq!(attempts, 1);
        assert!(matches!(result, Err(TransportError::OriginNotAllowed(_))));
    }

    #[test]
    fn retry_runner_reports_exhausted_status_and_body_context() {
        for reason in ["HTTP status 503", "response read failure"] {
            let mut attempts = 0;
            let error = execute_bounded_get(
                || {
                    attempts += 1;
                    AttemptOutcome::Retry(reason.into(), None)
                },
                |_, _| {},
            )
            .unwrap_err();

            assert_eq!(attempts, 3);
            assert!(matches!(
                error,
                TransportError::Http(message)
                    if message.contains(reason) && message.contains("after 3 attempts")
            ));
        }
    }

    #[test]
    fn retry_after_delta_seconds_is_bounded() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(
            retry_after_delay(429, &headers),
            Some(Duration::from_secs(2))
        );
        assert_eq!(retry_after_delay(503, &headers), None);
        headers.insert(reqwest::header::RETRY_AFTER, "9999".parse().unwrap());
        assert_eq!(
            retry_after_delay(429, &headers),
            Some(Duration::from_secs(60))
        );
        headers.insert(reqwest::header::RETRY_AFTER, "invalid".parse().unwrap());
        assert_eq!(retry_after_delay(429, &headers), None);
    }

    #[test]
    fn retry_after_http_dates_are_nonnegative_and_bounded() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            httpdate::fmt_http_date(now + Duration::from_secs(10))
                .parse()
                .unwrap(),
        );
        assert_eq!(
            retry_after_delay_at(429, &headers, now),
            Some(Duration::from_secs(10))
        );
        headers.insert(
            reqwest::header::RETRY_AFTER,
            httpdate::fmt_http_date(now + Duration::from_secs(120))
                .parse()
                .unwrap(),
        );
        assert_eq!(
            retry_after_delay_at(429, &headers, now),
            Some(Duration::from_secs(60))
        );
        headers.insert(
            reqwest::header::RETRY_AFTER,
            httpdate::fmt_http_date(now - Duration::from_secs(1))
                .parse()
                .unwrap(),
        );
        assert_eq!(
            retry_after_delay_at(429, &headers, now),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn only_transient_http_statuses_are_retried() {
        for status in [429, 500, 502, 503, 504] {
            assert!(should_retry_status(status));
        }
        for status in [400, 401, 403, 404, 501, 505] {
            assert!(!should_retry_status(status));
        }
    }

    #[test]
    fn cross_origin_redirect_is_rejected_even_when_destination_is_allowed_and_has_no_auth() {
        let source = "https://private.example";
        let destination = "https://cdn.example";
        let transport = HttpsTransport::new_authenticated(
            [source, destination],
            [(source, "source-only-token".to_owned())],
            Duration::from_secs(1),
            1024,
        )
        .unwrap();
        let previous = Url::parse("https://private.example/package").unwrap();
        let next = Url::parse("https://cdn.example/archive").unwrap();
        assert!(!redirect_is_allowed(
            &previous,
            &next,
            &transport.allowed_origins,
            1
        ));
        assert!(transport.authorization_for(&next).is_none());
    }

    #[test]
    fn redirect_policy_permits_only_the_same_allowed_origin() {
        let allowed = vec![Origin::parse("https://registry.example.test").unwrap()];
        let previous = Url::parse("https://registry.example.test/package").unwrap();
        let same_origin = Url::parse("https://registry.example.test/archive").unwrap();
        let cross_origin = Url::parse("https://cdn.example.test/archive").unwrap();
        let credentialed = Url::parse("https://user:pass@registry.example.test/archive").unwrap();

        assert!(redirect_is_allowed(
            &previous,
            &same_origin,
            &allowed,
            MAX_REDIRECT_HOPS
        ));
        assert!(!redirect_is_allowed(
            &previous,
            &same_origin,
            &allowed,
            MAX_REDIRECT_HOPS + 1
        ));
        assert!(!redirect_is_allowed(&previous, &cross_origin, &allowed, 0));
        assert!(!redirect_is_allowed(&previous, &credentialed, &allowed, 0));
    }

    #[test]
    fn request_urls_require_uppercase_percent_triplets() {
        let uppercase = "https://registry.npmjs.org/@alloc%2Fquick-lru";
        let lowercase = "https://registry.npmjs.org/@alloc%2fquick-lru";
        assert!(request_url_is_safe(
            uppercase,
            &Url::parse(uppercase).unwrap()
        ));
        assert!(!request_url_is_safe(
            lowercase,
            &Url::parse(lowercase).unwrap()
        ));
    }

    #[test]
    fn bounded_reader_handles_the_public_usize_max_limit_without_overflow() {
        let body = read_bounded(std::io::Cursor::new(Vec::<u8>::new()), usize::MAX).unwrap();
        assert!(body.is_empty());
    }

    #[test]
    fn bounded_reader_rejects_limit_plus_one_bytes() {
        let error = read_bounded(std::io::Cursor::new(vec![0_u8; 5]), 4).unwrap_err();
        assert!(matches!(error, TransportError::TooLarge { limit: 4 }));
    }

    #[test]
    fn standard_transports_keep_metadata_and_artifact_limits_separate() {
        let metadata = HttpsTransport::standard().unwrap();
        let artifact = HttpsTransport::standard_artifact().unwrap();

        assert_eq!(metadata.max_response_bytes, 32 * 1024 * 1024);
        assert_eq!(artifact.max_response_bytes, 512 * 1024 * 1024);
    }
}
