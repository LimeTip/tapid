//! Parsing and validation for npm-compatible `package.json` manifests.

#![deny(unsafe_code)]

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::Serialize;
use serde_json::Value;
use tapid_core::{PackageName, PackageVersion};

/// Returns the current crate version.
///
/// This small API keeps the initial scaffold non-empty while the crate
/// boundary is being established.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The minimal project metadata required by Tapid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageManifest {
    name: PackageName,
    version: PackageVersion,
    private: bool,
    dependencies: BTreeMap<String, String>,
}

impl PackageManifest {
    /// Creates a manifest with no dependencies.
    pub fn new(name: &str, version: &str, private: bool) -> Result<Self, ManifestError> {
        Ok(Self {
            name: PackageName::from_str(name).map_err(ManifestError::InvalidPackageName)?,
            version: PackageVersion::from_str(version)
                .map_err(ManifestError::InvalidPackageVersion)?,
            private,
            dependencies: BTreeMap::new(),
        })
    }

    /// Parses and validates a UTF-8 `package.json` document.
    pub fn parse(input: &str) -> Result<Self, ManifestError> {
        let value: Value = serde_json::from_str(input).map_err(ManifestError::InvalidJson)?;
        let object = value.as_object().ok_or(ManifestError::RootMustBeObject)?;

        let name = required_string(object, "name")?;
        let version = required_string(object, "version")?;
        let mut manifest = Self::new(name, version, optional_bool(object, "private")?)?;

        if let Some(dependencies) = object.get("dependencies") {
            manifest.dependencies = parse_dependency_map(dependencies)?;
        }

        Ok(manifest)
    }

    /// Serializes the supported fields in a stable, human-readable format.
    pub fn to_json(&self) -> String {
        let value = ManifestDocument {
            name: self.name.to_string(),
            version: self.version.to_string(),
            private: self.private,
            dependencies: (!self.dependencies.is_empty()).then_some(&self.dependencies),
        };
        format!(
            "{}\n",
            serde_json::to_string_pretty(&value).expect("manifest is serializable")
        )
    }

    pub fn name(&self) -> &PackageName {
        &self.name
    }

    pub fn version(&self) -> PackageVersion {
        self.version
    }

    pub fn is_private(&self) -> bool {
        self.private
    }

    pub fn dependencies(&self) -> &BTreeMap<String, String> {
        &self.dependencies
    }
}

#[derive(Serialize)]
struct ManifestDocument<'a> {
    name: String,
    version: String,
    private: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependencies: Option<&'a BTreeMap<String, String>>,
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<&'a str, ManifestError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ManifestError::RequiredString(key))
}

fn optional_bool(
    object: &serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<bool, ManifestError> {
    match object.get(key) {
        None => Ok(false),
        Some(value) => value.as_bool().ok_or(ManifestError::ExpectedBoolean(key)),
    }
}

fn parse_dependency_map(value: &Value) -> Result<BTreeMap<String, String>, ManifestError> {
    let object = value
        .as_object()
        .ok_or(ManifestError::ExpectedDependencyMap("dependencies"))?;
    object
        .iter()
        .map(|(name, version)| {
            PackageName::from_str(name).map_err(ManifestError::InvalidDependencyName)?;
            let version = version
                .as_str()
                .ok_or(ManifestError::DependencySpecMustBeString(name.clone()))?;
            Ok((name.clone(), version.to_owned()))
        })
        .collect()
}

#[derive(Debug)]
pub enum ManifestError {
    InvalidJson(serde_json::Error),
    RootMustBeObject,
    RequiredString(&'static str),
    ExpectedBoolean(&'static str),
    ExpectedDependencyMap(&'static str),
    DependencySpecMustBeString(String),
    InvalidPackageName(tapid_core::DomainError),
    InvalidPackageVersion(tapid_core::DomainError),
    InvalidDependencyName(tapid_core::DomainError),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(f, "invalid package.json: {error}"),
            Self::RootMustBeObject => write!(f, "package.json root must be an object"),
            Self::RequiredString(key) => write!(f, "package.json field '{key}' must be a string"),
            Self::ExpectedBoolean(key) => write!(f, "package.json field '{key}' must be a boolean"),
            Self::ExpectedDependencyMap(key) => {
                write!(f, "package.json field '{key}' must be an object")
            }
            Self::DependencySpecMustBeString(name) => {
                write!(f, "dependency '{name}' specification must be a string")
            }
            Self::InvalidPackageName(error)
            | Self::InvalidDependencyName(error)
            | Self::InvalidPackageVersion(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ManifestError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_present() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn parses_minimal_package_manifest() {
        let manifest = PackageManifest::parse(
            r#"{
                "name": "example-app",
                "version": "1.2.3",
                "private": true,
                "dependencies": {"kleur": "^4.1.5"}
            }"#,
        )
        .unwrap();

        assert_eq!(manifest.name().as_str(), "example-app");
        assert_eq!(manifest.version().to_string(), "1.2.3");
        assert!(manifest.is_private());
        assert_eq!(
            manifest.dependencies().get("kleur"),
            Some(&"^4.1.5".to_owned())
        );
    }

    #[test]
    fn rejects_missing_name_and_invalid_version() {
        assert!(PackageManifest::parse(r#"{"version":"1.0.0"}"#).is_err());
        assert!(PackageManifest::parse(r#"{"name":"app","version":"1"}"#).is_err());
    }

    #[test]
    fn renders_a_deterministic_minimal_manifest() {
        let manifest = PackageManifest::new("example-app", "0.1.0", true).unwrap();
        assert_eq!(
            manifest.to_json(),
            "{\n  \"name\": \"example-app\",\n  \"version\": \"0.1.0\",\n  \"private\": true\n}\n"
        );
    }
}
