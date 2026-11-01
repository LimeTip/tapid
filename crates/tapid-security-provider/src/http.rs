use std::time::{Duration, Instant};
use tapid_protocol::SecurityRequest;

use crate::{CredentialMode, ProviderConfig, RetrievalError};

/// Explicit HTTPS query adapter. Construction performs no request or credential
/// lookup. One instance represents one bounded collection; it never retries,
/// redirects, refreshes identity, or caches response bodies.
pub struct HttpsProvider {
    config: ProviderConfig,
    client: reqwest::blocking::Client,
    endpoint: String,
    request_timeout: Duration,
    deadline: Instant,
}

impl HttpsProvider {
    /// Both limits must be nonzero and at most five minutes. The collection
    /// deadline starts now and bounds every request, including body reads.
    pub fn new(
        config: ProviderConfig,
        request_timeout: Duration,
        collection_timeout: Duration,
    ) -> Result<Self, RetrievalError> {
        let maximum = Duration::from_secs(300);
        if request_timeout.is_zero()
            || collection_timeout.is_zero()
            || request_timeout > maximum
            || collection_timeout > maximum
        {
            return Err(RetrievalError::Unavailable);
        }
        let deadline = Instant::now()
            .checked_add(collection_timeout)
            .ok_or(RetrievalError::Unavailable)?;
        let client = client_builder(request_timeout)
            .build()
            .map_err(|_| RetrievalError::Unavailable)?;
        let endpoint = format!("{}/v1/security/evidence/query", config.origin.as_str());
        Ok(Self {
            config,
            client,
            endpoint,
            request_timeout,
            deadline,
        })
    }

    /// Return raw signed bytes as a stream for `retrieve_recorded` or
    /// `collect_recorded`; an HTTP success is never verified security evidence.
    /// Consent must cover the exact request. Credential lookup occurs only after
    /// that check; existing credentials do not require an identity/browser refresh.
    pub fn query(
        &self,
        request: &SecurityRequest,
        consented: &[SecurityRequest],
        mode: CredentialMode,
        lookup: &mut impl FnMut(&str) -> Option<String>,
    ) -> Result<reqwest::blocking::Response, RetrievalError> {
        if Instant::now() >= self.deadline {
            return Err(RetrievalError::Unavailable);
        }
        let token = self.config.authorize(
            request,
            consented,
            mode,
            self.config.origin.as_str(),
            lookup,
        )?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(RetrievalError::Unavailable);
        }
        let body = serde_json::to_vec(request).map_err(|_| RetrievalError::Unavailable)?;
        let mut query = self
            .client
            .post(&self.endpoint)
            .timeout(self.request_timeout.min(remaining))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header(reqwest::header::CACHE_CONTROL, "no-store")
            .body(body);
        if let Some(token) = token {
            let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| RetrievalError::NotAuthorized)?;
            value.set_sensitive(true);
            query = query.header(reqwest::header::AUTHORIZATION, value);
        }
        let response = query.send().map_err(|_| RetrievalError::Unavailable)?;
        if response.status() != reqwest::StatusCode::OK
            || response
                .headers()
                .get_all(reqwest::header::CONTENT_ENCODING)
                .iter()
                .any(|value| value.as_bytes() != b"identity")
        {
            return Err(RetrievalError::Unavailable);
        }
        Ok(response)
    }
}

fn client_builder(timeout: Duration) -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(timeout)
        .timeout(timeout)
        // Do not route bearer credentials through environment proxy settings.
        .no_proxy()
}

#[cfg(test)]
mod tests;
