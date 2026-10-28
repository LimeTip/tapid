//! Offline npm v3 migration. Tree verification is separate from version selection.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use tapid_core::{ArtifactDigest, PackageIntegrity, PackageName, PackageVersion};

mod json;
mod parse;
mod workspace;
pub use parse::NpmImportError;

/// Imported locks retain npm placement and constraints until and after verification.
/// Schema 8 deliberately cannot be read as a schema 7 verified-tree lock.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportedNpmLockfile {
    lockfile_version: u32,
    root_manifest_digest: String,
    npm_lock: Value,
    #[serde(skip)]
    graph: std::sync::OnceLock<std::sync::Arc<ImportedNpmGraph>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspace_manifest_digests: BTreeMap<String, String>,
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

/// Registry entry points for one project manifest, with npm optional overrides.
#[derive(Debug)]
pub struct ImportedNpmEntryPoints {
    pub dependencies: BTreeMap<String, String>,
    pub optional_dependencies: BTreeSet<String>,
}

/// Validated npm placement graph; edges target paths, never newly resolved versions.
#[derive(Debug)]
pub struct ImportedNpmGraph {
    /// Root-relative local targets and their selected npm manifest fields.
    pub workspaces: BTreeMap<String, Value>,
    /// Physical root or member-level links to contained workspace sources.
    pub workspace_links: BTreeMap<String, String>,
    pub packages: BTreeMap<String, ImportedNpmPackage>,
    pub roots: BTreeMap<String, String>,
    pub optional_roots: BTreeSet<String>,
    /// Member-relative registry entry points, preserving conflicting local selections.
    pub member_roots: BTreeMap<String, ImportedNpmEntryPoints>,
}

