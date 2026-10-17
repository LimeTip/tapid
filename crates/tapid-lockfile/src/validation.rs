use crate::LockfileError;
use std::str::FromStr;
use tapid_core::RegistryOrigin;

pub(crate) fn validate_registry_url(value: &str) -> Result<(), LockfileError> {
    if value.contains('?') || value.contains('#') || value.contains('@') {
        return Err(LockfileError::InvalidUrl(value.to_owned()));
    }
    RegistryOrigin::from_str(value)
        .map(|_| ())
        .map_err(|_| LockfileError::InvalidUrl(value.to_owned()))
}

pub(crate) fn validate_artifact_url(value: &str) -> Result<(), LockfileError> {
    value
        .parse::<tapid_core::ArtifactUrl>()
        .map(|_| ())
        .map_err(|_| LockfileError::InvalidUrl(value.to_owned()))
}
