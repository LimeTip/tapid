//! Application results, independent of terminal or wire rendering.
use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

const MAX_DIAGNOSTIC_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ErrorKind {
    InvalidRequest,
    Project,
    Manifest,
    Lockfile,
    LockfileMissing,
    LockManifestMismatch,
    RegistryConfiguration,
    RegistryCredentialMissing,
    RegistryMetadata,
    RegistryTransport,
    Resolution,
    PeerDependency,
    Integrity,
    Archive,
    Store,
    StoreUnavailable,
    StoreBusy,
    ProjectBusy,
    Materialization,
    Transaction,
    Recovery,
    InvalidData,
}

impl ErrorKind {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::Project => "PROJECT_UNAVAILABLE",
            Self::Manifest => "MANIFEST_INVALID",
            Self::Lockfile => "LOCKFILE_INVALID",
            Self::LockfileMissing => "LOCKFILE_MISSING",
            Self::LockManifestMismatch => "LOCK_MANIFEST_MISMATCH",
            Self::RegistryConfiguration => "REGISTRY_CONFIGURATION_INVALID",
            Self::RegistryCredentialMissing => "REGISTRY_AUTH_MISSING",
            Self::RegistryMetadata => "REGISTRY_METADATA_INVALID",
            Self::RegistryTransport => "REGISTRY_TRANSPORT_FAILED",
            Self::Resolution => "RESOLUTION_FAILED",
            Self::PeerDependency => "PEER_DEPENDENCY_UNSATISFIED",
            Self::Integrity => "INTEGRITY_MISMATCH",
            Self::Archive => "ARCHIVE_INVALID",
            Self::Store => "STORE_FAILED",
            Self::StoreUnavailable => "STORE_CONTENT_UNAVAILABLE",
            Self::StoreBusy => "STORE_BUSY",
            Self::ProjectBusy => "PROJECT_BUSY",
            Self::Materialization => "MATERIALIZATION_FAILED",
            Self::Transaction => "TRANSACTION_FAILED",
            Self::Recovery => "RECOVERY_FAILED",
            Self::InvalidData => "INVALID_DATA",
        }
    }
}

/// URLs in untrusted metadata can carry userinfo, query, or fragment credentials. Strip
/// those values before limiting the diagnostic; classification uses typed kinds.
fn sanitize(message: &str) -> String {
    let mut result = String::new();
    let mut rest = message;
    while let Some(start) = rest
        .as_bytes()
        .windows(7)
        .enumerate()
        .find_map(|(index, prefix)| {
            (prefix.eq_ignore_ascii_case(b"http://")
                || prefix.eq_ignore_ascii_case(b"https:/")
                    && rest.as_bytes().get(index + 7) == Some(&b'/'))
            .then_some(index)
        })
    {
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '\"' | '\'' | '<' | '>'))
            .unwrap_or(rest.len());
        let url = &rest[..end];
        let prefix_len = if url.as_bytes()[..7].eq_ignore_ascii_case(b"https:/") {
            8
        } else {
            7
        };
        result.push_str(&url[..prefix_len]);
        let address = &url[prefix_len..];
        let authority_end = address.find(['/', '?', '#']).unwrap_or(address.len());
        let authority = &address[..authority_end];
        if let Some(at) = authority.rfind('@') {
            result.push_str("[redacted]@");
            result.push_str(&authority[at + 1..]);
        } else {
            result.push_str(authority);
        }
        let suffix = &address[authority_end..];
        if let Some(query) = suffix.find(['?', '#']) {
            result.push_str(&suffix[..query]);
            result.push_str(&suffix[query..query + 1]);
            result.push_str("[redacted]");
        } else {
            result.push_str(suffix);
        }
        rest = &rest[end..];
    }
    result.push_str(rest);
    result
}

fn bounded(message: String) -> String {
    let mut message = sanitize(&message);
    if message.len() > MAX_DIAGNOSTIC_BYTES {
        let mut end = MAX_DIAGNOSTIC_BYTES - 3;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push_str("...");
    }
    message
}

pub(crate) struct OperationalError {
    pub(crate) kind: ErrorKind,
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl OperationalError {
    pub(crate) fn new(kind: ErrorKind, message: impl fmt::Display) -> Self {
        Self {
            kind,
            message: bounded(message.to_string()),
            source: None,
        }
    }

    pub(crate) fn from_source(kind: ErrorKind, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            kind,
            message: bounded(source.to_string()),
            source: Some(Box::new(source)),
        }
    }

    pub(crate) fn context(mut self, context: impl fmt::Display) -> Self {
        self.message = bounded(format!("{context}: {}", self.message));
        self
    }
}

