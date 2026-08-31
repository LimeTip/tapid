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
        .split_once("://")
        .map(|(_, remainder)| remainder)
        .and_then(|remainder| remainder.split(['/', '?', '#']).next())
        .is_some_and(|authority| authority.contains('@'));
    if !value.is_ascii()
        || !value.starts_with("https://")
        || parsed.as_str() != value
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
        || value.contains('\\')
        || path_has_noncanonical_percent_encoding(parsed.path())
        || parsed.scheme() != "https"
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
