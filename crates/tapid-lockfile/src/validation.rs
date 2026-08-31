use crate::LockfileError;
use std::str::FromStr;
use tapid_core::RegistryOrigin;
use url::Url;

/// Validates a registry URL without query parameters, fragments, or userinfo.
///
/// # Examples
///
/// ```
/// assert!(validate_registry_url("https://registry.example.com").is_ok());
/// assert!(validate_registry_url("https://registry.example.com?channel=stable").is_err());
/// ```
///
/// # Errors
///
/// Returns [`LockfileError::InvalidUrl`] if the value is malformed or contains
/// a query, fragment, or userinfo.
pub(crate) fn validate_registry_url(value: &str) -> Result<(), LockfileError> {
    if value.contains('?') || value.contains('#') || value.contains('@') {
        return Err(LockfileError::InvalidUrl(value.to_owned()));
    }
    RegistryOrigin::from_str(value)
        .map(|_| ())
        .map_err(|_| LockfileError::InvalidUrl(value.to_owned()))
}

/// Validates that an artifact URL is an HTTPS URL with a host and no userinfo, query, or fragment.
///
/// # Errors
///
/// Returns [`LockfileError::InvalidUrl`] if the value is malformed or does not meet the
/// artifact URL requirements.
///
/// # Examples
///
/// ```
/// assert!(validate_artifact_url("https://example.com/artifact.tar").is_ok());
/// ```
pub(crate) fn validate_artifact_url(value: &str) -> Result<(), LockfileError> {
pub(crate) fn validate_artifact_url(value: &str) -> Result<(), LockfileError> {
    let parsed = Url::parse(value).map_err(|_| LockfileError::InvalidUrl(value.to_owned()))?;
    let authority_has_userinfo = value
        .strip_prefix("https://")
        .and_then(|remainder| remainder.split('/').next())
        .is_some_and(|authority| authority.contains('@'));
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || authority_has_userinfo
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(LockfileError::InvalidUrl(value.to_owned()));
    }
    Ok(())
}
