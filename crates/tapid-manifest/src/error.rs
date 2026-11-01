use std::fmt;

/// Field evidence for a parse failure. Observations are JSON types, never values.
#[derive(Debug, Eq, PartialEq)]
pub struct ManifestDiagnostic {
    pub field: Option<&'static str>,
    pub key: Option<String>,
    pub expected: &'static str,
    pub observed: &'static str,
}

/// Workspace discovery retains manifest failure paths and parse evidence for callers.
#[derive(Debug)]
pub enum WorkspaceError {
    Discovery(String),
    Selection(String),
    DiscoveryLimitExceeded,
    ManifestRead {
        path: std::path::PathBuf,
        message: String,
    },
    Manifest {
        /// True when parsing a declared member during owner validation.
        member: bool,
        path: std::path::PathBuf,
        diagnostic: Box<ManifestDiagnostic>,
        error: Box<ManifestError>,
    },
}

impl From<String> for WorkspaceError {
    fn from(message: String) -> Self {
        Self::Discovery(message)
    }
}
impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(message) | Self::Selection(message) => f.write_str(message),
            Self::ManifestRead { message, .. } => f.write_str(message),
            Self::DiscoveryLimitExceeded => f.write_str("workspace discovery limit exceeded"),
            Self::Manifest { path, error, .. } => {
                write!(f, "invalid workspace manifest {}: {error}", path.display())
            }
        }
    }
}
impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Manifest { error, .. } => Some(error.as_ref()),
            Self::Discovery(_)
            | Self::Selection(_)
            | Self::DiscoveryLimitExceeded
            | Self::ManifestRead { .. } => None,
        }
    }
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
    DependencyNotDeclared { field: &'static str, name: String },
    InvalidBin { command: String, target: String },
    ExpectedBinString(String),
}

impl ManifestError {
    /// Locate the failed rule in the same input passed to `PackageManifest::parse`.
    /// No manifest or script contents are copied into the diagnostic.
    pub fn diagnostic(&self, input: &str) -> ManifestDiagnostic {
        use ManifestError::*;
        let value = serde_json::from_str::<serde_json::Value>(input).ok();
        let (field, key, expected) = match self {
            InvalidJson(_) => (None, None, "valid_json"),
            RootMustBeObject => (None, None, "object"),
            RequiredString(field) | ExpectedString(field) => (Some(*field), None, "string"),
            ExpectedBoolean(field) => (Some(*field), None, "boolean"),
            ExpectedMap(field) => (Some(*field), None, "object"),
            ExpectedMapValueString { field, key } => (Some(*field), Some(key.clone()), "string"),
            InvalidPackageName(_) => (Some("name"), None, "package_name"),
            InvalidPackageVersion(_) => (Some("version"), None, "semver_version"),
            InvalidDependencyName(tapid_core::DomainError::InvalidPackageName(name)) => {
                let found = [
                    "dependencies",
                    "devDependencies",
                    "optionalDependencies",
                    "peerDependencies",
                ]
                .into_iter()
                .find_map(|field| {
                    value
                        .as_ref()?
                        .get(field)?
                        .as_object()?
                        .keys()
                        .find(|key| {
                            key.strip_prefix("jsr:")
                                .or_else(|| key.strip_prefix("npm:"))
                                .unwrap_or(key)
                                == name
                        })
                        .map(|key| (field, key.clone()))
                });
                match found {
                    Some((field, key)) => (Some(field), Some(key), "dependency_name"),
                    None => (None, None, "dependency_name"),
                }
            }
            DependencyNotDeclared { field, name } => {
                (Some(*field), Some(name.clone()), "declared_dependency")
            }
            ExpectedBinString(key) => (Some("bin"), Some(key.clone()), "string"),
            InvalidBin { command, .. } => {
                let key = value
                    .as_ref()
                    .and_then(|value| value.get("bin"))
                    .and_then(serde_json::Value::as_object)
                    .filter(|map| map.contains_key(command))
                    .map(|_| command.clone());
                (Some("bin"), key, "contained_bin_target")
            }
            InvalidDependencyName(_) => (None, None, "dependency_name"),
        };
        let observed = if value.is_none() {
            "invalid_json"
        } else {
            let observed = match field {
                Some(field) => value.as_ref().and_then(|value| value.get(field)),
                None => value.as_ref(),
            };
            let observed = match &key {
                Some(key) => observed.and_then(|value| value.get(key)),
                None => observed,
            };
            match observed {
                None => "missing",
                Some(serde_json::Value::Null) => "null",
                Some(serde_json::Value::Bool(_)) => "boolean",
                Some(serde_json::Value::Number(_)) => "number",
                Some(serde_json::Value::String(_)) => "string",
                Some(serde_json::Value::Array(_)) => "array",
                Some(serde_json::Value::Object(_)) => "object",
            }
        };
        ManifestDiagnostic {
            field,
            key,
            expected,
            observed,
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(f, "invalid package.json: {error}"),
            Self::RootMustBeObject => write!(f, "package.json root must be an object"),
            Self::RequiredString(key) | Self::ExpectedString(key) => {
                write!(f, "package.json field '{key}' must be a string")
            }
            Self::ExpectedBoolean(key) => write!(f, "package.json field '{key}' must be a boolean"),
            Self::ExpectedMap(key) => write!(f, "package.json field '{key}' must be an object"),
            Self::ExpectedMapValueString { field, key } => write!(
                f,
                "package.json field '{field}' entry '{key}' must be a string"
            ),
            Self::DependencyNotDeclared { field, name } => write!(
                f,
                "dependency '{name}' is not declared in package.json field '{field}'"
            ),
            Self::InvalidPackageName(error)
            | Self::InvalidDependencyName(error)
            | Self::InvalidPackageVersion(error) => error.fmt(f),
            Self::InvalidBin { command, target } => {
                write!(f, "invalid package bin '{command}' -> '{target}'")
            }
            Self::ExpectedBinString(key) => {
                write!(f, "package.json bin entry '{key}' must be a string")
            }
        }
    }
}
impl std::error::Error for ManifestError {}
