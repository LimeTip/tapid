use std::{fmt, str::FromStr};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageName(String);

impl PackageName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for PackageName {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || value.len() > 214
            || value.starts_with('.')
            || value.starts_with('_')
            || value.ends_with('.')
            || value.ends_with('_')
            || value.chars().any(char::is_whitespace)
        {
            return Err(DomainError::InvalidPackageName(value.to_owned()));
        }

        if value.starts_with('@') {
            let mut parts = value.split('/');
            let scope = parts.next().unwrap_or_default();
            let name = parts.next().unwrap_or_default();
            if parts.next().is_some()
                || scope.len() < 2
                || name.is_empty()
                || scope[1..].chars().any(|c| !is_name_character(c))
                || name.chars().any(|c| !is_name_character(c))
            {
                return Err(DomainError::InvalidPackageName(value.to_owned()));
            }
        } else if value.chars().any(|c| !is_name_character(c)) {
            return Err(DomainError::InvalidPackageName(value.to_owned()));
        }

        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A canonical SemVer package identity without build metadata.
///
/// Prerelease identifiers are preserved because npm packages may depend on an
/// exact prerelease. Build metadata is rejected because it does not participate
/// in SemVer precedence and would make registry identities ambiguous.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageVersion(semver::Version);

impl PackageVersion {
    /// Constructs a stable package version with no prerelease identifier.
    pub fn stable(major: u64, minor: u64, patch: u64) -> Self {
        Self(semver::Version::new(major, minor, patch))
    }

    /// Returns the SemVer major component.
    pub fn major(&self) -> u64 {
        self.0.major
    }

    /// Returns the SemVer minor component.
    pub fn minor(&self) -> u64 {
        self.0.minor
    }

    /// Returns the SemVer patch component.
    pub fn patch(&self) -> u64 {
        self.0.patch
    }

    /// Returns the canonical prerelease identifier sequence when present.
    pub fn prerelease(&self) -> Option<&str> {
        (!self.0.pre.is_empty()).then(|| self.0.pre.as_str())
    }
}

impl FromStr for PackageVersion {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let version = semver::Version::parse(value)
            .map_err(|_| DomainError::InvalidPackageVersion(value.to_owned()))?;
        if !version.build.is_empty() {
            return Err(DomainError::InvalidPackageVersion(value.to_owned()));
        }
        Ok(Self(version))
    }
}

impl fmt::Display for PackageVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ArtifactDigest(String);

