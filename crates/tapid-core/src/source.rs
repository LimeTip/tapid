use crate::{ArtifactDigest, DomainError, RegistryOrigin};
use std::{fmt, str::FromStr};

/// Exact origin of a copied package artifact. Registry origins remain separately
/// validated so callers cannot pass a file or Git source to an HTTP registry client.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageSource {
    identity: String,
    registry: Option<RegistryOrigin>,
}

impl PackageSource {
    pub fn as_str(&self) -> &str {
        &self.identity
    }
    pub fn registry(&self) -> Option<&RegistryOrigin> {
        self.registry.as_ref()
    }
    pub fn file(path: &str, digest: ArtifactDigest) -> Result<Self, DomainError> {
        format!("file:{path}#{digest}").parse()
    }
    pub fn git(
        repository: &str,
        commit: &str,
        digest: ArtifactDigest,
    ) -> Result<Self, DomainError> {
        format!("git+{repository}#{commit}!{digest}").parse()
    }
    pub fn file_parts(&self) -> Option<(&str, &str)> {
        self.identity.strip_prefix("file:")?.rsplit_once('#')
    }
    pub fn git_parts(&self) -> Option<(&str, &str, &str)> {
        let (repository, pin) = self.identity.strip_prefix("git+")?.rsplit_once('#')?;
        let (commit, digest) = pin.split_once('!')?;
        Some((repository, commit, digest))
    }
    pub fn artifact_digest(&self) -> Option<&str> {
        self.file_parts()
            .map(|(_, digest)| digest)
            .or_else(|| self.git_parts().map(|(_, _, digest)| digest))
    }
}

impl From<RegistryOrigin> for PackageSource {
    fn from(registry: RegistryOrigin) -> Self {
        Self {
            identity: registry.to_string(),
            registry: Some(registry),
        }
    }
}
impl fmt::Display for PackageSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.identity.fmt(f)
    }
}
impl FromStr for PackageSource {
    type Err = DomainError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || DomainError::InvalidPackageSource;
        if let Some(rest) = value.strip_prefix("file:") {
            let (path, digest) = rest.rsplit_once('#').ok_or_else(invalid)?;
            if path.is_empty()
                || path.starts_with('/')
                || path.contains(['\\', ':', '|', '#'])
                || path.chars().any(char::is_control)
                || path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err(invalid());
            }
            let parsed: ArtifactDigest = digest.parse().map_err(|_| invalid())?;
            if parsed.as_str() != digest {
                return Err(invalid());
            }
            return Ok(Self {
                identity: value.into(),
                registry: None,
            });
        }
        if let Some(rest) = value.strip_prefix("git+") {
            let (repository, pin) = rest.rsplit_once('#').ok_or_else(invalid)?;
            let (commit, digest) = pin.split_once('!').ok_or_else(invalid)?;
            let parsed_repository: GitRepository = repository.parse().map_err(|_| invalid())?;
            if parsed_repository.as_str() != repository
                || !matches!(commit.len(), 40 | 64)
                || !commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(invalid());
            }
            let parsed: ArtifactDigest = digest.parse().map_err(|_| invalid())?;
            if parsed.as_str() != digest {
                return Err(invalid());
            }
            return Ok(Self {
                identity: value.into(),
                registry: None,
            });
        }
        let registry: RegistryOrigin = value.parse()?;
        Ok(registry.into())
    }
}

impl PartialEq<RegistryOrigin> for PackageSource {
    fn eq(&self, other: &RegistryOrigin) -> bool {
        self.registry() == Some(other)
    }
}
impl PartialEq<PackageSource> for RegistryOrigin {
    fn eq(&self, other: &PackageSource) -> bool {
        other == self
    }
}

/// Credential-free canonical HTTPS repository address.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitRepository(String);
impl GitRepository {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for GitRepository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for GitRepository {
    type Err = DomainError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || DomainError::InvalidPackageSource;
        let url = url::Url::parse(value).map_err(|_| invalid())?;
        let authority = value
            .split_once("://")
            .map(|(_, rest)| rest.split('/').next().unwrap_or_default())
            .unwrap_or_default();
        if url.scheme() != "https"
            || url.host_str().is_none()
            || url.path() == "/"
            || !url.username().is_empty()
            || url.password().is_some()
            || authority.contains('@')
            || url.query().is_some()
            || url.fragment().is_some()
            || value.contains(['|', '#', '!'])
            || value.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        Ok(Self(url.to_string()))
    }
}
