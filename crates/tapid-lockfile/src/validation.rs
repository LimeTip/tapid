use crate::LockfileError;
use std::str::FromStr;
use tapid_core::RegistryOrigin;
use url::Url;

pub(crate) fn validate_registry_url(value: &str) -> Result<(), LockfileError> {
    if value.contains('?') || value.contains('#') || value.contains('@') {
        return Err(LockfileError::InvalidUrl(value.to_owned()));
    }
    RegistryOrigin::from_str(value)
        .map(|_| ())
        .map_err(|_| LockfileError::InvalidUrl(value.to_owned()))
}

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
