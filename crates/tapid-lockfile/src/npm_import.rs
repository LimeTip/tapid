//! Offline npm v3 migration. Tree verification is separate from version selection.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use tapid_core::{ArtifactDigest, PackageIntegrity, PackageName, PackageVersion};

mod json;
mod parse;
pub use parse::NpmImportError;

/// Imported locks retain npm placement and constraints until and after verification.
/// Schema 8 deliberately cannot be read as a schema 7 verified-tree lock.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportedNpmLockfile {
    lockfile_version: u32,
    root_manifest_digest: String,
    npm_lock: Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    verified_trees: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    verified_artifacts: BTreeMap<String, ImportedNpmArtifactReceipt>,
}

/// Artifact identity proven when a verified tree was produced from an imported lock.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportedNpmArtifactReceipt {
    pub tree_digest: String,
    pub resolved: String,
    pub integrity: String,
}

/// Exact selected artifact and its npm placement, with no invented tree digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedNpmPackage {
    pub path: String,
    pub name: PackageName,
    pub version: PackageVersion,
    pub registry: tapid_core::RegistryOrigin,
    pub resolved: String,
    pub integrity: PackageIntegrity,
    pub dependencies: BTreeMap<String, String>,
    pub optional_dependencies: BTreeSet<String>,
    pub peers: BTreeMap<String, String>,
    pub optional_peers: BTreeSet<String>,
    pub optional: bool,
    pub os: Vec<String>,
    pub cpu: Vec<String>,
    pub libc: Vec<String>,
}

/// Validated npm placement graph; edges target paths, never newly resolved versions.
pub struct ImportedNpmGraph {
    pub packages: BTreeMap<String, ImportedNpmPackage>,
    pub roots: BTreeMap<String, String>,
    pub optional_roots: BTreeSet<String>,
}

impl ImportedNpmLockfile {
    pub fn import(input: &str, manifest: &str, digest: &str) -> Result<Self, NpmImportError> {
        let npm_lock = parse::json(input)?;
        parse::graph(&npm_lock)?;
        validate_manifest_fields(&npm_lock, manifest)?;
        let digest = digest
            .parse::<ArtifactDigest>()
            .map_err(|e| parse::error("/rootManifestDigest", "root", "digest", &e.to_string()))?;
        Ok(Self {
            lockfile_version: 8,
            root_manifest_digest: digest.to_string(),
            npm_lock,
            verified_trees: BTreeMap::new(),
            verified_artifacts: BTreeMap::new(),
        })
    }

    pub fn from_json(input: &str) -> Result<Self, NpmImportError> {
        let lock: Self = serde_json::from_value(parse::json(input)?)
            .map_err(|e| parse::error("/", "root", "JSON", &e.to_string()))?;
        if lock.lockfile_version != 8 {
            return Err(parse::error(
                "/lockfileVersion",
                "root",
                "lockfileVersion",
                "expected imported Tapid schema 8",
            ));
        }
        lock.root_manifest_digest
            .parse::<ArtifactDigest>()
            .map_err(|e| parse::error("/rootManifestDigest", "root", "digest", &e.to_string()))?;
        let graph = lock.graph()?;
        for (path, digest) in &lock.verified_trees {
            if !graph.packages.contains_key(path) {
                return Err(parse::error(
                    "/verifiedTrees",
                    path,
                    "path",
                    "unknown package placement",
                ));
            }
            digest
                .parse::<ArtifactDigest>()
                .map_err(|e| parse::error("/verifiedTrees", path, "digest", &e.to_string()))?;
        }
        for (path, receipt) in &lock.verified_artifacts {
            let Some(package) = graph.packages.get(path) else {
                return Err(parse::error(
                    "/verifiedArtifacts",
                    path,
                    "path",
                    "unknown package placement",
                ));
            };
            receipt.tree_digest.parse::<ArtifactDigest>().map_err(|e| {
                parse::error("/verifiedArtifacts", path, "treeDigest", &e.to_string())
            })?;
            receipt.integrity.parse::<PackageIntegrity>().map_err(|e| {
                parse::error("/verifiedArtifacts", path, "integrity", &e.to_string())
            })?;
            if lock.verified_trees.get(path) != Some(&receipt.tree_digest)
                || receipt.resolved != package.resolved
                || receipt.integrity != package.integrity.to_string()
            {
                return Err(parse::error(
                    "/verifiedArtifacts",
                    path,
                    "receipt",
                    "does not match the imported artifact selection",
                ));
            }
        }
        Ok(lock)
    }

    pub fn to_json(&self) -> Result<String, NpmImportError> {
        serde_json::to_string_pretty(self)
            .map(|s| s + "\n")
            .map_err(|e| parse::error("/", "root", "JSON", &e.to_string()))
    }

    pub fn validate_replay(&self, manifest: &str, digest: &str) -> Result<(), NpmImportError> {
        if self.root_manifest_digest != digest {
            return Err(parse::error(
                "/rootManifestDigest",
                "root",
                "digest",
                "imported lock does not match package.json; import its matching npm lock again",
            ));
        }
        validate_manifest_fields(&self.npm_lock, manifest)
    }

    pub fn graph(&self) -> Result<ImportedNpmGraph, NpmImportError> {
        parse::graph(&self.npm_lock)
    }
    pub fn root_manifest_digest(&self) -> &str {
        &self.root_manifest_digest
    }
    pub fn verified_tree(&self, path: &str) -> Option<&str> {
        self.verified_trees.get(path).map(String::as_str)
    }

