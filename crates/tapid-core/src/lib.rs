use std::{fmt, str::FromStr};

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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl FromStr for PackageVersion {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split('.');
        let numbers = [parts.next(), parts.next(), parts.next()];
        if parts.next().is_some() || numbers.iter().any(Option::is_none) {
            return Err(DomainError::InvalidPackageVersion(value.to_owned()));
        }

        let [Some(major), Some(minor), Some(patch)] = numbers else {
            unreachable!("checked above");
        };
        let parse = |part: &str| {
            if part.is_empty() || (part.len() > 1 && part.starts_with('0')) {
                return Err(DomainError::InvalidPackageVersion(value.to_owned()));
            }
            part.parse::<u64>()
                .map_err(|_| DomainError::InvalidPackageVersion(value.to_owned()))
        };

        Ok(Self {
            major: parse(major)?,
            minor: parse(minor)?,
            patch: parse(patch)?,
        })
    }
}

impl fmt::Display for PackageVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainError {
    InvalidPackageName(String),
    InvalidPackageVersion(String),
    InvalidArtifactDigest(String),
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPackageName(value) => write!(f, "invalid package name: {value}"),
            Self::InvalidPackageVersion(value) => write!(f, "invalid package version: {value}"),
            Self::InvalidArtifactDigest(value) => write!(f, "invalid artifact digest: {value}"),
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
    fn accepts_only_sha256_digests() {
        let digest = format!("sha256-{}", "A".repeat(64))
            .parse::<ArtifactDigest>()
            .unwrap();
        assert_eq!(digest.to_string(), format!("sha256-{}", "a".repeat(64)));
        assert!("sha512-deadbeef".parse::<ArtifactDigest>().is_err());
    }
}
