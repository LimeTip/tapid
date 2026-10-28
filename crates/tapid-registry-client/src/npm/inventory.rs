use super::*;
use sha2::{Digest, Sha256};

impl<T: HttpTransport> NpmRegistry<T> {
    /// Retrieve declarations through the configured transport without artifacts.
    pub fn inventory(&self, package: &str) -> Result<NpmInventory, RegistryClientError> {
        let name: PackageName = package.parse().map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidPackageName(package.into()))
        })?;
        let url = format!("{}/{}", self.origin, package.replace('/', "%2F"));
        let response = self
            .transport
            .get_with_accept(&url, NPM_FULL_METADATA_ACCEPT)
            .map_err(RegistryClientError::Transport)?;
        NpmInventory::from_metadata(&self.origin, name, json_response(&response)?)
            .map_err(RegistryClientError::Metadata)
    }
}

/// A reported tag target. Missing and unsupported targets never imply fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NpmTagTarget {
    Version(PackageVersion),
    MissingVersion(PackageVersion),
    Unsupported,
}

/// Registry declarations, independent of artifact installability or verification.
#[derive(Clone, Debug)]
pub struct NpmInventory {
    pub origin: RegistryOrigin,
    pub name: PackageName,
    versions: Vec<PackageVersion>,
    tags: BTreeMap<String, NpmTagTarget>,
    tags_reported: bool,
    unsupported_versions: usize,
    entries: serde_json::Map<String, serde_json::Value>,
    metadata_digest: [u8; 32],
}

/// A registry-reported peer declaration; no peer resolution is performed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmPeerRequirement {
    pub name: PackageName,
    pub requirement: String,
    pub optional: bool,
}

/// Selected declarations. `None` means absent, distinct from a reported empty map.
/// Strings remain untrusted registry data and require sanitization for display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmVersionFacts {
    pub identity: RegistryPackageId,
    pub peers: Option<Vec<NpmPeerRequirement>>,
    pub engines: Option<BTreeMap<String, String>>,
    pub deprecated: Option<String>,
    pub executables: Option<Vec<String>>,
}

