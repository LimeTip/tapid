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
    description: Option<String>,
    license: Option<String>,
    dependencies: BTreeMap<String, String>,
    dev_dependencies: BTreeMap<String, String>,
    optional_dependencies: BTreeMap<String, String>,
    peer_dependencies: BTreeMap<String, String>,
    scripts: BTreeMap<String, String>,
}

impl PackageManifest {
    /// Creates a manifest with no dependencies.
    pub fn new(name: &str, version: &str, private: bool) -> Result<Self, ManifestError> {
        Ok(Self {
            name: PackageName::from_str(name).map_err(ManifestError::InvalidPackageName)?,
            version: PackageVersion::from_str(version)
                .map_err(ManifestError::InvalidPackageVersion)?,
            private,
            description: None,
            license: None,
            dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
            optional_dependencies: BTreeMap::new(),
            peer_dependencies: BTreeMap::new(),
            scripts: BTreeMap::new(),
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
            manifest.dependencies = parse_string_map(dependencies, "dependencies", true)?;
        }
        if let Some(dependencies) = object.get("devDependencies") {
            manifest.dev_dependencies = parse_string_map(dependencies, "devDependencies", true)?;
        }
        if let Some(dependencies) = object.get("optionalDependencies") {
            manifest.optional_dependencies =
                parse_string_map(dependencies, "optionalDependencies", true)?;
        }
        if let Some(dependencies) = object.get("peerDependencies") {
            manifest.peer_dependencies = parse_string_map(dependencies, "peerDependencies", true)?;
        }
        if let Some(scripts) = object.get("scripts") {
            manifest.scripts = parse_string_map(scripts, "scripts", false)?;
        }
        manifest.description = optional_string(object, "description")?;
        manifest.license = optional_string(object, "license")?;

        Ok(manifest)
    }

    /// Serializes the supported fields in a stable, human-readable format.
    pub fn to_json(&self) -> String {
        let value = ManifestDocument {
            name: self.name.to_string(),
            version: self.version.to_string(),
            private: self.private,
            description: self.description.as_deref(),
            license: self.license.as_deref(),
            dependencies: (!self.dependencies.is_empty()).then_some(&self.dependencies),
            dev_dependencies: (!self.dev_dependencies.is_empty()).then_some(&self.dev_dependencies),
            optional_dependencies: (!self.optional_dependencies.is_empty())
                .then_some(&self.optional_dependencies),
            peer_dependencies: (!self.peer_dependencies.is_empty())
                .then_some(&self.peer_dependencies),
            scripts: (!self.scripts.is_empty()).then_some(&self.scripts),
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

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    pub fn dependencies(&self) -> &BTreeMap<String, String> {
        &self.dependencies
    }

    pub fn dev_dependencies(&self) -> &BTreeMap<String, String> {
        &self.dev_dependencies
    }

    pub fn optional_dependencies(&self) -> &BTreeMap<String, String> {
        &self.optional_dependencies
    }

    pub fn peer_dependencies(&self) -> &BTreeMap<String, String> {
        &self.peer_dependencies
    }

    pub fn scripts(&self) -> &BTreeMap<String, String> {
        &self.scripts
    }
}

#[derive(Serialize)]
struct ManifestDocument<'a> {
    name: String,
    version: String,
    private: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependencies: Option<&'a BTreeMap<String, String>>,
    #[serde(rename = "devDependencies", skip_serializing_if = "Option::is_none")]
    dev_dependencies: Option<&'a BTreeMap<String, String>>,
    #[serde(
        rename = "optionalDependencies",
        skip_serializing_if = "Option::is_none"
    )]
    optional_dependencies: Option<&'a BTreeMap<String, String>>,
    #[serde(rename = "peerDependencies", skip_serializing_if = "Option::is_none")]
    peer_dependencies: Option<&'a BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scripts: Option<&'a BTreeMap<String, String>>,
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

fn optional_string(
    object: &serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<Option<String>, ManifestError> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or(ManifestError::ExpectedString(key)),
    }
}

fn parse_string_map(
    value: &Value,
    field: &'static str,
    validate_names: bool,
) -> Result<BTreeMap<String, String>, ManifestError> {
    let object = value.as_object().ok_or(ManifestError::ExpectedMap(field))?;
    object
        .iter()
        .map(|(name, version)| {
            if validate_names {
                PackageName::from_str(name).map_err(ManifestError::InvalidDependencyName)?;
            }
            let version = version
                .as_str()
                .ok_or(ManifestError::ExpectedMapValueString {
                    field,
                    key: name.clone(),
                })?;
            Ok((name.clone(), version.to_owned()))
        })
        .collect()
}

#[derive(Debug)]
pub enum ManifestError {
    InvalidJson(serde_json::Error),
    RootMustBeObject,
    RequiredString(&'static str),
    ExpectedString(&'static str),
    ExpectedBoolean(&'static str),
    ExpectedMap(&'static str),
    ExpectedMapValueString { field: &'static str, key: String },
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
            Self::ExpectedString(key) => write!(f, "package.json field '{key}' must be a string"),
            Self::ExpectedBoolean(key) => write!(f, "package.json field '{key}' must be a boolean"),
            Self::ExpectedMap(key) => {
                write!(f, "package.json field '{key}' must be an object")
            }
            Self::ExpectedMapValueString { field, key } => {
                write!(
                    f,
                    "package.json field '{field}' entry '{key}' must be a string"
                )
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
    fn parses_development_optional_peer_and_script_metadata() {
        let manifest = PackageManifest::parse(
            r#"{
                "name": "example-app",
                "version": "1.2.3",
                "description": "An example application",
                "license": "MIT",
                "devDependencies": {"rustc": "^1.85.0"},
                "optionalDependencies": {"fsevents": "^2.3.3"},
                "peerDependencies": {"react": ">=18"},
                "scripts": {"test": "cargo test", "build": "cargo build"}
            }"#,
        )
        .unwrap();

        assert_eq!(manifest.description(), Some("An example application"));
        assert_eq!(manifest.license(), Some("MIT"));
        assert_eq!(manifest.dev_dependencies()["rustc"], "^1.85.0");
        assert_eq!(manifest.optional_dependencies()["fsevents"], "^2.3.3");
        assert_eq!(manifest.peer_dependencies()["react"], ">=18");
        assert_eq!(manifest.scripts()["test"], "cargo test");
    }

    #[test]
    fn rejects_non_string_metadata_and_script_values() {
        assert!(
            PackageManifest::parse(r#"{"name":"app","version":"1.0.0","description":false}"#)
                .is_err()
        );
        assert!(
            PackageManifest::parse(r#"{"name":"app","version":"1.0.0","scripts":{"test":true}}"#)
                .is_err()
        );
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
