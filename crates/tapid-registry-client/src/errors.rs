use crate::FetchMode;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
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