    pub fn verified_artifact(&self, path: &str) -> Option<&ImportedNpmArtifactReceipt> {
        self.verified_artifacts.get(path)
    }
    pub fn record_verified_trees(
        &mut self,
        trees: BTreeMap<String, String>,
    ) -> Result<(), NpmImportError> {
        let graph = self.graph()?;
        let mut validated = BTreeMap::new();
        for (path, digest) in trees {
            if !graph.packages.contains_key(&path) {
                return Err(parse::error(
                    "/verifiedTrees",
                    &path,
                    "path",
                    "unknown package placement",
                ));
            }
            let digest = digest
                .parse::<ArtifactDigest>()
                .map_err(|e| parse::error("/verifiedTrees", &path, "digest", &e.to_string()))?;
            validated.insert(path, digest.to_string());
        }
        self.verified_trees.extend(validated);
        Ok(())
    }

    pub fn record_verified_artifacts(
        &mut self,
        receipts: BTreeMap<String, ImportedNpmArtifactReceipt>,
    ) -> Result<(), NpmImportError> {
        let graph = self.graph()?;
        let mut validated = BTreeMap::new();
        for (path, receipt) in receipts {
            let Some(package) = graph.packages.get(&path) else {
                return Err(parse::error(
                    "/verifiedArtifacts",
                    &path,
                    "path",
                    "unknown package placement",
                ));
            };
            receipt.tree_digest.parse::<ArtifactDigest>().map_err(|e| {
                parse::error("/verifiedArtifacts", &path, "treeDigest", &e.to_string())
            })?;
            receipt.integrity.parse::<PackageIntegrity>().map_err(|e| {
                parse::error("/verifiedArtifacts", &path, "integrity", &e.to_string())
            })?;
            if self.verified_trees.get(&path) != Some(&receipt.tree_digest)
                || receipt.resolved != package.resolved
                || receipt.integrity != package.integrity.to_string()
            {
                return Err(parse::error(
                    "/verifiedArtifacts",
                    &path,
                    "receipt",
                    "does not match the imported artifact selection",
                ));
            }
            validated.insert(path, receipt);
        }
        self.verified_artifacts.extend(validated);
        Ok(())
    }
}

impl ImportedNpmGraph {
    /// Selects applicable pinned placements. Incompatible required edges fail closed.
    pub fn selected_paths(
        &self,
        os: &str,
        cpu: &str,
        libc: Option<&str>,
    ) -> Result<BTreeSet<String>, NpmImportError> {
        let os = match os {
            "macos" => "darwin",
            "windows" => "win32",
            value => value,
        };
        let cpu = match cpu {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            value => value,
        };
        fn matches(values: &[String], current: Option<&str>) -> bool {
            values.is_empty()
                || current.is_some_and(|current| {
                    !values.iter().any(|v| v.strip_prefix('!') == Some(current))
                        && (values.iter().all(|v| v.starts_with('!'))
                            || values.iter().any(|v| v == current))
                })
        }
        let mut selected = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut pending: Vec<_> = self
            .roots
            .iter()
            .map(|(name, path)| (path.clone(), self.optional_roots.contains(name)))
            .collect();
        while let Some((path, optional)) = pending.pop() {
            if !visited.insert((path.clone(), optional)) {
                continue;
            }
            let package = &self.packages[&path];
            if !matches(&package.os, Some(os))
                || !matches(&package.cpu, Some(cpu))
                || os == "linux"
                    && !(libc.is_none() && package.libc.iter().all(|value| value.starts_with('!')))
                    && !matches(&package.libc, libc)
            {
                if optional {
                    continue;
                }
                return Err(parse::error(
                    &format!("/packages/{}", path.replace('/', "~1")),
                    &format!("{}@{}", package.name, package.version),
                    "os/cpu/libc",
                    "required selected package is incompatible with this platform",
                ));
            }
            selected.insert(path);
            for (name, child) in &package.dependencies {
                pending.push((child.clone(), package.optional_dependencies.contains(name)));
            }
            for (name, child) in &package.peers {
                pending.push((child.clone(), package.optional_peers.contains(name)));
            }
        }
        Ok(selected)
    }

    pub fn peer_context(&self, path: &str) -> tapid_core::PeerContext {
        self.packages[path].peers.iter().fold(
            tapid_core::PeerContext::default(),
            |context, (name, target)| {
                context.with(
                    name.parse().expect("validated peer name"),
                    self.packages[target].version.clone(),
                )
            },
        )
    }
}

#[cfg(test)]
mod tests;

fn validate_manifest_fields(npm_lock: &Value, manifest: &str) -> Result<(), NpmImportError> {
    let manifest = parse::json(manifest)?;
    let root = npm_lock["packages"][""]
        .as_object()
        .expect("validated root");
    let manifest = parse::object(&manifest, "/package.json", "root")?;
    for field in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
        "peerDependenciesMeta",
    ] {
        if root
            .get(field)
            .filter(|v| !v.as_object().is_some_and(Map::is_empty))
            != manifest
                .get(field)
                .filter(|v| !v.as_object().is_some_and(Map::is_empty))
        {
            return Err(parse::error(
                "/packages/",
                "root",
                field,
                "does not match package.json; regenerate package-lock.json with npm before importing",
            ));
        }
    }
    if manifest.contains_key("workspaces") {
        return Err(parse::error(
            "/package.json",
            "root",
            "workspaces",
            "workspace import is unsupported; linked entries require workspace conversion",
        ));
    }
    Ok(())
}
