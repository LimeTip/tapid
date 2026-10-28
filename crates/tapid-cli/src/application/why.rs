use crate::{
    application::{
        lifecycle::resolve_workspace,
        outcome::{ErrorKind, OperationalError},
    },
    filesystem::atomic::digest_bytes,
};
use serde::Serialize;
use std::{collections::BTreeMap, fs, path::Path};
use tapid_lockfile::{Lockfile, LockfilePackageKey};
use tapid_manifest::PackageManifest;

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

use tapid_lockfile::{LockedGraphPath as WhyPath, LockedGraphRoot as Root};

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
    let graph = lock.graph().map_err(OperationalError::from)?;
    let state = graph.paths_to(&roots, &requested);

    let warnings = if state.paths.iter().any(|path| {
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
    for (parent, edges) in lock.graph().map_err(OperationalError::from)?.edges() {
        let parent = parent.replace('|', " ");
        for edge in edges {
            reasons
                .entry(edge.target.clone())
                .or_default()
                .push(format!("{parent} through {} '{}'", edge.kind, edge.name));
        }
    }
    Ok(reasons)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut providers = std::collections::BTreeSet::new();
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
        lock.set_roots([consumer.clone()]).unwrap();
        let graph = lock.graph().unwrap();
        let targets = graph.edges()[&consumer]
            .iter()
            .filter(|edge| edge.kind == "peer")
            .map(|edge| edge.target.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(targets, providers);
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
