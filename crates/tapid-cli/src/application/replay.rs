use crate::context;
use std::{collections::BTreeMap, fs, path::PathBuf};
use tapid_core::{ArtifactDigest, PackageInstanceId};
use tapid_linker::{
    InstanceKey, NamedDependency, NamedDependencyEdge, NamedLayoutInput, PackageInstance, Platform,
    VerifiedTreeReference,
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

fn replay_progress_checkpoint(completed: usize, total: usize) -> bool {
    total > 0 && (completed == 1 || completed == total || completed.is_multiple_of(50))
}

fn complete_progress_step<T, E>(
    completed: usize,
    total: usize,
    action: impl FnOnce() -> Result<T, E>,
    report: impl FnOnce(usize, usize),
) -> Result<T, E> {
    let value = action()?;
    if replay_progress_checkpoint(completed, total) {
        report(completed, total);
    }
    Ok(value)
}

pub(crate) fn replay_input(
    lock: &Lockfile,
    manifest: &PackageManifest,
    store: &Store,
    registry_config: &crate::registry::RegistryConfig,
    mut report_progress: impl FnMut(usize, usize),
) -> Result<(NamedLayoutInput, BTreeMap<String, PathBuf>), String> {
    let mut instances = Vec::new();
    let mut keys = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut snapshots = ReplaySnapshotGuard {
        paths: Vec::new(),
        keep: false,
    };
    let typed_packages = lock.packages_typed().map_err(|e| e.to_string())?;
    let mut typed_keys = typed_packages
        .iter()
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    typed_keys.extend(
        lock.workspace_packages_typed()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(key, _)| key),
    );
    let root_keys = replay_root_keys_with_config(lock, manifest, &typed_keys, registry_config)?;
    store
        .cleanup_stale_replay_snapshots()
        .map_err(|error| format!("cannot recover stale replay snapshots: {error}"))?;
    let package_total = typed_packages.len();
    for (index, (key, package)) in typed_packages.iter().enumerate() {
        let completed = index + 1;
        let encoded = key.to_string();
        let digest: ArtifactDigest = package
            .tree_digest()
            .parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?;
        let tree = complete_progress_step(
            completed,
            package_total,
            || store.verified_tree_snapshot(&digest),
            &mut report_progress,
        )
        .map_err(|e| format!("package {encoded} tree unavailable: {e}"))?;
        snapshots.paths.push(tree.clone());
        let peer = context::parse_peer(&key.peer_context)?;
        let platform = context::parse_platform(&key.platform_context)?;
        let registry = key.source.registry().cloned().ok_or_else(|| {
            format!("workspace source unexpectedly appeared as a registry artifact: {encoded}")
        })?;
        let id = PackageInstanceId::new(registry, key.name.clone(), key.version.clone());
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
    let bindings = if lock.root_bindings().is_empty() {
        root_keys
            .iter()
            .filter(|root| {
                root.parse::<tapid_lockfile::LockfilePackageKey>()
                    .is_ok_and(|key| key.source.registry().is_some())
            })
            .map(|root| {
                let key: tapid_lockfile::LockfilePackageKey = root
                    .parse()
                    .map_err(|error: tapid_lockfile::LockfileError| error.to_string())?;
                Ok((key.name.to_string(), root.clone()))
            })
            // Preserve duplicate local names so the linker rejects conflicting
            // origins rather than silently dropping one legacy root.
            .collect::<Result<Vec<_>, String>>()?
    } else {
        lock.root_bindings()
            .iter()
            .map(|(name, key)| (name.clone(), key.clone()))
            .collect()
    };
    let roots = bindings
        .iter()
        .map(|(name, root)| {
            Ok(NamedDependency {
                name: name
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?,
                child: keys
                    .get(root)
                    .cloned()
                    .ok_or_else(|| format!("missing root package target {root}"))?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut roots = roots;
    for workspace_package in lock.workspace_packages().values() {
        for (name, target) in workspace_package.dependencies() {
            let target_key = target
                .parse::<tapid_lockfile::LockfilePackageKey>()
                .map_err(|error| error.to_string())?;
            if target_key.source.registry().is_some() {
                roots.push(NamedDependency {
                    name: name
                        .parse()
                        .map_err(|error: tapid_core::DomainError| error.to_string())?,
                    child: keys
                        .get(target)
                        .cloned()
                        .ok_or_else(|| format!("missing workspace dependency target {target}"))?,
                });
            }
        }
    }
    let mut edges = Vec::new();
    for (key, package) in &typed_packages {
        let encoded = key.to_string();
        for (name, dependency) in package.dependencies() {
            edges.push(NamedDependencyEdge {
                parent: keys[&encoded].clone(),
                dependency: NamedDependency {
                    name: name
                        .parse()
                        .map_err(|error: tapid_core::DomainError| error.to_string())?,
                    child: keys
                        .get(dependency)
                        .cloned()
                        .ok_or_else(|| format!("missing dependency target {dependency}"))?,
                },
            });
        }
    }
    snapshots.keep = true;
    Ok((
        NamedLayoutInput {
            instances,
            root_dependencies: roots,
            dependency_edges: edges,
        },
        trees,
    ))
}

#[cfg(test)]
fn replay_root_keys(
    lock: &Lockfile,
    manifest: &PackageManifest,
    typed_keys: &[tapid_lockfile::LockfilePackageKey],
) -> Result<Vec<String>, String> {
    replay_root_keys_with_config(
        lock,
        manifest,
        typed_keys,
        &crate::registry::RegistryConfig::default(),
    )
}

fn replay_root_keys_with_config(
    lock: &Lockfile,
    manifest: &PackageManifest,
    typed_keys: &[tapid_lockfile::LockfilePackageKey],
    registry_config: &crate::registry::RegistryConfig,
) -> Result<Vec<String>, String> {
    if !lock.root_bindings().is_empty() {
        let required_names = manifest
            .dependencies()
            .keys()
            .chain(manifest.dev_dependencies().keys())
            .map(|name| crate::online::dep_parts(name).map(|(_, package)| package))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        let optional_names = manifest
            .optional_dependencies()
            .keys()
            .map(|name| crate::online::dep_parts(name).map(|(_, package)| package))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        let mut declarations =
            BTreeMap::<tapid_core::PackageName, Vec<tapid_resolver::Dependency>>::new();
        for dependency in crate::online::manifest_roots(manifest)? {
            let workspace_root = lock.roots().iter().any(|root| {
                root.parse::<tapid_lockfile::LockfilePackageKey>()
                    .ok()
                    .is_some_and(|key| {
                        key.source.workspace().is_some_and(|source| {
                            source.name() == dependency.name.as_str()
                                && dependency.requirement.matches(&key.version)
                        })
                    })
            });
            if workspace_root {
                continue;
            }
            declarations
                .entry(dependency.name.clone())
                .or_default()
                .push(dependency);
        }
        let typed_by_key = typed_keys
            .iter()
            .map(|key| (key.to_string(), key))
            .collect::<BTreeMap<_, _>>();
        for (name, target) in lock.root_bindings() {
            let name = name
                .parse::<tapid_core::PackageName>()
                .map_err(|error| error.to_string())?;
            let expected = declarations.get(&name).ok_or_else(|| {
                format!("lockfile root binding {name} is not a direct manifest dependency")
            })?;
            let key = typed_by_key
                .get(target)
                .ok_or_else(|| format!("missing root package target {target}"))?;
            for dependency in expected {
                let actual = dependency.requirement.package_name(&dependency.name);
                let registry = if dependency.registry.as_str() == "https://jsr.io" {
                    dependency.registry.clone()
                } else {
                    registry_config.origin_for_name(actual)?
                };
                if key.source.registry() != Some(&registry)
                    || &key.name != actual
                    || !dependency.requirement.matches(&key.version)
                {
                    return Err(format!(
                        "lockfile root binding {name} does not satisfy the manifest declaration"
                    ));
                }
            }
        }
        for name in declarations.keys() {
            let optional_only = optional_names.contains(name) && !required_names.contains(name);
            if !optional_only && !lock.root_bindings().contains_key(name.as_str()) {
                return Err(format!("lockfile is missing root binding for {name}"));
            }
        }
        return Ok(lock.roots().to_vec());
    }
    if crate::online::manifest_roots(manifest)?
        .iter()
        .any(|dependency| dependency.requirement.is_alias())
    {
        return Err(
            "npm aliases require lockfile root bindings; regenerate tapid.lock online".into(),
        );
    }
    let root_identities = replay_root_identities_with_config(manifest, registry_config)?;
    let optional_only = optional_only_root_identities_with_config(manifest, registry_config)?;
    if root_identities.is_empty() {
        let has_registry_root = lock.roots().iter().any(|root| {
            root.parse::<tapid_lockfile::LockfilePackageKey>()
                .is_ok_and(|key| key.source.registry().is_some())
        });
        return if has_registry_root {
            Err("lockfile has registry roots but the manifest has no direct dependencies".into())
        } else {
            Ok(lock.roots().to_vec())
        };
    }

    if lock.roots().is_empty() {
        return root_identities
            .keys()
            .map(|identity| {
                let candidates = typed_keys
                    .iter()
                    .filter(|key| {
                        key.source.registry() == Some(&identity.0)
                            && key.name == identity.1
                            && replay_root_matches(&root_identities, key)
                    })
                    .collect::<Vec<_>>();
                let Some(highest_version) = candidates.iter().map(|key| &key.version).max() else {
                    if optional_only.contains(identity) {
                        return Ok(None);
                    }
                    return Err(format!(
                        "legacy lockfile has no exact root candidate for {}:{}",
                        identity.0, identity.1
                    ));
                };
                let highest = candidates
                    .into_iter()
                    .filter(|key| &key.version == highest_version)
                    .collect::<Vec<_>>();
                match highest.as_slice() {
                    [selected] => Ok(Some(selected.to_string())),
                    _ => Err(format!(
                        "legacy lockfile has ambiguous exact root candidates for {}:{} at version {}",
                        identity.0, identity.1, highest_version
                    )),
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|roots| roots.into_iter().flatten().collect());
    }

    let typed_by_key = typed_keys
        .iter()
        .map(|key| (key.to_string(), key))
        .collect::<BTreeMap<_, _>>();
    let mut matched = BTreeMap::new();
    let mut matched_workspace = std::collections::BTreeSet::new();
    for root in lock.roots() {
        let key = typed_by_key
            .get(root)
            .ok_or_else(|| format!("missing root package target {root}"))?;
        if key.source.workspace().is_some() {
            if !workspace_root_matches(key) {
                return Err(format!(
                    "lockfile workspace root {root} has inconsistent package identity"
                ));
            }
            let identity = registry_config.identity_for_spec(key.name.as_str())?;
            matched_workspace.insert(identity);
            continue;
        }
        if !replay_root_matches(&root_identities, key) {
            return Err(format!(
                "lockfile root {root} does not satisfy a direct manifest dependency"
            ));
        }
        if let Some(registry) = key.source.registry() {
            *matched
                .entry((registry.clone(), key.name.clone()))
                .or_insert(0_usize) += 1;
        }
    }
    for identity in root_identities.keys() {
        if matched_workspace.contains(identity) {
            continue;
        }
        if optional_only.contains(identity) && !matched.contains_key(identity) {
            continue;
        }
        if matched.get(identity) != Some(&1) {
            return Err(format!(
                "lockfile must contain exactly one root for direct dependency {}:{}",
                identity.0, identity.1
            ));
        }
    }
    Ok(lock.roots().to_vec())
}

fn workspace_root_matches(key: &tapid_lockfile::LockfilePackageKey) -> bool {
    key.source.workspace().is_some_and(|source| {
        source.name() == key.name.as_str() && source.version() == key.version.to_string()
    })
}

fn optional_only_root_identities_with_config(
    manifest: &PackageManifest,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<std::collections::BTreeSet<(tapid_core::RegistryOrigin, tapid_core::PackageName)>, String>
{
    let mut required = std::collections::BTreeSet::new();
    for map in [manifest.dependencies(), manifest.dev_dependencies()] {
        for name in map.keys() {
            required.insert(registry_config.identity_for_spec(name)?);
        }
    }
    let mut optional = std::collections::BTreeSet::new();
    for name in manifest.optional_dependencies().keys() {
        let identity = registry_config.identity_for_spec(name)?;
        if !required.contains(&identity) {
            optional.insert(identity);
        }
    }
    Ok(optional)
}

#[cfg(test)]
pub(crate) fn replay_root_identities(
    manifest: &PackageManifest,
) -> Result<
    std::collections::BTreeMap<
        (tapid_core::RegistryOrigin, tapid_core::PackageName),
        Vec<tapid_resolver::Requirement>,
    >,
    String,
> {
    replay_root_identities_with_config(manifest, &crate::registry::RegistryConfig::default())
}

fn replay_root_identities_with_config(
    manifest: &PackageManifest,
    registry_config: &crate::registry::RegistryConfig,
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
            let (registry, package) = registry_config.identity_for_spec(name)?;
            if requirement.starts_with("workspace:") {
                continue;
            }
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
    key.source
        .registry()
        .and_then(|registry| roots.get(&(registry.clone(), key.name.clone())))
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
    fn replay_progress_is_reported_after_snapshot_completion() {
        let events = std::cell::RefCell::new(Vec::new());

        let value = complete_progress_step(
            1,
            1,
            || {
                events.borrow_mut().push("snapshot");
                Ok::<_, ()>(42)
            },
            |_, _| events.borrow_mut().push("progress"),
        )
        .unwrap();

        assert_eq!(value, 42);
        assert_eq!(*events.borrow(), ["snapshot", "progress"]);
    }

    #[test]
    fn replay_progress_is_emitted_at_bounded_checkpoints() {
        let checkpoints = (1..=612)
            .filter(|completed| replay_progress_checkpoint(*completed, 612))
            .collect::<Vec<_>>();

        assert_eq!(checkpoints.first(), Some(&1));
        assert_eq!(checkpoints.last(), Some(&612));
        assert!(checkpoints.len() <= 14);
    }

    fn legacy_lock() -> Lockfile {
        Lockfile::from_json(
            r#"{"lockfileVersion":4,"rootManifestDigest":"sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","resolverVersion":"0","linkerVersion":"0","packages":{}}"#,
        )
        .unwrap()
    }

    #[test]
    fn replay_allows_a_platform_omitted_optional_root() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","optionalDependencies":{"native":"1.0.0"}}"#,
        )
        .unwrap();
        let current = Lockfile::new(
            "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();

        assert_eq!(
            replay_root_keys(&current, &manifest, &[]).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            replay_root_keys(&legacy_lock(), &manifest, &[]).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn optional_overlap_does_not_relax_a_required_root() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"native":"1.0.0"},"optionalDependencies":{"native":"1.0.0"}}"#,
        )
        .unwrap();

        assert!(replay_root_keys(&legacy_lock(), &manifest, &[]).is_err());
    }

    #[test]
    fn legacy_root_reconstruction_selects_highest_candidate_matching_all_manifest_maps() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"debug":"*"},"devDependencies":{"debug":"^4.0.0"}}"#,
        )
        .unwrap();
        let keys = [
            "https://registry.npmjs.org|debug@3.0.0|peer=-|platform=-"
                .parse()
                .unwrap(),
            "https://registry.npmjs.org|debug@4.0.0|peer=-|platform=-"
                .parse()
                .unwrap(),
        ];

        assert_eq!(
            replay_root_keys(&legacy_lock(), &manifest, &keys).unwrap(),
            ["https://registry.npmjs.org|debug@4.0.0|peer=-|platform=-"]
        );
    }

    #[test]
    fn legacy_root_reconstruction_rejects_ambiguous_highest_contexts() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"debug":"^4.0.0"}}"#,
        )
        .unwrap();
        let origin = "https://registry.npmjs.org";
        let keys = [
            format!("{origin}|debug@4.0.0|peer=-|platform=-")
                .parse()
                .unwrap(),
            format!("{origin}|debug@4.0.0|peer=name=react;version=18.2.0|platform=-")
                .parse()
                .unwrap(),
        ];

        let error = replay_root_keys(&legacy_lock(), &manifest, &keys).unwrap_err();
        assert!(error.contains("ambiguous exact root candidates"));
    }

    #[test]
    fn legacy_root_reconstruction_rejects_a_missing_direct_candidate() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"debug":"^4.0.0"}}"#,
        )
        .unwrap();
        let keys = ["https://registry.npmjs.org|debug@3.0.0|peer=-|platform=-"
            .parse()
            .unwrap()];

        assert!(replay_root_keys(&legacy_lock(), &manifest, &keys).is_err());
    }

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
