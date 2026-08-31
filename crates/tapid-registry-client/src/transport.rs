use crate::TransportError;
use std::{io::Read, time::Duration};
use url::Url;

const STANDARD_METADATA_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const STANDARD_ARTIFACT_MAX_RESPONSE_BYTES: usize = 512 * 1024 * 1024;

/// A deliberately small boundary: production uses HTTPS, tests can use a local server.
pub trait HttpTransport: Send + Sync {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError>;

    /// Fetches a resource while requesting the specified response media type.
    ///
    /// Implementations that do not support content negotiation may return the ordinary response.
    ///
    /// # Parameters
    ///
    /// * `url` - URL of the resource to fetch.
    /// * `accept` - Response media type to request.
    ///
    /// # Returns
    ///
    /// The HTTP response.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn example() -> Result<(), TransportError> {
    /// let transport = HttpsTransport::standard()?;
    /// let response = transport.get_with_accept(
    ///     "https://example.com/metadata",
    ///     "application/json",
    /// )?;
    /// # let _ = response;
    /// # Ok(())
    /// # }
    /// ```
    fn get_with_accept(&self, url: &str, _accept: &str) -> Result<HttpResponse, TransportError> {
        self.get(url)
    }
}

impl<T: HttpTransport + ?Sized> HttpTransport for &T {
    /// Performs an HTTP GET request through the referenced transport.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let response = transport.get("https://example.com")?;
    /// # Ok::<(), TransportError>(())
    /// ```
    ///
    /// # Returns
    ///
    /// The HTTP response or a transport error.
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        (**self).get(url)
    }

    /// Requests a resource with a specified response media type.
    ///
    /// # Arguments
    ///
    /// * `url` - The resource URL.
    /// * `accept` - The accepted response media type.
    ///
    /// # Returns
    ///
    /// The HTTP response returned for the request.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let response = transport.get_with_accept("https://example.com/data", "application/json")?;
    /// # Ok::<(), TransportError>(())
    /// ```
    fn get_with_accept(&self, url: &str, accept: &str) -> Result<HttpResponse, TransportError> {
        (**self).get_with_accept(url, accept)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
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
        if url.scheme() != "https" {
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
impl HttpsTransport {
    /// Creates an HTTPS transport restricted to the specified origins and response size limit.
    ///
    /// Redirects remain within the allow-listed origins and cannot cross origins.
    ///
    /// # Errors
    ///
    /// Returns an error if an origin is invalid, the allow-list is empty, the response
    /// limit is zero, or the HTTP client cannot be constructed.
    ///
    /// # Examples
    ///
    /// ```
    /// let transport = HttpsTransport::new(
    ///     ["https://registry.example.com"],
    ///     std::time::Duration::from_secs(20),
    ///     32 * 1024 * 1024,
    /// )?;
    /// # let _ = transport;
    /// # Ok::<(), TransportError>(())
    /// ```
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
                let from = attempt.previous().last().map(Origin::of);
                let to = Origin::of(attempt.url());
                if from.as_ref().is_some_and(|origin| *origin != to) || !allowed.contains(&to) {
                    attempt.stop()
                } else {
                    attempt.follow()
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
    /// Creates a bounded transport for fetching registry metadata from supported HTTPS origins.
    ///
    /// The transport uses a 20-second timeout and limits responses to 32 MiB.
    ///
    /// # Examples
    ///
    /// ```
    /// let transport = HttpsTransport::standard();
    /// assert!(transport.is_ok());
    /// ```
    pub fn standard() -> Result<Self, TransportError> {
        Self::new(
            [
                "https://registry.npmjs.org",
                "https://jsr.io",
                "https://npm.jsr.io",
            ],
            Duration::from_secs(20),
            STANDARD_METADATA_MAX_RESPONSE_BYTES,
        )
    }

    /// Creates a transport for package archives with the standard timeout and response-size limit.
    ///
    /// # Examples
    ///
    /// ```
    /// let transport = HttpsTransport::standard_artifact().unwrap();
    /// ```
    ///
    /// # Returns
    ///
    /// A configured transport allowing requests to the npm and JSR registries.
    pub fn standard_artifact() -> Result<Self, TransportError> {
        Self::new(
            [
                "https://registry.npmjs.org",
                "https://jsr.io",
                "https://npm.jsr.io",
            ],
            Duration::from_secs(20),
            STANDARD_ARTIFACT_MAX_RESPONSE_BYTES,
        )
    }

    /// Performs an HTTPS GET request for an allowed origin and collects the response body.
    ///
    /// An optional `Accept` header can be supplied for content negotiation. The response
    /// body must not exceed the configured maximum size.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let response = transport.get_internal("https://registry.example/package", Some("application/json"))?;
    /// assert_eq!(response.status, 200);
    /// # Ok::<(), TransportError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when the URL is invalid, the origin is not allowed, the
    /// `Accept` header is invalid, the request fails, or the response exceeds the
    /// configured size limit.
    ///
    /// # Returns
    ///
    /// The HTTP status, content type, and body of the response.
    fn get_internal(
        &self,
        url: &str,
        accept: Option<&str>,
    ) -> Result<HttpResponse, TransportError> {
        let parsed = Url::parse(url).map_err(|_| TransportError::InvalidUrl(url.into()))?;
        if parsed.scheme() != "https"
            || !self
                .allowed_origins
                .iter()
                .any(|origin| *origin == Origin::of(&parsed))
        {
            return Err(TransportError::OriginNotAllowed(url.into()));
        }
        let mut request = self.client.get(parsed);
        if let Some(accept) = accept {
            let value = reqwest::header::HeaderValue::from_str(accept).map_err(|_| {
                TransportError::InvalidResponse("invalid Accept header value".into())
            })?;
            request = request.header(reqwest::header::ACCEPT, value);
        }
        let response = request
            .send()
            .map_err(|error| TransportError::Http(error.to_string()))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        response
            .take(self.max_response_bytes as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|error| TransportError::Http(error.to_string()))?;
        if body.len() > self.max_response_bytes {
            return Err(TransportError::TooLarge {
                limit: self.max_response_bytes,
            });
        }
        Ok(HttpResponse {
            status,
            content_type,
            body,
        })
    }
}
impl HttpTransport for HttpsTransport {
    /// Retrieves a response from the specified HTTPS URL.
    ///
    /// # Examples
    ///
    /// ```
    /// let response = transport.get("https://example.com")?;
    /// assert!(response.status.is_success());
    /// # Ok::<(), TransportError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a [`TransportError`] if the request fails or the response cannot be read.
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        self.get_internal(url, None)
    }

    /// Retrieves a resource while requesting a specific response media type.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tapid_registry_client::transport::{HttpTransport, HttpsTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = HttpsTransport::standard()?;
    /// let response = transport.get_with_accept(
    ///     "https://registry.example.com/metadata",
    ///     "application/json",
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Returns
    ///
    /// The HTTP response received from the requested URL.
    fn get_with_accept(&self, url: &str, accept: &str) -> Result<HttpResponse, TransportError> {
        self.get_internal(url, Some(accept))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_transports_keep_metadata_and_artifact_limits_separate() {
        let metadata = HttpsTransport::standard().unwrap();
        let artifact = HttpsTransport::standard_artifact().unwrap();

        assert_eq!(metadata.max_response_bytes, 32 * 1024 * 1024);
        assert_eq!(artifact.max_response_bytes, 512 * 1024 * 1024);
    }
}
