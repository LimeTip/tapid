use std::fmt;

use tapid_core::DomainError;

#[derive(Debug)]
pub enum LockfileError {
    Serialization(serde_json::Error),
    Domain(DomainError),
    InvalidUrl(String),
    InvalidSha512(String),
    InvalidWorkspaceSource(String),
    UnsupportedVersion(u32),
    RegenerationRequired(u32),
    DuplicatePackage(String),
    PackageKeyMismatch(String),
    /// Persisted registry spelling would change identity when parsed.
    NonCanonicalRegistryIdentity,
    InvalidPackageKey(String),
    DanglingDependency {
        package: String,
        dependency: String,
    },
    DanglingRoot(String),
    MissingRoots,
    WorkspaceIdentityRequiresCurrentVersion,
    MissingRegistryIntegrityProvenance(String),
    UnverifiedRegistryArtifact(String),
    NonCanonicalRoots,
    DependencyNameMismatch {
        package: String,
        dependency: String,
        target: String,
    },
    SelfDependency(String),
    RootManifestDigestMismatch {
        expected: String,
        actual: String,
    },
}

impl fmt::Display for LockfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialization(error) => write!(f, "invalid lockfile JSON: {error}"),
            Self::Domain(error) => error.fmt(f),
            Self::InvalidUrl(value) => {
                write!(f, "lockfile URL is not an approved HTTPS origin: {value}")
            }
            Self::InvalidSha512(value) => write!(f, "invalid SHA-512 integrity: {value}"),
            Self::InvalidWorkspaceSource(value) => {
                write!(f, "invalid local workspace source identity: {value}")
            }
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported lockfile version: {version}")
            }
            Self::RegenerationRequired(version) => write!(
                f,
                "lockfile version {version} lacks required integrity provenance; regenerate it online"
            ),
            Self::DuplicatePackage(key) => write!(f, "duplicate locked package: {key}"),
            Self::PackageKeyMismatch(key) => {
                write!(f, "lockfile package key does not match package: {key}")
            }
            Self::NonCanonicalRegistryIdentity => write!(
                f,
                "noncanonical persisted registry identity; replay is refused without rekeying; preserve a backup of tapid.lock, then deliberately re-resolve online with `tapid install` (without --offline or --frozen) and review the new graph; see docs/compatibility.md"
            ),
            Self::InvalidPackageKey(key) => {
                write!(f, "invalid canonical lockfile package key: {key}")
            }
            Self::DanglingDependency {
                package,
                dependency,
            } => {
                write!(f, "package {package} has dangling dependency {dependency}")
            }
            Self::DanglingRoot(root) => write!(f, "lockfile has dangling root package {root}"),
            Self::MissingRoots => write!(f, "current lockfile schema requires exact root packages"),
            Self::WorkspaceIdentityRequiresCurrentVersion => write!(
                f,
                "workspace package identities require the current lockfile schema"
            ),
            Self::MissingRegistryIntegrityProvenance(package) => write!(
                f,
                "current lockfile schema requires registry integrity provenance for {package}"
            ),
            Self::UnverifiedRegistryArtifact(package) => write!(
                f,
                "lockfile package lacks registry-declared artifact integrity: {package}"
            ),
            Self::NonCanonicalRoots => {
                write!(f, "lockfile root packages must be sorted and unique")
            }
            Self::DependencyNameMismatch {
                package,
                dependency,
                target,
            } => write!(
                f,
                "package {package} dependency {dependency} targets package {target}"
            ),
            Self::SelfDependency(key) => write!(f, "package cannot depend on itself: {key}"),
            Self::RootManifestDigestMismatch { expected, actual } => write!(
                f,
                "lockfile root manifest digest mismatch: expected {expected}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for LockfileError {}
