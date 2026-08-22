use std::fmt;

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
            Self::RequiredString(key) | Self::ExpectedString(key) => {
                write!(f, "package.json field '{key}' must be a string")
            }
            Self::ExpectedBoolean(key) => write!(f, "package.json field '{key}' must be a boolean"),
            Self::ExpectedMap(key) => write!(f, "package.json field '{key}' must be an object"),
            Self::ExpectedMapValueString { field, key } => write!(
                f,
                "package.json field '{field}' entry '{key}' must be a string"
            ),
            Self::InvalidPackageName(error)
            | Self::InvalidDependencyName(error)
            | Self::InvalidPackageVersion(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ManifestError {}
