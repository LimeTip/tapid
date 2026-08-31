use crate::{context, online};
use std::{collections::BTreeMap, fs, path::PathBuf};
use tapid_core::{ArtifactDigest, PackageInstanceId};
use tapid_linker::{
    DependencyEdge, InstanceKey, LayoutInput, PackageInstance, Platform, VerifiedTreeReference,
};
use tapid_lockfile::Lockfile;
use tapid_manifest::PackageManifest;
use tapid_store::Store;

struct ReplaySnapshotGuard {
    paths: Vec<PathBuf>,
    keep: bool,
}

pub(crate) fn cleanup_replay_snapshots(trees: &BTreeMap<String, PathBuf>) {
    for tree in trees.values() {
        let _ = fs::remove_dir_all(tree);
    }
}

impl Drop for ReplaySnapshotGuard {
    fn drop(&mut self) {
        if !self.keep {
            for path in &self.paths {
                let _ = fs::remove_dir_all(path);
            }
        }
    }
}

pub(crate) fn replay_input(
    lock: &Lockfile,
    manifest: &PackageManifest,
    store: &Store,
) -> Result<(LayoutInput, BTreeMap<String, PathBuf>), String> {
    let mut instances = Vec::new();
    let mut keys = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut snapshots = ReplaySnapshotGuard {
        paths: Vec::new(),
        keep: false,
    };
    let typed_packages = lock.packages_typed().map_err(|e| e.to_string())?;
    for (key, package) in &typed_packages {
        let encoded = key.to_string();
        let digest: ArtifactDigest = package
            .tree_digest()
            .parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?;
        let tree = store
            .verified_tree_snapshot(&digest)
            .map_err(|e| format!("package {encoded} tree unavailable: {e}"))?;
        snapshots.paths.push(tree.clone());
        let peer = context::parse_peer(&key.peer_context)?;
        let platform = context::parse_platform(&key.platform_context)?;
        let id =
            PackageInstanceId::new(key.registry.clone(), key.name.clone(), key.version.clone());
        let instance = PackageInstance {
            id,
            peer_context: peer,
            platform_context: platform,
            tree: VerifiedTreeReference::new(package.tree_digest(), &tree)
                .map_err(|e| e.to_string())?,
        };
        keys.insert(encoded.clone(), InstanceKey::from(&instance));
        trees.insert(encoded, tree);
        instances.push(instance);
    }
    let mut roots = Vec::new();
    if lock.roots().is_empty() {
        let root_identities = replay_root_identities(manifest)?;
        if root_identities.is_empty() {
            roots.extend(keys.values().cloned());
        } else {
            for identity in root_identities.keys() {
                let selected = typed_packages
                    .iter()
                    .map(|(key, _)| key)
                    .filter(|key| {
                        (&key.registry, &key.name) == (&identity.0, &identity.1)
                            && replay_root_matches(&root_identities, key)
                    })
                    .max_by(|left, right| left.version.cmp(&right.version))
                    .ok_or_else(|| {
                        format!(
                            "legacy lockfile has no exact root candidate for {}:{}",
                            identity.0, identity.1
                        )
                    })?;
                roots.push(keys[&selected.to_string()].clone());
            }
        }
    } else {
        for root in lock.roots() {
            roots.push(
                keys.get(root)
                    .cloned()
                    .ok_or_else(|| format!("missing root package target {root}"))?,
            );
        }
    }
    let mut edges = Vec::new();
    for (key, package) in &typed_packages {
        let encoded = key.to_string();
        for dependency in package.dependencies().values() {
            edges.push(DependencyEdge {
                parent: keys[&encoded].clone(),
                child: keys
                    .get(dependency)
                    .cloned()
                    .ok_or_else(|| format!("missing dependency target {dependency}"))?,
            });
        }
    }
    snapshots.keep = true;
    Ok((
        LayoutInput {
            instances,
            root_dependencies: roots,
            dependency_edges: edges,
        },
        trees,
    ))
}

pub(crate) fn replay_root_identities(
    manifest: &PackageManifest,
) -> Result<
    std::collections::BTreeMap<
        (tapid_core::RegistryOrigin, tapid_core::PackageName),
        Vec<tapid_resolver::Requirement>,
    >,
    String,
> {
    let mut identities = std::collections::BTreeMap::new();
    for map in [
        manifest.dependencies(),
        manifest.dev_dependencies(),
        manifest.optional_dependencies(),
    ] {
        for (name, requirement) in map {
            let (registry, package) = online::dep_parts(name)?;
            identities
                .entry((registry, package))
                .or_insert_with(Vec::new)
                .push(requirement.parse().map_err(|error| format!("{error:?}"))?);
        }
    }
    Ok(identities)
}

pub(crate) fn replay_root_matches(
    roots: &std::collections::BTreeMap<
        (tapid_core::RegistryOrigin, tapid_core::PackageName),
        Vec<tapid_resolver::Requirement>,
    >,
    key: &tapid_lockfile::LockfilePackageKey,
) -> bool {
    roots
        .get(&(key.registry.clone(), key.name.clone()))
        .is_some_and(|requirements| {
            requirements
                .iter()
                .all(|requirement| requirement.matches(&key.version))
        })
}

pub(crate) fn current_platform() -> Platform {
    if cfg!(target_family = "windows") {
        Platform::Windows
    } else if cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd"
    )) {
        Platform::Unix
    } else {
        Platform::Other
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;

    #[test]
    fn explicit_registry_root_matches_only_its_registry_identity() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"jsr:@std/path":"1.0.0"}}"#,
        )
        .unwrap();
        let roots = replay_root_identities(&manifest).unwrap();
        let jsr: tapid_lockfile::LockfilePackageKey =
            "https://jsr.io|@std/path@1.0.0|peer=-|platform=-"
                .parse()
                .unwrap();
        let npm: tapid_lockfile::LockfilePackageKey =
            "https://registry.npmjs.org|@std/path@1.0.0|peer=-|platform=-"
                .parse()
                .unwrap();

        assert!(replay_root_matches(&roots, &jsr));
        assert!(!replay_root_matches(&roots, &npm));
    }
}