// Legacy private validation helpers still return text. Callers assign a more
// specific category at capability boundaries when the source is available.
impl From<String> for OperationalError {
    fn from(message: String) -> Self {
        Self::new(ErrorKind::InvalidData, message)
    }
}
impl From<&str> for OperationalError {
    fn from(message: &str) -> Self {
        Self::new(ErrorKind::InvalidData, message)
    }
}
impl From<tapid_resolver::ResolveError> for OperationalError {
    fn from(error: tapid_resolver::ResolveError) -> Self {
        let kind = match error {
            tapid_resolver::ResolveError::PeerDependency { .. } => ErrorKind::PeerDependency,
            _ => ErrorKind::Resolution,
        };
        Self::from_source(kind, error)
    }
}
impl From<tapid_lockfile::LockfileError> for OperationalError {
    fn from(error: tapid_lockfile::LockfileError) -> Self {
        let kind = match error {
            tapid_lockfile::LockfileError::RootManifestDigestMismatch { .. } => {
                ErrorKind::LockManifestMismatch
            }
            _ => ErrorKind::Lockfile,
        };
        Self::from_source(kind, error)
    }
}
impl From<tapid_registry_client::RegistryClientError> for OperationalError {
    fn from(error: tapid_registry_client::RegistryClientError) -> Self {
        let kind = match error {
            tapid_registry_client::RegistryClientError::Transport(_) => {
                ErrorKind::RegistryTransport
            }
            tapid_registry_client::RegistryClientError::Metadata(_) => ErrorKind::RegistryMetadata,
        };
        Self::from_source(kind, error)
    }
}
impl From<tapid_store::IngestError> for OperationalError {
    fn from(error: tapid_store::IngestError) -> Self {
        use tapid_store::IngestError;
        let kind = match &error {
            IngestError::DigestMismatch { .. } | IngestError::TreeDigestMismatch { .. } => {
                ErrorKind::Integrity
            }
            IngestError::Archive(_) => ErrorKind::Archive,
            IngestError::Io(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                ErrorKind::StoreBusy
            }
            IngestError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ErrorKind::StoreUnavailable
            }
            _ => ErrorKind::Store,
        };
        Self::from_source(kind, error)
    }
}
impl fmt::Debug for OperationalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperationalError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}
impl fmt::Display for OperationalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl Error for OperationalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChangeState {
    Unchanged,
    RolledBack,
    Committed,
    CommittedCleanupPending,
    RecoveryRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetryAdvice {
    AfterCorrection,
    AfterContention,
    DoNotRepeat,
    RecoverFirst,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Warning {
    UnverifiedRegistryArtifactsAllowed,
    PreviousTransactionRecovered,
}
impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnverifiedRegistryArtifactsAllowed => "npm artifacts without registry integrity are not authenticated against a registry-declared digest",
            Self::PreviousTransactionRecovered => "recovered an interrupted project transaction before this operation",
        })
    }
}

#[derive(Debug)]
pub(crate) struct OperationOutcome {
    pub(crate) project_dir: PathBuf,
    pub(crate) state: ChangeState,
    /// Project outputs actually changed, or requiring inspection after failed recovery.
    /// Shared-store effects are represented by state, not attributed to project paths.
    pub(crate) changed_files: Vec<PathBuf>,
    pub(crate) warnings: Vec<Warning>,
}
impl OperationOutcome {
    pub(crate) fn unchanged(project: &Path) -> Self {
        Self {
            project_dir: project.to_owned(),
            state: ChangeState::Unchanged,
            changed_files: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct OperationFailure {
    pub(crate) error: Box<OperationalError>,
    pub(crate) outcome: Box<OperationOutcome>,
    pub(crate) retry: RetryAdvice,
    pub(crate) recovery_error: Option<OperationalError>,
}
impl OperationFailure {
    pub(crate) fn new(
        error: OperationalError,
        mut outcome: OperationOutcome,
        recovery_error: Option<OperationalError>,
    ) -> Self {
        if outcome.state == ChangeState::RecoveryRequired {
            // An older transaction may fail before this attempt records mutations.
            // Recovery must inspect all durable project outputs, not report zero.
            for name in ["package.json", "tapid.lock", "node_modules"] {
                let path = outcome.project_dir.join(name);
                if !outcome.changed_files.contains(&path) {
                    outcome.changed_files.push(path);
                }
            }
        }
        let retry = match outcome.state {
            ChangeState::Committed | ChangeState::CommittedCleanupPending => {
                RetryAdvice::DoNotRepeat
            }
            ChangeState::RecoveryRequired => RetryAdvice::RecoverFirst,
            _ if matches!(error.kind, ErrorKind::ProjectBusy | ErrorKind::StoreBusy) => {
                RetryAdvice::AfterContention
            }
            _ => RetryAdvice::AfterCorrection,
        };
        Self {
            error: Box::new(error),
            outcome: Box::new(outcome),
            retry,
            recovery_error,
        }
    }
    pub(crate) fn unchanged(project: &Path, error: OperationalError) -> Self {
        Self::new(error, OperationOutcome::unchanged(project), None)
    }
}
impl fmt::Display for OperationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)?;
        if let Some(error) = &self.recovery_error {
            write!(f, "; recovery failed: {error}")?;
        }
        Ok(())
    }
}
impl Error for OperationFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.error.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_bearing_urls_are_redacted_in_display_and_debug() {
        for url in [
            "https://user:fixture-secret@registry.example/path?token=fixture-query-secret",
            "HTTPS://user:fixture-secret@registry.example/path#fixture-query-secret",
        ] {
            let error =
                OperationalError::from(tapid_lockfile::LockfileError::InvalidUrl(url.into()));
            for rendered in [error.to_string(), format!("{error:?}")] {
                assert!(!rendered.contains("fixture-secret"), "{rendered}");
                assert!(!rendered.contains("fixture-query-secret"), "{rendered}");
                assert!(rendered.contains("registry.example"));
            }
        }
    }

    #[test]
    fn diagnostics_are_bounded_at_utf8_character_boundaries() {
        let error = OperationalError::new(ErrorKind::RegistryMetadata, "界".repeat(4096));
        let message = error.to_string();
        assert!(message.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(message.ends_with("..."));
    }
}