impl NpmInventory {
    /// Parse a full metadata snapshot without imposing artifact requirements.
    pub fn from_metadata(
        origin: &RegistryOrigin,
        name: PackageName,
        body: &[u8],
    ) -> Result<Self, MetadataError> {
        if body.len() > crate::transport::STANDARD_METADATA_MAX_RESPONSE_BYTES {
            return Err(MetadataError::InvalidJson("metadata exceeds 32 MiB".into()));
        }
        let root = json_object(body)?;
        if required_str(&root, "name")? != name.as_str() {
            return Err(MetadataError::ConflictingField("name".into()));
        }
        let entries = root
            .get("versions")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| MetadataError::MissingField("versions".into()))?;
        let mut versions: Vec<PackageVersion> =
            entries.keys().filter_map(|key| key.parse().ok()).collect();
        versions.sort_by(|a, b| b.cmp(a));
        let mut tags = BTreeMap::new();
        if let Some(value) = root.get("dist-tags") {
            let object = value
                .as_object()
                .ok_or_else(|| MetadataError::InvalidJson("dist-tags must be an object".into()))?;
            for (tag, value) in object {
                let target = match value
                    .as_str()
                    .and_then(|value| value.parse::<PackageVersion>().ok())
                {
                    Some(version) if entries.contains_key(&version.to_string()) => {
                        NpmTagTarget::Version(version)
                    }
                    Some(version) => NpmTagTarget::MissingVersion(version),
                    None => NpmTagTarget::Unsupported,
                };
                tags.insert(tag.clone(), target);
            }
        }
        let unsupported_versions = entries.len() - versions.len();
        Ok(Self {
            origin: origin.clone(),
            name,
            versions,
            tags,
            tags_reported: root.contains_key("dist-tags"),
            unsupported_versions,
            entries: entries.clone(),
            metadata_digest: Sha256::digest(body).into(),
        })
    }

    /// Supported version keys in descending order, including non-installable records.
    pub fn versions(&self) -> &[PackageVersion] {
        &self.versions
    }

    /// Lexically ordered tags with explicit unusable targets.
    pub fn tags(&self) -> &BTreeMap<String, NpmTagTarget> {
        &self.tags
    }

    /// Distinguishes an absent tag map from a reported empty map.
    pub fn tags_reported(&self) -> bool {
        self.tags_reported
    }

    /// Keys excluded because Tapid cannot interpret their version identity.
    pub fn unsupported_versions(&self) -> usize {
        self.unsupported_versions
    }

    /// SHA-256 of exact source bytes, for reproducible traversal, not authenticity.
    pub fn metadata_digest(&self) -> &[u8; 32] {
        &self.metadata_digest
    }

    /// Validate selected identity and declarations without reading artifact evidence.
    /// Malformed unrelated records do not prevent inspecting this version.
    pub fn facts(&self, version: &PackageVersion) -> Result<NpmVersionFacts, MetadataError> {
        let key = version.to_string();
        let entry = self
            .entries
            .get(&key)
            .ok_or_else(|| MetadataError::MissingField("selected version".into()))?
            .as_object()
            .ok_or_else(|| MetadataError::InvalidJson("version entry must be an object".into()))?;
        if required_str(entry, "name")? != self.name.as_str()
            || required_str(entry, "version")? != key
        {
            return Err(MetadataError::ConflictingField("package identity".into()));
        }
        let peers = parse_dependencies(entry.get("peerDependencies")).map_err(metadata_error)?;
        let optional = parse_optional_peer_dependencies(&peers, entry.get("peerDependenciesMeta"))
            .map_err(metadata_error)?;
        let peers = entry.contains_key("peerDependencies").then(|| {
            peers
                .into_iter()
                .map(|(name, requirement)| {
                    let is_optional = optional.contains(&name);
                    NpmPeerRequirement {
                        name,
                        requirement,
                        optional: is_optional,
                    }
                })
                .collect()
        });
        let engines = entry
            .get("engines")
            .map(|value| {
                let object = value.as_object().ok_or_else(|| {
                    MetadataError::InvalidJson("engines must be an object".into())
                })?;
                object
                    .iter()
                    .map(|(name, value)| {
                        let requirement = value.as_str().ok_or_else(|| {
                            MetadataError::InvalidJson("engine requirement must be a string".into())
                        })?;
                        Ok((name.clone(), requirement.to_owned()))
                    })
                    .collect::<Result<BTreeMap<_, _>, MetadataError>>()
            })
            .transpose()?;
        let deprecated = entry
            .get("deprecated")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| MetadataError::InvalidJson("deprecated must be a string".into()))
            })
            .transpose()?;
        let executables = entry
            .get("bin")
            .map(|value| {
                if value.is_string() {
                    // npm's string form names the command after the unscoped package.
                    return Ok(vec![
                        self.name.as_str().rsplit('/').next().unwrap().to_owned(),
                    ]);
                }
                let object = value.as_object().ok_or_else(|| {
                    MetadataError::InvalidJson("bin must be a string or object".into())
                })?;
                if object.values().any(|value| !value.is_string()) {
                    return Err(MetadataError::InvalidJson(
                        "bin targets must be strings".into(),
                    ));
                }
                let mut names = object.keys().cloned().collect::<Vec<_>>();
                names.sort();
                Ok(names)
            })
            .transpose()?;
        Ok(NpmVersionFacts {
            identity: RegistryPackageId::new(
                self.origin.clone(),
                self.name.clone(),
                version.clone(),
            ),
            peers,
            engines,
            deprecated,
            executables,
        })
    }
}

fn metadata_error(error: RegistryClientError) -> MetadataError {
    match error {
        RegistryClientError::Metadata(error) => error,
        RegistryClientError::Transport(_) => {
            unreachable!("declaration parsers do not use transport")
        }
    }
}
