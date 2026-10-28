use crate::FetchMode;
use std::fmt;

#[derive(Clone, Eq, PartialEq)]
pub enum MetadataError {
    InvalidRegistry(String),
    InvalidPackageName(String),
    InvalidVersion(String),
    InvalidIntegrity(String),
    InvalidArtifact(String),
    InvalidDependency(String),
    UnsupportedIntegrity(String),
    InvalidJson(String),
    MissingField(String),
    ConflictingField(String),
    DuplicateVersion(String),
    HttpStatus(u16),
    UnsupportedContentType(String),
}
// Payloads originate in untrusted metadata and may contain credential-bearing
// URLs or response text. Keep structured fields available to callers, but never
// interpolate them into diagnostics (including nested Debug implementations).
impl fmt::Debug for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::InvalidRegistry(_) => "InvalidRegistry",
            Self::InvalidPackageName(_) => "InvalidPackageName",
            Self::InvalidVersion(_) => "InvalidVersion",
            Self::InvalidIntegrity(_) => "InvalidIntegrity",
            Self::InvalidArtifact(_) => "InvalidArtifact",
            Self::InvalidDependency(_) => "InvalidDependency",
            Self::UnsupportedIntegrity(_) => "UnsupportedIntegrity",
            Self::InvalidJson(_) => "InvalidJson",
            Self::MissingField(_) => "MissingField",
            Self::ConflictingField(_) => "ConflictingField",
            Self::DuplicateVersion(_) => "DuplicateVersion",
            Self::UnsupportedContentType(_) => "UnsupportedContentType",
            Self::HttpStatus(status) => return f.debug_tuple("HttpStatus").field(status).finish(),
        };
        f.write_str(category)
    }
}
impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MetadataError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
    InvalidUrl(String),
    OriginNotAllowed(String),
    Http(String),
    TooLarge { limit: usize },
    InvalidResponse(String),
}
impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for TransportError {}

#[derive(Debug)]
pub enum ClientError<E> {
    Transport(E),
    Metadata(MetadataError),
    NetworkDisabled(FetchMode),
}
impl<E: fmt::Display> fmt::Display for ClientError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "registry transport failed: {e}"),
            Self::Metadata(e) => write!(f, "invalid registry metadata: {e}"),
            Self::NetworkDisabled(m) => write!(f, "network disabled in {m:?} mode"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for ClientError<E> {}

#[derive(Debug)]
pub enum RegistryClientError {
    Transport(TransportError),
    Metadata(MetadataError),
}
impl fmt::Display for RegistryClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for RegistryClientError {}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn metadata_error_rendering_does_not_expose_untrusted_payloads() {
        for error in [
            MetadataError::InvalidRegistry(
                "https://user:metadata-secret-sentinel@registry.test".into(),
            ),
            MetadataError::InvalidArtifact(
                "https://user:metadata-secret-sentinel@registry.test/archive".into(),
            ),
            MetadataError::InvalidJson("remote metadata-secret-sentinel response".into()),
            MetadataError::MissingField("versions.metadata-secret-sentinel".into()),
            MetadataError::UnsupportedContentType("metadata-secret-sentinel".into()),
            MetadataError::InvalidDependency("metadata-secret-sentinel".into()),
        ] {
            let rendered = format!("{error:?} {error}");
            assert!(!rendered.contains("metadata-secret-sentinel"), "{rendered}");
            let wrapped = RegistryClientError::Metadata(error);
            assert!(!format!("{wrapped:?} {wrapped}").contains("metadata-secret-sentinel"));
        }
        assert_eq!(
            MetadataError::HttpStatus(401).to_string(),
            "HttpStatus(401)"
        );
    }
}
