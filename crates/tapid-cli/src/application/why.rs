use crate::{
    application::{
        lifecycle::resolve_workspace,
        outcome::{ErrorKind, OperationalError},
    },
    filesystem::atomic::digest_bytes,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
use tapid_lockfile::{Lockfile, LockfilePackageKey, LockfilePackageSource};
use tapid_manifest::PackageManifest;

const MAX_PATHS: usize = 50;
const MAX_DEPTH: usize = 128;
const MAX_VISITS: usize = 10_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WhyReport {
    pub(crate) schema_version: u32,
    pub(crate) operation: &'static str,
    pub(crate) outcome: &'static str,
    pub(crate) effective_project: String,
    pub(crate) changes: Vec<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) package: String,
    pub(crate) paths: Vec<WhyPath>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WhyPath {
    pub(crate) steps: Vec<WhyStep>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WhyStep {
    pub(crate) key: String,
    pub(crate) via: Option<String>,
    pub(crate) edge_kind: Option<String>,
    pub(crate) source_kind: &'static str,
}

#[derive(Clone)]
struct Edge {
    name: String,
    target: String,
}

#[derive(Clone)]
struct Root {
    key: String,
    name: String,
    kind: String,
}

#[derive(Default)]
struct SearchState {
    paths: Vec<WhyPath>,
    truncated: bool,
    visits: usize,
}

pub(crate) fn explain(
    project_dir: &Path,
    workspace: Option<&str>,
    package: &str,
) -> Result<WhyReport, OperationalError> {
    let requested = package
        .parse::<tapid_core::PackageName>()
        .map_err(|error| {
            OperationalError::new(
                ErrorKind::InvalidRequest,
                format!("invalid package name: {error}"),
            )
        })?;
    let selection = resolve_workspace(project_dir, workspace)?;
    let root_manifest_path = selection.root_dir.join("package.json");
    let root_manifest_bytes = fs::read(&root_manifest_path).map_err(|error| {
        OperationalError::from_source(ErrorKind::Manifest, error)
            .context("cannot read root package.json")
    })?;
    let root_digest = digest_bytes(&root_manifest_bytes);
    let lock_path = selection.root_dir.join("tapid.lock");
    let lock_bytes = fs::read_to_string(&lock_path).map_err(|error| {
        let kind = if error.kind() == std::io::ErrorKind::NotFound {
            ErrorKind::LockfileMissing
        } else {
            ErrorKind::Lockfile
        };
        OperationalError::from_source(kind, error).context("cannot read tapid.lock")
    })?;
    let lock = Lockfile::from_json(&lock_bytes).map_err(OperationalError::from)?;
    lock.validate_replay(&root_digest)
        .map_err(OperationalError::from)?;

    let roots = if selection.manifest_path == root_manifest_path {
        direct_roots(&lock, &selection.manifest, &selection.root_dir)?
    } else {
        workspace_roots(
            &lock,
            &selection.manifest,
            &requested,
            &selection.root_dir,
            &selection.manifest_path,
        )?
    };
    let graph = graph(&lock)?;
    let source_kinds = graph
        .keys()
        .map(|key| (key.clone(), source_kind(key)))
        .collect::<BTreeMap<_, _>>();
    let mut state = SearchState::default();
    for root in roots {
        let Some(source_kind) = source_kinds.get(&root.key).copied() else {
            continue;
        };
        let first = WhyStep {
            key: root.key.clone(),
            via: Some(root.name.clone()),
            edge_kind: Some(root.kind.clone()),
            source_kind,
        };
        let mut path = vec![first];
        let mut visited = BTreeSet::from([root.key.clone()]);
        search_paths(
            &root.key,
            &requested,
            &graph,
            &source_kinds,
            &mut visited,
            &mut path,
            &mut state,
            0,
        );
        if state.truncated {
            break;
        }
    }

    let warnings = if state.paths.iter().any(|path| path.steps.len() > 1) {
        vec!["transitive dependency edge kinds are not recorded in the lockfile; those edges are labeled dependency".to_owned()]
    } else {
        Vec::new()
    };
    let outcome = if state.paths.is_empty() {
        "not_found"
    } else {
        "success"
    };
    Ok(WhyReport {
        schema_version: 1,
        operation: "why",
        outcome,
        effective_project: selection.root_dir.display().to_string(),
        changes: Vec::new(),
        warnings,
        package: requested.to_string(),
        paths: state.paths,
        truncated: state.truncated,
    })
}

fn direct_roots(
    lock: &Lockfile,
    manifest: &PackageManifest,
    project_dir: &Path,
) -> Result<Vec<Root>, OperationalError> {
    let kinds = direct_dependency_kinds(manifest);
    if !lock.root_bindings().is_empty() {
        return Ok(lock
            .root_bindings()
            .iter()
            .filter_map(|(name, key)| {
                let kind = kinds.get(name.as_str())?;
                Some(Root {
                    key: key.clone(),
                    name: name.clone(),
                    kind: kind.clone(),
                })
            })
            .collect());
    }

    let typed_keys = lock
        .packages_typed()
        .map_err(OperationalError::from)?
        .into_iter()
        .map(|(key, _)| key)
        .chain(
            lock.workspace_packages_typed()
                .map_err(OperationalError::from)?
                .into_iter()
                .map(|(key, _)| key),
        )
        .collect::<Vec<_>>();
    let roots = if !lock.roots().is_empty() {
        lock.roots().to_vec()
    } else {
        let config = crate::registry::RegistryConfig::load(project_dir)
            .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?;
        crate::application::replay::replay_root_keys_with_config(
            lock,
            manifest,
            &typed_keys,
            &config,
        )
        .map_err(|error| OperationalError::new(ErrorKind::Lockfile, error))?
    };
    let mut result = Vec::new();
    for key in roots {
        let parsed = key
            .parse::<LockfilePackageKey>()
            .map_err(OperationalError::from)?;
        let name = parsed.name.to_string();
        if let Some(kind) = kinds.get(&name) {
            result.push(Root {
                key,
                name,
                kind: kind.clone(),
            });
        }
    }
    Ok(result)
}

fn workspace_roots(
    lock: &Lockfile,
    manifest: &PackageManifest,
    requested: &tapid_core::PackageName,
    project_dir: &Path,
    manifest_path: &Path,
) -> Result<Vec<Root>, OperationalError> {
    let member_dir = manifest_path.parent().ok_or_else(|| {
        OperationalError::new(
            ErrorKind::Manifest,
            "workspace manifest has no parent directory",
        )
    })?;
    let relative_path = member_dir
        .strip_prefix(project_dir)
        .map_err(|error| OperationalError::from_source(ErrorKind::Manifest, error))?
        .to_string_lossy()
        .replace('\\', "/");
    let member_bytes = fs::read(manifest_path).map_err(|error| {
        OperationalError::from_source(ErrorKind::Manifest, error)
            .context("cannot read selected workspace package.json")
    })?;
    let member_digest = digest_bytes(&member_bytes);
    let matched = lock
        .workspace_packages_typed()
        .map_err(OperationalError::from)?
        .into_iter()
        .find(|(key, package)| {
            key.name == *manifest.name()
                && key.version == manifest.version()
                && key.source.workspace().is_some_and(|source| {
                    source.path() == relative_path && package.manifest_digest() == member_digest
                })
        });
    let (key, workspace_package) = matched.ok_or_else(|| {
        OperationalError::new(
            ErrorKind::LockManifestMismatch,
            "selected workspace manifest or membership does not match tapid.lock",
        )
    })?;
    let kinds = direct_dependency_kinds(manifest);
    let mut roots = workspace_package
        .dependencies()
        .iter()
        .map(|(name, target)| Root {
            key: target.clone(),
            name: name.clone(),
            kind: kinds
                .get(name.as_str())
                .cloned()
                .unwrap_or_else(|| "dependency".to_owned()),
        })
        .collect::<Vec<_>>();
    if requested == manifest.name() {
        roots.push(Root {
            key: key.to_string(),
            name: manifest.name().to_string(),
            kind: "workspace".to_owned(),
        });
    }
    Ok(roots)
}

fn direct_dependency_kinds(manifest: &PackageManifest) -> BTreeMap<String, String> {
    [
        (manifest.dependencies(), "dependencies"),
        (manifest.dev_dependencies(), "devDependencies"),
        (manifest.optional_dependencies(), "optionalDependencies"),
    ]
    .into_iter()
    .flat_map(|(entries, kind)| {
        entries.keys().flat_map(move |name| {
            let normalized = crate::online::dep_parts(name)
                .map(|(_, package)| package.to_string())
                .unwrap_or_else(|_| name.clone());
            [
                (name.clone(), kind.to_owned()),
                (normalized, kind.to_owned()),
            ]
        })
    })
    .collect()
}

fn graph(lock: &Lockfile) -> Result<BTreeMap<String, Vec<Edge>>, OperationalError> {
    let mut graph = BTreeMap::new();
    for (key, package) in lock.packages_typed().map_err(OperationalError::from)? {
        graph.insert(
            key.to_string(),
            package
                .dependencies()
                .iter()
                .map(|(name, target)| Edge {
                    name: name.clone(),
                    target: target.clone(),
                })
                .collect(),
        );
    }
    for (key, package) in lock
        .workspace_packages_typed()
        .map_err(OperationalError::from)?
    {
        graph.insert(
            key.to_string(),
            package
                .dependencies()
                .iter()
                .map(|(name, target)| Edge {
                    name: name.clone(),
                    target: target.clone(),
                })
                .collect(),
        );
    }
    Ok(graph)
}

fn source_kind(key: &str) -> &'static str {
    match key.parse::<LockfilePackageKey>() {
        Ok(LockfilePackageKey {
            source: LockfilePackageSource::Workspace(_),
            ..
        }) => "workspace",
        _ => "registry",
    }
}

#[allow(clippy::too_many_arguments)]
fn search_paths(
    current: &str,
    requested: &tapid_core::PackageName,
    graph: &BTreeMap<String, Vec<Edge>>,
    source_kinds: &BTreeMap<String, &'static str>,
    visited: &mut BTreeSet<String>,
    path: &mut Vec<WhyStep>,
    state: &mut SearchState,
    depth: usize,
) {
    if state.visits >= MAX_VISITS {
        state.truncated = true;
        return;
    }
    state.visits += 1;
    let current_key = match current.parse::<LockfilePackageKey>() {
        Ok(key) => key,
        Err(_) => return,
    };
    if &current_key.name == requested {
        if state.paths.len() == MAX_PATHS {
            state.truncated = true;
        } else {
            state.paths.push(WhyPath {
                steps: path.clone(),
            });
        }
    }
    if depth >= MAX_DEPTH {
        if graph.get(current).is_some_and(|edges| !edges.is_empty()) {
            state.truncated = true;
        }
        return;
    }
    for edge in graph.get(current).into_iter().flatten() {
        if state.truncated {
            return;
        }
        if !visited.insert(edge.target.clone()) {
            continue;
        }
        let Some(source_kind) = source_kinds.get(&edge.target).copied() else {
            visited.remove(&edge.target);
            continue;
        };
        path.push(WhyStep {
            key: edge.target.clone(),
            via: Some(edge.name.clone()),
            edge_kind: Some("dependency".to_owned()),
            source_kind,
        });
        search_paths(
            &edge.target,
            requested,
            graph,
            source_kinds,
            visited,
            path,
            state,
            depth + 1,
        );
        path.pop();
        visited.remove(&edge.target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package_key(name: &str, version: &str) -> String {
        tapid_lockfile::LockedPackage::new_with_provenance(
            "https://registry.npmjs.org",
            name,
            version,
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "a".repeat(64)),
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap()
        .key()
    }

    #[test]
    fn traversal_is_cycle_safe_and_reports_duplicate_scoped_versions() {
        let root = package_key("root", "1.0.0");
        let middle = package_key("middle", "1.0.0");
        let target_one = package_key("@scope/target", "1.0.0");
        let target_two = package_key("@scope/target", "2.0.0");
        let graph = BTreeMap::from([
            (
                root.clone(),
                vec![
                    Edge {
                        name: "middle".into(),
                        target: middle.clone(),
                    },
                    Edge {
                        name: "target-v2".into(),
                        target: target_two.clone(),
                    },
                ],
            ),
            (
                middle.clone(),
                vec![
                    Edge {
                        name: "root".into(),
                        target: root.clone(),
                    },
                    Edge {
                        name: "target-v1".into(),
                        target: target_one.clone(),
                    },
                ],
            ),
            (
                target_one.clone(),
                vec![Edge {
                    name: "target-v2".into(),
                    target: target_two.clone(),
                }],
            ),
            (target_two.clone(), Vec::new()),
        ]);
        let source_kinds = graph.keys().map(|key| (key.clone(), "registry")).collect();
        let requested: tapid_core::PackageName = "@scope/target".parse().unwrap();
        let mut state = SearchState::default();
        let mut path = vec![WhyStep {
            key: root.clone(),
            via: Some("root".into()),
            edge_kind: Some("dependencies".into()),
            source_kind: "registry",
        }];
        search_paths(
            &root,
            &requested,
            &graph,
            &source_kinds,
            &mut BTreeSet::from([root.clone()]),
            &mut path,
            &mut state,
            0,
        );
        let final_keys = state
            .paths
            .iter()
            .map(|path| path.steps.last().unwrap().key.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            final_keys,
            BTreeSet::from([target_one.as_str(), target_two.as_str()])
        );
        assert_eq!(state.paths.len(), 3);
        assert!(state.paths.iter().any(|path| {
            path.steps.last().is_some_and(|step| step.key == target_two) && path.steps.len() == 4
        }));
        assert!(!state.truncated);
    }

    #[test]
    fn traversal_caps_path_results_and_marks_the_report_truncated() {
        let root = package_key("root", "1.0.0");
        let mut graph = BTreeMap::new();
        let mut edges = Vec::new();
        for version in 0..=MAX_PATHS {
            let target = package_key("target", &format!("1.0.{version}"));
            edges.push(Edge {
                name: format!("target-{version}"),
                target: target.clone(),
            });
            graph.insert(target, Vec::new());
        }
        graph.insert(root.clone(), edges);
        let source_kinds = graph.keys().map(|key| (key.clone(), "registry")).collect();
        let requested: tapid_core::PackageName = "target".parse().unwrap();
        let mut state = SearchState::default();
        search_paths(
            &root,
            &requested,
            &graph,
            &source_kinds,
            &mut BTreeSet::from([root.clone()]),
            &mut vec![WhyStep {
                key: root.clone(),
                via: Some("root".into()),
                edge_kind: Some("dependencies".into()),
                source_kind: "registry",
            }],
            &mut state,
            0,
        );
        assert_eq!(state.paths.len(), MAX_PATHS);
        assert!(state.truncated);
    }

    #[test]
    fn workspace_roots_require_matching_member_path_and_manifest_digest() {
        let project = tapid_test_support::TempProject::new("why-workspace-validation").unwrap();
        let root_bytes = br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
        let member_bytes =
            br#"{"name":"member","version":"1.0.0","dependencies":{"removed":"1.0.0"}}"#;
        project.write("package.json", root_bytes).unwrap();
        project
            .write("packages/member/package.json", member_bytes)
            .unwrap();
        let root_manifest = digest_bytes(root_bytes);
        let member_digest = digest_bytes(member_bytes);
        let manifest = PackageManifest::parse(std::str::from_utf8(member_bytes).unwrap()).unwrap();
        let source =
            tapid_lockfile::LocalWorkspaceSource::new("packages/member", "member", "1.0.0")
                .unwrap();
        let workspace =
            tapid_lockfile::LockedWorkspacePackage::new(source, &member_digest).unwrap();
        let mut lock = Lockfile::new(&root_manifest).unwrap();
        lock.insert_workspace_package(workspace).unwrap();
        let requested: tapid_core::PackageName = "member".parse().unwrap();
        let manifest_path = project.path().join("packages/member/package.json");
        let valid =
            workspace_roots(&lock, &manifest, &requested, project.path(), &manifest_path).unwrap();
        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0].kind, "workspace");

        let stale_source =
            tapid_lockfile::LocalWorkspaceSource::new("packages/member", "member", "1.0.0")
                .unwrap();
        let stale_workspace = tapid_lockfile::LockedWorkspacePackage::new(
            stale_source,
            &format!("sha256-{}", "f".repeat(64)),
        )
        .unwrap();
        let mut stale_lock = Lockfile::new(&root_manifest).unwrap();
        stale_lock
            .insert_workspace_package(stale_workspace)
            .unwrap();
        assert!(
            workspace_roots(
                &stale_lock,
                &manifest,
                &requested,
                project.path(),
                &manifest_path
            )
            .is_err()
        );
    }

    #[test]
    fn workspace_lock_identity_is_reported_as_workspace_source() {
        let source =
            tapid_lockfile::LocalWorkspaceSource::new("packages/lib", "lib", "1.0.0").unwrap();
        let key = tapid_lockfile::LockfilePackageKey::workspace(source).to_string();
        assert_eq!(source_kind(&key), "workspace");
    }

    #[test]
    fn direct_manifest_edges_keep_optional_and_normalize_prefixed_names_without_peer_roots() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","optionalDependencies":{"opt":"1.0.0"},"devDependencies":{"jsr:@scope/pkg":"1.0.0"},"peerDependencies":{"peer":"1.0.0"}}"#,
        )
        .unwrap();
        let kinds = direct_dependency_kinds(&manifest);
        assert_eq!(kinds["opt"], "optionalDependencies");
        assert_eq!(kinds["@scope/pkg"], "devDependencies");
        assert!(!kinds.contains_key("peer"));
    }
}
