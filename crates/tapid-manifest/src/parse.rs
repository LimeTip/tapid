use crate::{ManifestError, PackageManifest};
use serde_json::Value;
use std::{collections::BTreeMap, str::FromStr};
use tapid_core::PackageName;

impl PackageManifest {
    /// Parses and validates a UTF-8 `package.json` document.
    pub fn parse(input: &str) -> Result<Self, ManifestError> {
        let value: Value = serde_json::from_str(input).map_err(ManifestError::InvalidJson)?;
        let object = value.as_object().ok_or(ManifestError::RootMustBeObject)?;
        let name = required_string(object, "name")?;
        let version = required_string(object, "version")?;
        let mut manifest = Self::new(name, version, optional_bool(object, "private")?)?;
        manifest.description = optional_string(object, "description")?;
        manifest.license = optional_string(object, "license")?;
        if let Some(value) = object.get("dependencies") {
            manifest.dependencies = parse_string_map(value, "dependencies", true)?;
        }
        if let Some(value) = object.get("devDependencies") {
            manifest.dev_dependencies = parse_string_map(value, "devDependencies", true)?;
        }
        if let Some(value) = object.get("optionalDependencies") {
            manifest.optional_dependencies = parse_string_map(value, "optionalDependencies", true)?;
        }
        if let Some(value) = object.get("peerDependencies") {
            manifest.peer_dependencies = parse_string_map(value, "peerDependencies", true)?;
        }
        if let Some(value) = object.get("scripts") {
            manifest.scripts = parse_string_map(value, "scripts", false)?;
        }
        Ok(manifest)
    }
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
            .map(|v| Some(v.to_owned()))
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
                let candidate = name.strip_prefix("jsr:").unwrap_or(name);
                PackageName::from_str(candidate).map_err(ManifestError::InvalidDependencyName)?;
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
