use serde::Deserialize;
use tapid_core::RegistryOrigin;
use tapid_protocol::SecurityRequest;

use crate::RetrievalError;

/// Explicit credential selection. Headless execution never falls back to an
/// account credential, browser session, registry token or interactive sign-in.
#[derive(Clone, Copy)]
pub enum CredentialMode {
    Anonymous,
    Account,
    Headless,
}

/// Configuration contains environment variable names, never credential values.
/// Parsing performs no environment access, inventory disclosure or live requests.
/// This is an adapter input, not an activation of the CLI's live provider.
pub struct ProviderConfig {
    pub(crate) origin: RegistryOrigin,
    account: Option<String>,
    headless: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    version: u32,
    origin: String,
    account_credential_env: Option<String>,
    headless_credential_env: Option<String>,
    cache: CachePolicy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsentDocument {
    version: u32,
    origin: String,
    requests: Vec<SecurityRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CachePolicy {
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidProviderConfig;

impl ProviderConfig {
    /// Parse separately supplied disclosure consent for this destination. No
    /// wildcard, name-only or sign-in-derived grants are accepted. Callers must
    /// explicitly select a trusted consent document; never discover it in an
    /// untrusted project or infer consent from provider configuration.
    pub fn parse_consent(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<SecurityRequest>, InvalidProviderConfig> {
        if bytes.len() > 1024 * 1024 {
            return Err(InvalidProviderConfig);
        }
        let document: ConsentDocument =
            serde_json::from_slice(bytes).map_err(|_| InvalidProviderConfig)?;
        if document.version != 1
            || document.origin != self.origin.as_str()
            || document.requests.len() > crate::MAX_COLLECTION_REQUESTS
        {
            return Err(InvalidProviderConfig);
        }
        for (index, request) in document.requests.iter().enumerate() {
            request.validate_v2().map_err(|_| InvalidProviderConfig)?;
            if document.requests[..index].contains(request) {
                return Err(InvalidProviderConfig);
            }
        }
        Ok(document.requests)
    }

    /// Parse a bounded version-1 JSON adapter configuration. Evidence body caching
    /// must explicitly be disabled until a verified body cache is implemented.
    /// Errors deliberately contain no input, origins, variable names or secrets.
    pub fn parse(bytes: &[u8]) -> Result<Self, InvalidProviderConfig> {
        if bytes.len() > 64 * 1024 {
            return Err(InvalidProviderConfig);
        }
        let document: Document =
            serde_json::from_slice(bytes).map_err(|_| InvalidProviderConfig)?;
        let origin = document
            .origin
            .parse::<RegistryOrigin>()
            .map_err(|_| InvalidProviderConfig)?;
        if document.version != 1 || origin.as_str() != document.origin {
            return Err(InvalidProviderConfig);
        }
        for name in [
            &document.account_credential_env,
            &document.headless_credential_env,
        ]
        .into_iter()
        .flatten()
        {
            let mut chars = name.bytes();
            if name.len() > 128
                || !chars
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
                || !chars.all(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                return Err(InvalidProviderConfig);
            }
        }
        if document
            .account_credential_env
            .as_ref()
            .zip(document.headless_credential_env.as_ref())
            .is_some_and(|(account, headless)| account.eq_ignore_ascii_case(headless))
        {
            return Err(InvalidProviderConfig);
        }
        let CachePolicy::Disabled = document.cache;
        Ok(Self {
            origin,
            account: document.account_credential_env,
            headless: document.headless_credential_env,
        })
    }

    /// Authorize one exact inventory request before consulting a credential
    /// source. `consented` must come from a separate explicit disclosure action,
    /// never from sign-in success or public report visibility. Transport must
    /// disable redirects and invoke this gate for the exact destination origin.
    /// An existing credential needs no browser/identity refresh here; the service
    /// must independently enforce current authorization and fail closed on outage.
    pub fn authorize(
        &self,
        request: &SecurityRequest,
        consented: &[SecurityRequest],
        mode: CredentialMode,
        destination_origin: &str,
        lookup: &mut impl FnMut(&str) -> Option<String>,
    ) -> Result<Option<String>, RetrievalError> {
        if destination_origin != self.origin.as_str() || !consented.contains(request) {
            return Err(RetrievalError::NotAuthorized);
        }
        request.validate_v2().map_err(RetrievalError::Evidence)?;
        let variable = match mode {
            CredentialMode::Anonymous => {
                return if matches!(request.audience, tapid_protocol::EvidenceAudience::Public) {
                    Ok(None)
                } else {
                    Err(RetrievalError::NotAuthorized)
                };
            }
            CredentialMode::Account => &self.account,
            CredentialMode::Headless => &self.headless,
        };
        let token = variable
            .as_deref()
            .and_then(lookup)
            .ok_or(RetrievalError::NotAuthorized)?;
        // Conservative bearer-token alphabet excludes header injection.
        let body = token.trim_end_matches('=');
        if token.len() > 8192
            || body.is_empty()
            || !body
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-._~+/".contains(&c))
        {
            return Err(RetrievalError::NotAuthorized);
        }
        Ok(Some(token))
    }
}