impl ImportedNpmLockfile {
    pub fn import(input: &str, manifest: &str, digest: &str) -> Result<Self, NpmImportError> {
        let npm_lock = parse::json(input)?;
        let graph = parse::graph(&npm_lock)?;
        validate_manifest_fields(&npm_lock, manifest)?;
        let digest = digest
            .parse::<ArtifactDigest>()
            .map_err(|e| parse::error("/rootManifestDigest", "root", "digest", &e.to_string()))?;
        Ok(Self {
            lockfile_version: 8,
            root_manifest_digest: digest.to_string(),
            npm_lock,
            graph: std::sync::OnceLock::from(std::sync::Arc::new(graph)),
            workspace_manifest_digests: BTreeMap::new(),
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
        for (path, digest) in &lock.workspace_manifest_digests {
            if !graph.workspaces.contains_key(path) {
                return Err(parse::error(
                    "/workspaceManifestDigests",
                    path,
                    "path",
                    "unknown workspace source",
                ));
            }
            digest.parse::<ArtifactDigest>().map_err(|e| {
                parse::error("/workspaceManifestDigests", path, "digest", &e.to_string())
            })?;
        }
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

    /// Bind contained member manifests after offline discovery and selection validation.
    pub fn bind_workspace_manifests(
        &mut self,
        manifests: &BTreeMap<String, (String, String)>,
    ) -> Result<(), NpmImportError> {
        self.workspace_manifest_digests =
            workspace::validate_manifests(self.graph()?.as_ref(), manifests)?;
        Ok(())
    }

    pub fn validate_workspace_manifests(
        &self,
        manifests: &BTreeMap<String, (String, String)>,
    ) -> Result<(), NpmImportError> {
        let current = workspace::validate_manifests(self.graph()?.as_ref(), manifests)?;
        if current != self.workspace_manifest_digests {
            return Err(parse::error(
                "/workspaceManifestDigests",
                "root",
                "workspaces",
                "member manifest changed or has no import receipt; import the matching npm lock again",
            ));
        }
        Ok(())
    }

    pub fn graph(&self) -> Result<std::sync::Arc<ImportedNpmGraph>, NpmImportError> {
        if let Some(graph) = self.graph.get() {
            return Ok(graph.clone());
        }
        let graph = parse::graph(&self.npm_lock)?;
        let _ = self.graph.set(std::sync::Arc::new(graph));
        Ok(self
            .graph
            .get()
            .expect("validated graph was initialized")
            .clone())
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
    /// All recorded entry points and their transitive placements, without host
    /// selection or explanation limits. Workspace sources remain distinct.
    pub fn reachable(&self) -> crate::ReachableLockedGraph {
        use crate::graph::PathGraph;
        let graph = ImportedPathGraph::new(self);
        let mut result = crate::ReachableLockedGraph {
            occurrences: BTreeMap::new(),
            roots_recorded: true,
            unresolved_peers: BTreeSet::new(),
        };
        let mut pending = self
            .roots
            .iter()
            .map(|(name, key)| (name.clone(), key.clone()))
            .collect::<Vec<_>>();
        pending.extend(self.workspaces.iter().map(|(key, manifest)| {
            (
                manifest["name"].as_str().unwrap_or_default().to_owned(),
                key.clone(),
            )
        }));
        while let Some((name, key)) = pending.pop() {
            let names = result.occurrences.entry(key.clone()).or_default();
            let first = names.is_empty();
            names.insert(name);
            if first {
                pending.extend(
                    graph
                        .outgoing(&key)
                        .iter()
                        .map(|edge| (edge.name.clone(), edge.target.clone())),
                );
            }
        }
        result
    }

    /// Selects applicable pinned placements. Incompatible required edges fail closed.
    pub fn selected_paths(
        &self,
        os: &str,
        cpu: &str,
        libc: Option<&str>,
    ) -> Result<BTreeSet<String>, NpmImportError> {
        let mut selected = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut pending: Vec<_> = self
            .roots
            .iter()
            .map(|(name, path)| (path.clone(), self.optional_roots.contains(name)))
            .collect();
        for member in self.member_roots.values() {
            let roots = &member.dependencies;
            let optional = &member.optional_dependencies;
            pending.extend(
                roots
                    .iter()
                    .map(|(name, path)| (path.clone(), optional.contains(name))),
            );
        }
        while let Some((path, optional)) = pending.pop() {
            if !visited.insert((path.clone(), optional)) {
                continue;
            }
            let package = &self.packages[&path];
            let constraints = tapid_core::PackagePlatform {
                os: package.os.clone(),
                cpu: package.cpu.clone(),
                libc: package.libc.clone(),
            };
            if !constraints.matches(os, cpu, libc) {
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

/// One placement index per traversal, shared by all workspace visits.
pub(crate) struct ImportedPathGraph<'a> {
    graph: &'a ImportedNpmGraph,
    placements: BTreeMap<String, ()>,
}

impl<'a> ImportedPathGraph<'a> {
    pub(crate) fn new(graph: &'a ImportedNpmGraph) -> Self {
        Self {
            graph,
            placements: graph
                .packages
                .keys()
                .chain(graph.workspace_links.keys())
                .map(|path| (path.clone(), ()))
                .collect(),
        }
    }
}

impl crate::graph::PathGraph for ImportedPathGraph<'_> {
    fn node(&self, key: &str) -> Option<(&str, &'static str)> {
        self.graph
            .packages
            .get(key)
            .map(|node| (node.name.as_str(), "registry"))
            .or_else(|| {
                self.graph
                    .workspaces
                    .get(key)
                    .map(|node| (node["name"].as_str().unwrap_or_default(), "workspace"))
            })
    }

    fn outgoing(&self, key: &str) -> std::borrow::Cow<'_, [crate::LockedGraphEdge]> {
        use crate::LockedGraphEdge;
        let mut edges = Vec::new();
        if let Some(package) = self.graph.packages.get(key) {
            for (kind, targets) in [
                ("dependency", &package.dependencies),
                ("peer", &package.peers),
            ] {
                edges.extend(targets.iter().map(|(name, target)| LockedGraphEdge {
                    name: name.clone(),
                    target: target.clone(),
                    kind,
                }));
            }
        } else if let Some(member) = self.graph.member_roots.get(key) {
            edges.extend(
                member
                    .dependencies
                    .iter()
                    .map(|(name, target)| LockedGraphEdge {
                        name: name.clone(),
                        target: target.clone(),
                        kind: "dependency",
                    }),
            );
            // Registry entry points deliberately omit local links. Reuse npm's
            // validated placement lookup to retain local workspace edges too.
            for field in [
                "dependencies",
                "devDependencies",
                "optionalDependencies",
                "peerDependencies",
            ] {
                for name in self.graph.workspaces[key][field]
                    .as_object()
                    .into_iter()
                    .flat_map(|fields| fields.keys())
                {
                    let peer = field == "peerDependencies";
                    if let Some(target) = parse::lookup(&self.placements, key, name, peer)
                        .and_then(|placement| self.graph.workspace_links.get(&placement))
                    {
                        let kind = if peer { "peer" } else { "dependency" };
                        if !edges.iter().any(|edge| {
                            edge.name == *name && edge.target == *target && edge.kind == kind
                        }) {
                            edges.push(LockedGraphEdge {
                                name: name.clone(),
                                target: target.clone(),
                                kind,
                            });
                        }
                    }
                }
            }
        }
        std::borrow::Cow::Owned(edges)
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
    validate_manifest_entry(
        root,
        manifest,
        &[
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
            "peerDependenciesMeta",
            "workspaces",
        ],
        "/packages/",
        "root",
    )
}

fn validate_manifest_entry(
    selected: &Map<String, Value>,
    manifest: &Map<String, Value>,
    fields: &[&str],
    at: &str,
    identity: &str,
) -> Result<(), NpmImportError> {
    for &field in fields {
        if selected
            .get(field)
            .filter(|v| !v.as_object().is_some_and(Map::is_empty))
            != manifest
                .get(field)
                .filter(|v| !v.as_object().is_some_and(Map::is_empty))
        {
            return Err(parse::error(
                at,
                identity,
                field,
                "does not match package.json; regenerate package-lock.json with npm before importing",
            ));
        }
    }
    Ok(())
}
