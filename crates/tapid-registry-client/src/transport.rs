use crate::TransportError;
use std::{io::Read, time::Duration};
use url::Url;

const STANDARD_METADATA_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const STANDARD_ARTIFACT_MAX_RESPONSE_BYTES: usize = 512 * 1024 * 1024;

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
    /// Creates the bounded transport used for registry metadata.
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

    /// Creates the separately bounded transport used for package archives.
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
    fn standard_transports_keep_metadata_and_artifact_limits_separate() {
        let metadata = HttpsTransport::standard().unwrap();
        let artifact = HttpsTransport::standard_artifact().unwrap();

        assert_eq!(metadata.max_response_bytes, 32 * 1024 * 1024);
        assert_eq!(artifact.max_response_bytes, 512 * 1024 * 1024);
    }
}