impl ArtifactDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ArtifactDigest {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(hex) = value.strip_prefix("sha256-") else {
            return Err(DomainError::InvalidArtifactDigest(value.to_owned()));
        };
        if hex.len() != 64 || hex.chars().any(|c| !c.is_ascii_hexdigit()) {
            return Err(DomainError::InvalidArtifactDigest(value.to_owned()));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl fmt::Display for ArtifactDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RegistryOrigin(String);

impl RegistryOrigin {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RegistryOrigin {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim_end_matches('/');
        let valid = trimmed.starts_with("https://")
            && trimmed.len() > "https://".len()
            && !trimmed.contains(['@', '?', '#', '|'])
            && trimmed[8..]
                .split('/')
                .next()
                .is_some_and(|host| !host.is_empty());
        if !valid {
            return Err(DomainError::InvalidRegistryOrigin(value.to_owned()));
        }
        Ok(Self(trimmed.to_owned()))
    }
}

impl fmt::Display for RegistryOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageInstanceId {
    pub registry: RegistryOrigin,
    pub name: PackageName,
    pub version: PackageVersion,
}

impl PackageInstanceId {
    pub fn new(registry: RegistryOrigin, name: PackageName, version: PackageVersion) -> Self {
        Self {
            registry,
            name,
            version,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageIntegrity(String);

impl PackageIntegrity {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for PackageIntegrity {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(encoded) = value.strip_prefix("sha512-") else {
            return Err(DomainError::InvalidPackageIntegrity(value.to_owned()));
        };
        let decoded = if encoded.len() == 86 && !encoded.contains('=') {
            STANDARD_NO_PAD.decode(encoded)
        } else if encoded.len() == 88 && encoded.ends_with("==") {
            STANDARD.decode(encoded)
        } else {
            return Err(DomainError::InvalidPackageIntegrity(value.to_owned()));
        };
        let Ok(digest) = decoded else {
            return Err(DomainError::InvalidPackageIntegrity(value.to_owned()));
        };
        if digest.len() != 64 {
            return Err(DomainError::InvalidPackageIntegrity(value.to_owned()));
        }
        Ok(Self(format!("sha512-{}", STANDARD.encode(digest))))
    }
}

impl fmt::Display for PackageIntegrity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PeerContext(std::collections::BTreeMap<PackageName, PackageVersion>);

impl PeerContext {
    pub fn with(mut self, name: PackageName, version: PackageVersion) -> Self {
        self.0.insert(name, version);
        self
    }
    pub fn entries(&self) -> &std::collections::BTreeMap<PackageName, PackageVersion> {
        &self.0
    }
}

impl fmt::Display for PeerContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (name, version) in &self.0 {
            if !first {
                f.write_str(",")?;
            }
            first = false;
            write!(f, "{name}@{version}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlatformContext {
    pub os: Option<String>,
    pub cpu: Option<String>,
    pub libc: Option<String>,
}

impl PlatformContext {
    pub fn new(
        os: Option<&str>,
        cpu: Option<&str>,
        libc: Option<&str>,
    ) -> Result<Self, DomainError> {
        let context = Self {
            os: os.map(str::to_owned),
            cpu: cpu.map(str::to_owned),
            libc: libc.map(str::to_owned),
        };
        if [&context.os, &context.cpu, &context.libc]
            .into_iter()
            .flatten()
            .any(|v| {
                v.is_empty()
                    || v.chars().any(char::is_whitespace)
                    || v.chars().any(char::is_control)
                    || v == "-"
                    || v.contains('|')
            })
        {
            return Err(DomainError::InvalidPlatformContext);
        }
        Ok(context)
    }
}

impl fmt::Display for PlatformContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.os.is_none() && self.cpu.is_none() && self.libc.is_none() {
            return Ok(());
        }
        let values = [&self.os, &self.cpu, &self.libc];
        for (index, value) in values.into_iter().enumerate() {
            if index > 0 {
                f.write_str("-")?;
            }
            if let Some(value) = value {
                f.write_str(value)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainError {
    InvalidPackageName(String),
    InvalidPackageVersion(String),
    InvalidArtifactDigest(String),
    InvalidRegistryOrigin(String),
    InvalidPackageIntegrity(String),
    InvalidPlatformContext,
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPackageName(value) => write!(f, "invalid package name: {value}"),
            Self::InvalidPackageVersion(value) => write!(f, "invalid package version: {value}"),
            Self::InvalidArtifactDigest(value) => write!(f, "invalid artifact digest: {value}"),
            Self::InvalidRegistryOrigin(value) => write!(f, "invalid registry origin: {value}"),
            Self::InvalidPackageIntegrity(value) => write!(f, "invalid package integrity: {value}"),
            Self::InvalidPlatformContext => f.write_str("invalid platform context"),
        }
    }
}

impl std::error::Error for DomainError {}

fn is_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_scoped_and_unscoped_package_names() {
        assert!("tapid".parse::<PackageName>().is_ok());
        assert!("@tapid/core".parse::<PackageName>().is_ok());
    }

    #[test]
    fn rejects_unsafe_package_names() {
        for value in [
            "",
            "../tapid",
            "@tapid",
            "@tapid/core/extra",
            "tap id",
            "tapid/core",
        ] {
            assert!(value.parse::<PackageName>().is_err(), "accepted {value}");
        }
    }

    #[test]
    fn parses_canonical_versions() {
        let version = "1.2.3".parse::<PackageVersion>().unwrap();
        assert_eq!(version.to_string(), "1.2.3");
        assert!("01.2.3".parse::<PackageVersion>().is_err());
    }

    #[test]
    fn parses_canonical_prerelease_versions_without_build_metadata() {
        let version = "5.20260811.1-alpha".parse::<PackageVersion>().unwrap();
        assert_eq!(version.to_string(), "5.20260811.1-alpha");
        assert_eq!(version.prerelease(), Some("alpha"));
        assert!("1.2.3+build.1".parse::<PackageVersion>().is_err());
        assert!("1.2.3-01".parse::<PackageVersion>().is_err());
    }

    #[test]
    fn accepts_only_sha256_digests() {
        let digest = format!("sha256-{}", "A".repeat(64))
            .parse::<ArtifactDigest>()
            .unwrap();
        assert_eq!(digest.to_string(), format!("sha256-{}", "a".repeat(64)));
        assert!("sha512-deadbeef".parse::<ArtifactDigest>().is_err());
    }

    #[test]
    fn registry_origin_is_typed_and_canonical_without_secrets() {
        let origin = "https://REGISTRY.example.test/"
            .parse::<RegistryOrigin>()
            .unwrap();
        assert_eq!(origin.as_str(), "https://REGISTRY.example.test");
        assert!(
            "http://registry.example.test"
                .parse::<RegistryOrigin>()
                .is_err()
        );
        assert!(
            "https://user:pass@registry.example.test"
                .parse::<RegistryOrigin>()
                .is_err()
        );
        assert!(
            "https://registry.example.test/path|ambiguous"
                .parse::<RegistryOrigin>()
                .is_err()
        );
    }

    #[test]
    fn integrity_preserves_mixed_case_wire_encoding() {
        let value = "sha512-vjezHzaHfTgpmqTTye2FWJ751nFdp6l4EtqfRsd2sylZY73USlHKS75q67jhw5cb7uMi0xRAdd1MiTHAfaR9TA==".to_owned();
        let integrity = value.parse::<PackageIntegrity>().unwrap();
        assert_eq!(integrity.to_string(), value);
    }

    #[test]
    fn package_integrity_requires_canonical_base64_for_exactly_64_bytes() {
        let unpadded = format!("sha512-{}", "A".repeat(86));
        let padded = format!("{unpadded}==");
        assert_eq!(
            unpadded.parse::<PackageIntegrity>().unwrap().to_string(),
            padded
        );
        assert!(padded.parse::<PackageIntegrity>().is_ok());

        for invalid in [
            format!("sha512-{}", "A".repeat(88)),
            format!("sha512-{}=", "A".repeat(86)),
            format!("sha512-{}===", "A".repeat(85)),
        ] {
            assert!(
                invalid.parse::<PackageIntegrity>().is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn package_instance_identity_includes_registry() {
        let name: PackageName = "tapid".parse().unwrap();
        let version: PackageVersion = "1.0.0".parse().unwrap();
        let first = PackageInstanceId::new(
            "https://one.example".parse().unwrap(),
            name.clone(),
            version.clone(),
        );
        let second = PackageInstanceId::new("https://two.example".parse().unwrap(), name, version);
        assert_ne!(first, second);
    }

    #[test]
    fn contexts_have_deterministic_empty_and_nonempty_forms() {
        let peer = PeerContext::default().with("react".parse().unwrap(), "18.2.0".parse().unwrap());
        assert_eq!(peer.to_string(), "react@18.2.0");
        let platform = PlatformContext::new(Some("linux"), Some("x86_64"), Some("gnu")).unwrap();
        assert_eq!(platform.to_string(), "linux-x86_64-gnu");
        assert!(PlatformContext::new(Some("linux-x"), None, None).is_ok());
        assert!(PlatformContext::new(Some("-"), None, None).is_err());
        assert!(PlatformContext::new(Some("linux|custom"), None, None).is_err());
        assert!(PlatformContext::new(Some("linux\u{0000}"), None, None).is_err());
        assert!(PlatformContext::new(Some("linux\u{0007}"), None, None).is_err());
        assert_eq!(
            PlatformContext::new(None, Some("x86_64"), None)
                .unwrap()
                .to_string(),
            "-x86_64-"
        );
        assert_eq!(
            PlatformContext::new(Some("linux"), None, Some("gnu"))
                .unwrap()
                .to_string(),
            "linux--gnu"
        );
    }
}
