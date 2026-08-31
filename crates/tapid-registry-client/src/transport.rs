use crate::TransportError;
use std::{io::Read, time::Duration};
use url::Url;

const STANDARD_METADATA_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const STANDARD_ARTIFACT_MAX_RESPONSE_BYTES: usize = 512 * 1024 * 1024;
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

fn read_bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>, TransportError> {
    let mut body = Vec::new();
    let take_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    reader
        .take(take_limit)
        .read_to_end(&mut body)
        .map_err(|error| TransportError::Http(error.to_string()))?;
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

/// HTTPS transport with an allow-list and bounded response body. It sends no credentials.
pub struct HttpsTransport {
    client: reqwest::blocking::Client,
    allowed_origins: Vec<Origin>,
    max_response_bytes: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}
impl Origin {
    fn parse(s: &str) -> Result<Self, TransportError> {
        let url = Url::parse(s).map_err(|_| TransportError::InvalidUrl(s.into()))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(TransportError::OriginNotAllowed(s.into()));
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
    if !raw.is_ascii() {
        return false;
    }
    let Some(remainder) = raw.strip_prefix("https://") else {
        return false;
    };
    let Some(authority) = remainder.split(['/', '?', '#']).next() else {
        return false;
    };
    !authority.is_empty()
        && !authority.starts_with('/')
        && !authority.contains('@')
        && url.as_str() == raw
        && !raw
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
        && !raw.contains('\\')
        && !path_has_noncanonical_percent_encoding(url.path())
        && url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn path_has_noncanonical_percent_encoding(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        let Some(encoded) = bytes.get(index + 1..index + 3) else {
            return true;
        };
        if encoded.iter().any(|byte| matches!(byte, b'a'..=b'f')) {
            return true;
        }
        let Ok(encoded) = str::from_utf8(encoded) else {
            return true;
        };
        let Ok(decoded) = u8::from_str_radix(encoded, 16) else {
            return true;
        };
        if decoded.is_ascii_control()
            || decoded.is_ascii_alphanumeric()
            || b"-._~".contains(&decoded)
        {
            return true;
        }
        index += 3;
    }
    false
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
        let allowed_origins = allowed_origins
            .into_iter()
            .map(|s| Origin::parse(s.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        if allowed_origins.is_empty() || max_response_bytes == 0 {
            return Err(TransportError::InvalidResponse(
                "non-empty origins and positive response limit required".into(),
            ));
        }
        let policy = reqwest::redirect::Policy::custom({
            let allowed = allowed_origins.clone();
            move |attempt| {
                if attempt.previous().last().is_some_and(|previous| {
                    redirect_is_allowed(previous, attempt.url(), &allowed, attempt.previous().len())
                }) {
                    attempt.follow()
                } else {
                    attempt.error("redirect rejected by exact-origin policy")
                }
            }
        });
        let client = reqwest::blocking::Client::builder()
            .user_agent("tapid/0.0.2")
            .timeout(timeout)
            .redirect(policy)
            .build()
            .map_err(|error| TransportError::Http(error.to_string()))?;
        Ok(Self {
            client,
            allowed_origins,
            max_response_bytes,
        })
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

    fn get_internal(
        &self,
        url: &str,
        accept: Option<&str>,
    ) -> Result<HttpResponse, TransportError> {
        let parsed = Url::parse(url).map_err(|_| TransportError::InvalidUrl(url.into()))?;
        if !request_url_is_safe(url, &parsed)
            || !self
                .allowed_origins
                .iter()
                .any(|origin| *origin == Origin::of(&parsed))
        {
            return Err(TransportError::OriginNotAllowed(url.into()));
        }
        let accept = accept
            .map(reqwest::header::HeaderValue::from_str)
            .transpose()
            .map_err(|_| TransportError::InvalidResponse("invalid Accept header value".into()))?;
        execute_bounded_get(
            || {
                let mut request = self.client.get(parsed.clone());
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
                        return AttemptOutcome::Retry(format!("{class} failure: {error}"), None);
                    }
                    Err(error) => {
                        let class = request_error_class(&error);
                        return AttemptOutcome::Fatal(TransportError::Http(format!(
                            "{class} failure: {error}"
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
