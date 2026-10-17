//! Canonical artifact references shared by transport and persisted selections.
use crate::DomainError;
use std::{
    fmt,
    str::{self, FromStr},
};
use url::Url;

/// A canonical HTTPS artifact URL without credentials, queries, or fragments.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactUrl(String);
impl ArtifactUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl FromStr for ArtifactUrl {
    type Err = DomainError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let url = Url::parse(value).map_err(|_| DomainError::InvalidArtifactUrl)?;
        if !canonical_url(value, &url) {
            return Err(DomainError::InvalidArtifactUrl);
        }
        Ok(Self(value.to_owned()))
    }
}
impl fmt::Display for ArtifactUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

fn canonical_url(raw: &str, url: &Url) -> bool {
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
