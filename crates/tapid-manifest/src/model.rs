use crate::ManifestError;
use serde::Serialize;
use std::{collections::BTreeMap, path::PathBuf};
use tapid_core::{PackageName, PackageVersion};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyKind {
    Dependencies,
    DevDependencies,
    OptionalDependencies,
    PeerDependencies,
}

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
    pub(crate) bin: Option<PackageBin>,
    pub(crate) extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BinTarget {
    pub command: String,
    pub target: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageBin {
    pub package_name: PackageName,
    pub(crate) targets: Vec<BinTarget>,
}

impl PackageBin {
    pub fn targets(&self) -> &[BinTarget] {
        &self.targets
    }
    pub fn command_names(&self) -> Vec<&str> {
        self.targets
            .iter()
            .map(|target| target.command.as_str())
            .collect()
    }
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
            bin: None,
            extra: BTreeMap::new(),
        })
    }

    pub fn name(&self) -> &PackageName {
        &self.name
    }
    pub fn version(&self) -> PackageVersion {
        self.version.clone()
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

    /// Add or update a regular dependency while preserving deterministic output.
    pub fn with_dependency(self, name: &str, requirement: &str) -> Result<Self, ManifestError> {
        self.set_dependency(DependencyKind::Dependencies, name, requirement)
    }

    /// Add or update a dependency in the selected manifest section.
    pub fn with_dependency_kind(
        self,
        kind: DependencyKind,
        name: &str,
        requirement: &str,
    ) -> Result<Self, ManifestError> {
        self.set_dependency(kind, name, requirement)
    }

    fn set_dependency(
        mut self,
        kind: DependencyKind,
        name: &str,
        requirement: &str,
    ) -> Result<Self, ManifestError> {
        let validation_name = name
            .strip_prefix("npm:")
            .or_else(|| name.strip_prefix("jsr:"))
            .unwrap_or(name);
        validation_name
            .parse::<PackageName>()
            .map_err(ManifestError::InvalidPackageName)?;
        for dependencies in [
            &mut self.dependencies,
            &mut self.dev_dependencies,
            &mut self.optional_dependencies,
            &mut self.peer_dependencies,
        ] {
            dependencies.remove(name);
        }
        let dependencies = match kind {
            DependencyKind::Dependencies => &mut self.dependencies,
            DependencyKind::DevDependencies => &mut self.dev_dependencies,
            DependencyKind::OptionalDependencies => &mut self.optional_dependencies,
            DependencyKind::PeerDependencies => &mut self.peer_dependencies,
        };
        dependencies.insert(name.to_owned(), requirement.trim().to_owned());
        Ok(self)
    }

    /// Remove a dependency from every manifest section.
    pub fn without_dependency(mut self, name: &str) -> Result<Self, ManifestError> {
        let validation_name = name
            .strip_prefix("npm:")
            .or_else(|| name.strip_prefix("jsr:"))
            .unwrap_or(name);
        validation_name
            .parse::<PackageName>()
            .map_err(ManifestError::InvalidPackageName)?;
        for dependencies in [
            &mut self.dependencies,
            &mut self.dev_dependencies,
            &mut self.optional_dependencies,
            &mut self.peer_dependencies,
        ] {
            dependencies.remove(name);
        }
        Ok(self)
    }

    pub fn dependency_kind(&self, name: &str) -> Option<DependencyKind> {
        [
            (DependencyKind::Dependencies, &self.dependencies),
            (DependencyKind::DevDependencies, &self.dev_dependencies),
            (
                DependencyKind::OptionalDependencies,
                &self.optional_dependencies,
            ),
            (DependencyKind::PeerDependencies, &self.peer_dependencies),
        ]
        .into_iter()
        .find_map(|(kind, dependencies)| dependencies.contains_key(name).then_some(kind))
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
    pub fn bin(&self) -> Option<&PackageBin> {
        self.bin.as_ref()
    }

    pub fn to_json(&self) -> String {
        let document = ManifestDocument {
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
            bin: self.bin.as_ref().map(|bin| {
                bin.targets
                    .iter()
                    .map(|target| {
                        (
                            target.command.clone(),
                            target.target.to_string_lossy().into_owned(),
                        )
                    })
                    .collect()
            }),
            extra: &self.extra,
        };
        format!(
            "{}\n",
            serde_json::to_string_pretty(&document).expect("manifest is serializable")
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
    #[serde(skip_serializing_if = "Option::is_none")]
    bin: Option<BTreeMap<String, String>>,
    #[serde(flatten)]
    extra: &'a BTreeMap<String, serde_json::Value>,
}
