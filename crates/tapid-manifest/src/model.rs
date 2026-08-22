use crate::ManifestError;
use serde::Serialize;
use std::collections::BTreeMap;
use tapid_core::{PackageName, PackageVersion};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageManifest {
    pub(crate) name: PackageName,
    pub(crate) version: PackageVersion,
    pub(crate) private: bool,
    pub(crate) description: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) dependencies: BTreeMap<String, String>,
    pub(crate) dev_dependencies: BTreeMap<String, String>,
    pub(crate) optional_dependencies: BTreeMap<String, String>,
    pub(crate) peer_dependencies: BTreeMap<String, String>,
    pub(crate) scripts: BTreeMap<String, String>,
}

impl PackageManifest {
    pub fn new(name: &str, version: &str, private: bool) -> Result<Self, ManifestError> {
        Ok(Self {
            name: name.parse().map_err(ManifestError::InvalidPackageName)?,
            version: version
                .parse()
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
