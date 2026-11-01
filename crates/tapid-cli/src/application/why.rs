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
    kind: &'static str,
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
    let native = serde_json::from_str::<serde_json::Value>(&lock_bytes)
        .ok()
        .is_some_and(|value| value["lockfileVersion"] == tapid_lockfile::PROFILE_LOCKFILE_VERSION);
    let (roots, graph) = if native {
        native_graph(&lock_bytes, &selection, &root_digest, &requested)?
    } else {
        let lock = Lockfile::from_json(&lock_bytes).map_err(OperationalError::from)?;
        lock.validate_replay(&root_digest)
            .map_err(|error| OperationalError::from(error).at_manifest(&root_manifest_path))?;

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
        (roots, graph)
    };
    let mut state = SearchState::default();
    for root in roots {
        let Some(source_kind) = graph.nodes.get(&root.key).map(source_kind) else {
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
            &mut visited,
            &mut path,
            &mut state,
            0,
        );
        if state.truncated {
            break;
        }
    }

    let warnings = if native {
        vec!["paths describe the complete selection, including packages omitted from the installed profile; installation and source integrity are not checked".to_owned()]
    } else if state.paths.iter().any(|path| {
        path.steps
            .iter()
            .skip(1)
            .any(|step| step.edge_kind.as_deref() == Some("dependency"))
    }) {
        vec!["transitive dependency edge kinds are not recorded in the lockfile; those edges are labeled dependency".to_owned()]
    } else {
        Vec::new()
    };
    let outcome = if state.truncated {
        "partial"
    } else if state.paths.is_empty() {
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

fn native_graph(
    input: &str,
    workspace: &super::lifecycle::WorkspaceSelection,
    root_digest: &str,
    requested: &tapid_core::PackageName,
) -> Result<(Vec<Root>, WhyGraph), OperationalError> {
    use tapid_lockfile::{
        InstallProfile, NativeDeclarationKind as DeclarationKind,
        NativeDependencyKind as DependencyKind, NativeSelection,
    };
    let selection = NativeSelection::from_json(input)?;
    let discovered = tapid_manifest::Workspace::discover_typed(&workspace.root_dir)
        .map_err(OperationalError::from)?;
    let mut digests = BTreeMap::new();
    for member in discovered.members() {
        let path = member.path();
        let relative = path
            .parent()
            .and_then(|path| path.strip_prefix(&workspace.root_dir).ok())
            .ok_or_else(|| {
                OperationalError::new(ErrorKind::Manifest, "workspace member is outside project")
            })?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(path)
            .map_err(|error| OperationalError::from_source(ErrorKind::Manifest, error))?;
        digests.insert(relative, digest_bytes(&bytes));
    }
    selection.validate_replay(root_digest, &digests)?;
    let selected_scope = if workspace.manifest_path == workspace.root_dir.join("package.json") {
        None
    } else {
        Some(
            workspace
                .manifest_path
                .parent()
                .expect("manifest parent")
                .strip_prefix(&workspace.root_dir)
                .map_err(|error| OperationalError::from_source(ErrorKind::Manifest, error))?
                .to_string_lossy()
                .replace('\\', "/"),
        )
    };
    let mut roots = selection
        .materialized_declarations(InstallProfile::default())
        .filter(|edge| edge.workspace == selected_scope)
        .map(|edge| Root {
            key: edge.target.clone(),
            name: edge.name.clone(),
            kind: match edge.kind {
                DeclarationKind::Required => "dependencies",
                DeclarationKind::Dev => "devDependencies",
                DeclarationKind::Optional => "optionalDependencies",
            }
            .into(),
        })
        .collect::<Vec<_>>();
    roots.sort_by(|a, b| (&a.name, &a.kind, &a.key).cmp(&(&b.name, &b.kind, &b.key)));
    let mut graph = WhyGraph {
        nodes: BTreeMap::new(),
        edges: BTreeMap::new(),
    };
    for (key, package) in selection.packages() {
        let identity = key.parse::<LockfilePackageKey>()?;
        graph.nodes.insert(key.clone(), identity.clone());
        let mut edges = package
            .dependencies
            .iter()
            .map(|(name, edge)| Edge {
                name: name.clone(),
                target: edge.target.clone(),
                kind: match edge.kind {
                    DependencyKind::Required => "dependencies",
                    DependencyKind::Optional => "optionalDependencies",
                },
            })
            .chain(package.peer_providers.iter().map(|(name, target)| Edge {
                name: name.clone(),
                target: target.clone(),
                kind: "peer",
            }))
            .collect::<Vec<_>>();
        if let Some(source) = identity.source.workspace() {
            edges.extend(
                selection
                    .materialized_declarations(InstallProfile::default())
                    .filter(|edge| edge.workspace.as_deref() == Some(source.path()))
                    .map(|edge| Edge {
                        name: edge.name.clone(),
                        target: edge.target.clone(),
                        kind: match edge.kind {
                            DeclarationKind::Required => "dependencies",
                            DeclarationKind::Dev => "devDependencies",
                            DeclarationKind::Optional => "optionalDependencies",
                        },
                    }),
            );
            if selected_scope.as_deref() == Some(source.path()) && &identity.name == requested {
                roots.push(Root {
                    key: key.clone(),
                    name: identity.name.to_string(),
                    kind: "workspace".into(),
                });
            }
        }
        graph.edges.insert(key.clone(), edges);
    }
    Ok((roots, graph))
}

fn direct_roots(
    lock: &Lockfile,
    manifest: &PackageManifest,
    project_dir: &Path,
) -> Result<Vec<Root>, OperationalError> {
    let kinds = direct_dependency_kinds(manifest);
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
    let config = crate::registry::RegistryConfig::load(project_dir)
        .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?;
    let roots = crate::application::replay::replay_root_keys_with_config(
        lock,
        manifest,
        &typed_keys,
        &config,
    )
    .map_err(|error| OperationalError::new(ErrorKind::Lockfile, error))?;
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
    manifest
        .dependency_sections()
        .flat_map(|(kind, entries)| {
            let kind = kind.field_name();
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

/// Explain immediate dependency and peer edges using the same graph as `why`.
pub(super) fn requiring_edges(
    lock: &Lockfile,
) -> Result<BTreeMap<String, Vec<String>>, OperationalError> {
    let mut reasons = BTreeMap::<String, Vec<String>>::new();
    for (parent, edges) in graph(lock)?.edges {
        let parent = parent.replace('|', " ");
        for edge in edges {
            reasons
                .entry(edge.target)
                .or_default()
                .push(format!("{parent} through {} '{}'", edge.kind, edge.name));
        }
    }
    Ok(reasons)
}

struct WhyGraph {
    edges: BTreeMap<String, Vec<Edge>>,
    nodes: BTreeMap<String, LockfilePackageKey>,
}

fn graph(lock: &Lockfile) -> Result<WhyGraph, OperationalError> {
    let mut graph: BTreeMap<String, Vec<Edge>> = BTreeMap::new();
    let mut nodes = BTreeMap::new();
    for (key, package) in lock.packages_typed().map_err(OperationalError::from)? {
        nodes.insert(key.to_string(), key.clone());
        graph.insert(
            key.to_string(),
            package
                .dependencies()
                .iter()
                .map(|(name, target)| Edge {
                    name: name.clone(),
                    target: target.clone(),
                    kind: "dependency",
                })
                .collect(),
        );
    }
    for (key, package) in lock
        .workspace_packages_typed()
        .map_err(OperationalError::from)?
    {
        nodes.insert(key.to_string(), key.clone());
        graph.insert(
            key.to_string(),
            package
                .dependencies()
                .iter()
                .map(|(name, target)| Edge {
                    name: name.clone(),
                    target: target.clone(),
                    kind: "dependency",
                })
                .collect(),
        );
    }
    let mut providers = BTreeMap::<_, Vec<&LockfilePackageKey>>::new();
    for key in nodes.values() {
        providers
            .entry((&key.source, &key.name, &key.version))
            .or_default()
            .push(key);
    }
    for (encoded, source) in &nodes {
        let peer_context = source.peer_context()?;
        for (name, version) in peer_context.entries() {
            for target in providers
                .get(&(&source.source, name, version))
                .into_iter()
                .flatten()
            {
                graph
                    .get_mut(encoded)
                    .expect("indexed graph node")
                    .push(Edge {
                        name: name.to_string(),
                        target: target.to_string(),
                        kind: "peer",
                    });
            }
        }
    }
    Ok(WhyGraph {
        edges: graph,
        nodes,
    })
}

fn source_kind(key: &LockfilePackageKey) -> &'static str {
    match &key.source {
        LockfilePackageSource::Workspace(_) => "workspace",
        LockfilePackageSource::Copied(_) => "copied",
        _ => "registry",
    }
}

#[allow(clippy::too_many_arguments)]
fn search_paths(
    current: &str,
    requested: &tapid_core::PackageName,
    graph: &WhyGraph,
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
    let Some(current_key) = graph.nodes.get(current) else {
        return;
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
        if graph
            .edges
            .get(current)
            .is_some_and(|edges| !edges.is_empty())
        {
            state.truncated = true;
        }
        return;
    }
    for edge in graph.edges.get(current).into_iter().flatten() {
        if state.truncated {
            return;
        }
        if !visited.insert(edge.target.clone()) {
            continue;
        }
        let Some(source_kind) = graph.nodes.get(&edge.target).map(source_kind) else {
            visited.remove(&edge.target);
            continue;
        };
        path.push(WhyStep {
            key: edge.target.clone(),
            via: Some(edge.name.clone()),
            edge_kind: Some(edge.kind.to_owned()),
            source_kind,
        });
        search_paths(
            &edge.target,
            requested,
            graph,
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

    #[test]
    fn native_why_explains_complete_selection_with_typed_optional_edges() {
        use serde_json::json;
        let project = tapid_test_support::TempProject::new("why-native").unwrap();
        let manifest =
            br#"{"name":"root","version":"1.0.0","devDependencies":{"dev-alias":"npm:dev@1.0.0"}}"#;
        project.write("package.json", manifest).unwrap();
        let dev = package_key("dev", "1.0.0");
        let optional = package_key("optional", "1.0.0");
        let package = |dependencies| {
            json!({
                "sourceEvidence": {"state":"pending", "artifactUrl":"https://registry.npmjs.org/pkg.tgz",
                    "artifactIntegrity":format!("sha512-{}==", "A".repeat(86))},
                "dependencies": dependencies, "peerProviders":{}
            })
        };
        let lock = json!({
            "lockfileVersion":11, "resolverVersion":"0", "linkerVersion":"0",
            "rootManifestDigest":digest_bytes(manifest), "workspaceManifestDigests":{},
            "declarations":[{"workspace":null,"name":"dev-alias","kind":"dev","target":dev}],
            "packages":{
                dev.clone():package(json!({"optional-alias":{"kind":"optional","target":optional}})),
                optional.clone():package(json!({}))
            }
        }).to_string();
        project.write("tapid.lock", lock.as_bytes()).unwrap();
        // An installed layout is deliberately unnecessary: why explains selection,
        // including packages omitted from production materialization.
        let report = explain(project.path(), None, "optional").unwrap();
        assert_eq!(report.paths.len(), 1);
        let steps = &report.paths[0].steps;
        assert_eq!(steps[0].via.as_deref(), Some("dev-alias"));
        assert_eq!(steps[0].edge_kind.as_deref(), Some("devDependencies"));
        assert_eq!(steps[1].key, optional);
        assert_eq!(steps[1].via.as_deref(), Some("optional-alias"));
        assert_eq!(steps[1].edge_kind.as_deref(), Some("optionalDependencies"));
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("complete selection"))
        );
        assert_eq!(
            fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            lock
        );
        assert!(!project.path().join("node_modules").exists());
        project
            .write("package.json", br#"{"name":"root","version":"2.0.0"}"#)
            .unwrap();
        assert!(explain(project.path(), None, "optional").is_err());
    }

    #[test]
    fn native_why_traverses_workspace_declarations_and_validates_all_members() {
        use serde_json::json;
        let project = tapid_test_support::TempProject::new("why-native-workspace").unwrap();
        let root = br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"member":"1.0.0"}}"#;
        let member = br#"{"name":"member","version":"1.0.0","devDependencies":{"tool":"1.0.0"}}"#;
        project.write("package.json", root).unwrap();
        project
            .write("packages/member/package.json", member)
            .unwrap();
        let source =
            tapid_lockfile::LocalWorkspaceSource::new("packages/member", "member", "1.0.0")
                .unwrap();
        let key = LockfilePackageKey::workspace(source.clone()).to_string();
        let tool = package_key("tool", "1.0.0");
        let lock = json!({
            "lockfileVersion":11,"resolverVersion":"0","linkerVersion":"0",
            "rootManifestDigest":digest_bytes(root),
            "workspaceManifestDigests":{"packages/member":digest_bytes(member)},
            "declarations":[
                {"workspace":null,"name":"member","kind":"required","target":key},
                {"workspace":"packages/member","name":"tool","kind":"dev","target":tool}
            ],
            "packages":{
                key.clone():{"sourceEvidence":{"state":"workspace","source":source},"dependencies":{},"peerProviders":{}},
                tool.clone():{"sourceEvidence":{"state":"pending","artifactUrl":"https://registry.npmjs.org/tool.tgz",
                    "artifactIntegrity":format!("sha512-{}==", "A".repeat(86))},"dependencies":{},"peerProviders":{}}
            }
        }).to_string();
        project.write("tapid.lock", lock.as_bytes()).unwrap();
        let report = explain(project.path(), None, "tool").unwrap();
        assert_eq!(report.paths.len(), 1);
        assert_eq!(report.paths[0].steps.len(), 2);
        assert_eq!(report.paths[0].steps[0].source_kind, "workspace");
        assert_eq!(
            report.paths[0].steps[1].edge_kind.as_deref(),
            Some("devDependencies")
        );
        let report = explain(project.path(), Some("member"), "tool").unwrap();
        assert_eq!(report.paths.len(), 1);
        assert_eq!(report.paths[0].steps.len(), 1);
        let report = explain(project.path(), Some("member"), "member").unwrap();
        assert_eq!(report.paths.len(), 1);
        assert_eq!(
            report.paths[0].steps[0].edge_kind.as_deref(),
            Some("workspace")
        );
        project
            .write(
                "packages/member/package.json",
                br#"{"name":"member","version":"2.0.0"}"#,
            )
            .unwrap();
        assert!(explain(project.path(), None, "tool").is_err());
        assert_eq!(
            fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            lock
        );
    }

    #[test]
    fn native_why_uses_exact_peer_provider_instead_of_matching_all_contexts() {
        use serde_json::json;
        use tapid_core::{PeerContext, PlatformContext};
        let project = tapid_test_support::TempProject::new("why-native-peers").unwrap();
        let manifest = br#"{"name":"root","version":"1.0.0","dependencies":{"consumer":"1.0.0"}}"#;
        project.write("package.json", manifest).unwrap();
        let provider = package_key("provider", "1.0.0");
        let other = LockfilePackageKey::new(
            "https://registry.npmjs.org".parse().unwrap(),
            "provider".parse().unwrap(),
            "1.0.0".parse().unwrap(),
            &PeerContext::default(),
            &PlatformContext::new(Some("linux"), None, None).unwrap(),
        )
        .to_string();
        let consumer = LockfilePackageKey::new(
            "https://registry.npmjs.org".parse().unwrap(),
            "consumer".parse().unwrap(),
            "1.0.0".parse().unwrap(),
            &PeerContext::default().with("provider".parse().unwrap(), "1.0.0".parse().unwrap()),
            &PlatformContext::new(None, None, None).unwrap(),
        )
        .to_string();
        let package = |providers| {
            json!({
                "sourceEvidence":{"state":"pending","artifactUrl":"https://registry.npmjs.org/pkg.tgz",
                    "artifactIntegrity":format!("sha512-{}==", "A".repeat(86))},
                "dependencies":{},"peerProviders":providers
            })
        };
        let lock = json!({
            "lockfileVersion":11,"resolverVersion":"0","linkerVersion":"0",
            "rootManifestDigest":digest_bytes(manifest),"workspaceManifestDigests":{},
            "declarations":[{"workspace":null,"name":"consumer","kind":"required","target":consumer}],
            "packages":{
                consumer.clone():package(json!({"provider":provider})),
                provider.clone():package(json!({})), other.clone():package(json!({}))
            }
        }).to_string();
        project.write("tapid.lock", lock.as_bytes()).unwrap();
        let report = explain(project.path(), None, "provider").unwrap();
        assert_eq!(report.paths.len(), 1);
        let step = report.paths[0].steps.last().unwrap();
        assert_eq!(step.key, provider);
        assert_eq!(step.edge_kind.as_deref(), Some("peer"));
        assert_eq!(step.via.as_deref(), Some("provider"));
        assert_eq!(
            fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            lock
        );
    }

    #[test]
    fn peer_edges_retain_all_matching_provider_contexts() {
        use tapid_core::{PeerContext, PlatformContext};
        use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
        let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
        let empty = PeerContext::default();
        let peers =
            PeerContext::default().with("provider".parse().unwrap(), "1.0.0".parse().unwrap());
        let generic = PlatformContext::new(None, None, None).unwrap();
        let linux = PlatformContext::new(Some("linux"), None, None).unwrap();
        let mut providers = BTreeSet::new();
        let mut consumer = String::new();
        for (name, peer, platform) in [
            ("provider", &empty, &generic),
            ("provider", &empty, &linux),
            ("consumer", &peers, &generic),
        ] {
            let package = LockedPackage::new_with_context_and_provenance(
                "https://registry.npmjs.org",
                name,
                "1.0.0",
                &format!("sha512-{}==", "A".repeat(86)),
                &format!("sha256-{}", "a".repeat(64)),
                (peer, platform),
                RegistryIntegrityProvenance::RegistryDeclared,
            )
            .unwrap();
            if name == "provider" {
                providers.insert(package.key());
            } else {
                consumer = package.key();
            }
            lock.insert_package(package).unwrap();
        }
        let graph = graph(&lock).unwrap();
        let targets = graph.edges[&consumer]
            .iter()
            .filter(|edge| edge.kind == "peer")
            .map(|edge| edge.target.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(targets, providers);
    }

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
                        kind: "dependency",
                    },
                    Edge {
                        name: "target-v2".into(),
                        target: target_two.clone(),
                        kind: "dependency",
                    },
                ],
            ),
            (
                middle.clone(),
                vec![
                    Edge {
                        name: "root".into(),
                        target: root.clone(),
                        kind: "dependency",
                    },
                    Edge {
                        name: "target-v1".into(),
                        target: target_one.clone(),
                        kind: "dependency",
                    },
                ],
            ),
            (
                target_one.clone(),
                vec![Edge {
                    name: "target-v2".into(),
                    target: target_two.clone(),
                    kind: "dependency",
                }],
            ),
            (target_two.clone(), Vec::new()),
        ]);
        let graph = WhyGraph {
            nodes: graph
                .keys()
                .map(|key| (key.clone(), key.parse().unwrap()))
                .collect(),
            edges: graph,
        };
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
        let mut graph: BTreeMap<String, Vec<Edge>> = BTreeMap::new();
        let mut edges = Vec::new();
        for version in 0..=MAX_PATHS {
            let target = package_key("target", &format!("1.0.{version}"));
            edges.push(Edge {
                name: format!("target-{version}"),
                target: target.clone(),
                kind: "dependency",
            });
            graph.insert(target, Vec::new());
        }
        graph.insert(root.clone(), edges);
        let graph = WhyGraph {
            nodes: graph
                .keys()
                .map(|key| (key.clone(), key.parse().unwrap()))
                .collect(),
            edges: graph,
        };
        let requested: tapid_core::PackageName = "target".parse().unwrap();
        let mut state = SearchState::default();
        search_paths(
            &root,
            &requested,
            &graph,
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
        assert_eq!(source_kind(&key.parse().unwrap()), "workspace");
    }

    #[test]
    fn direct_manifest_edges_keep_optional_peer_and_normalize_prefixed_names() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","optionalDependencies":{"opt":"1.0.0"},"devDependencies":{"jsr:@scope/pkg":"1.0.0"},"peerDependencies":{"peer":"1.0.0"}}"#,
        )
        .unwrap();
        let kinds = direct_dependency_kinds(&manifest);
        assert_eq!(kinds["opt"], "optionalDependencies");
        assert_eq!(kinds["@scope/pkg"], "devDependencies");
        assert_eq!(kinds["peer"], "peerDependencies");
    }
}
