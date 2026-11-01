use std::path::PathBuf;
use tapid_core::PackageName;
use tapid_linker::{
    InstanceKey, ManagedRoot, NamedLayoutInput, PlanError, Platform, ShimPackage, ShimStrategy,
};
use tapid_manifest::PackageManifest;

/// Exact command ownership, with authenticated instances kept separate from
/// mutable workspace identities. Neither variant confers launch authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CommandOwner {
    Authenticated(InstanceKey),
    /// Identity and placement only: workspace source bytes remain mutable.
    Workspace(tapid_linker::WorkspaceLink),
}

/// Selection facts; bind the relative entry to a verified live placement before launch.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SelectedCommand {
    pub install_name: PackageName,
    pub owner: CommandOwner,
    pub relative_entry: PathBuf,
}

#[derive(Debug)]
pub(crate) enum SelectionError {
    InvalidProjectSelection(String),
    ProjectEvidence(crate::application::outcome::OperationalError),
    InvalidPackageSelection(String),
    InvalidCommand,
    MissingCommand,
    Ambiguous(Vec<String>),
    InconsistentInstallation(String),
    InvalidMapping(PlanError),
}

impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProjectSelection(error) => f.write_str(error),
            Self::ProjectEvidence(error) => error.fmt(f),
            Self::InvalidPackageSelection(name) => {
                write!(f, "not an installed direct dependency binding: {name}")
            }
            Self::InvalidCommand => f.write_str("invalid binary command name"),
            Self::MissingCommand => f.write_str("no eligible dependency exports the command"),
            Self::Ambiguous(names) => write!(
                f,
                "command is ambiguous between bindings: {}",
                names.join(", ")
            ),
            Self::InconsistentInstallation(name) => {
                write!(f, "inconsistent installed evidence for binding: {name}")
            }
            Self::InvalidMapping(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SelectionError {}

/// Restrict command eligibility to one selected manifest while retaining every
/// instance and edge for the caller's complete-layout verification.
pub(super) fn select_manifest_command(
    input: &NamedLayoutInput,
    links: &tapid_linker::WorkspaceLinkPlan,
    manifest: &PackageManifest,
    package: Option<&str>,
    command: &str,
    platform: Platform,
) -> Result<SelectedCommand, SelectionError> {
    let names = |map: &std::collections::BTreeMap<String, String>| {
        map.keys()
            .map(|name| {
                crate::online::dep_parts(name)
                    .map(|(_, name)| name)
                    .map_err(SelectionError::InconsistentInstallation)
            })
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
    };
    let mut required = names(manifest.dependencies())?;
    required.extend(names(manifest.dev_dependencies())?);
    let mut eligible = required.clone();
    eligible.extend(names(manifest.optional_dependencies())?);
    if let Some(package) = package
        && !eligible.iter().any(|name| name.as_str() == package)
    {
        return Err(SelectionError::InvalidPackageSelection(package.into()));
    }
    for name in required {
        if !input
            .root_dependencies
            .iter()
            .any(|binding| binding.name == name)
            && !links.links.iter().any(|link| link.name == name)
        {
            return Err(SelectionError::InconsistentInstallation(format!(
                "declared dependency {name} has no installed binding"
            )));
        }
    }
    let mut scoped = input.clone();
    scoped
        .root_dependencies
        .retain(|binding| eligible.contains(&binding.name));
    // Native workspace reconstruction may repeat a binding shared by members.
    // Only identical bindings collapse; conflicting identities remain errors.
    scoped
        .root_dependencies
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.child.cmp(&b.child)));
    scoped.root_dependencies.dedup();
    let mut links = links.clone();
    links.links.retain(|link| eligible.contains(&link.name));
    select_command(&scoped, &links, package, command, platform)
}

/// Only root bindings are eligible; instances reachable solely by dependency
/// edges are deliberately excluded. Each binding is planned independently so
/// qualification can disambiguate aliases without weakening bin validation.
pub(super) fn select_command(
    input: &NamedLayoutInput,
    links: &tapid_linker::WorkspaceLinkPlan,
    package: Option<&str>,
    command: &str,
    platform: Platform,
) -> Result<SelectedCommand, SelectionError> {
    let query =
        serde_json::json!({"name":"selection", "version":"1.0.0", "bin":{(command):"entry.js"}});
    PackageManifest::parse(&query.to_string()).map_err(|_| SelectionError::InvalidCommand)?;
    if let Some(package) = package
        && !input
            .root_dependencies
            .iter()
            .any(|binding| binding.name.as_str() == package)
        && !links.links.iter().any(|link| link.name.as_str() == package)
    {
        return Err(SelectionError::InvalidPackageSelection(package.into()));
    }
    let strategy = match platform {
        Platform::Unix => ShimStrategy::UnixSymlink,
        Platform::Windows => ShimStrategy::WindowsCmdAndPowerShell,
        Platform::Other => {
            return Err(SelectionError::InvalidMapping(
                PlanError::UnsupportedPlatform(platform),
            ));
        }
    };
    let mut bindings = input.root_dependencies.iter().collect::<Vec<_>>();
    bindings.sort_by(|a, b| a.name.cmp(&b.name));
    let mut sources = Vec::new();
    for binding in bindings {
        if package.is_some_and(|package| package != binding.name.as_str()) {
            continue;
        }
        let mut instances = input
            .instances
            .iter()
            .filter(|instance| InstanceKey::from(*instance) == binding.child);
        let instance = instances
            .next()
            .ok_or_else(|| SelectionError::InconsistentInstallation(binding.name.to_string()))?;
        if instances.next().is_some() {
            return Err(SelectionError::InconsistentInstallation(
                binding.name.to_string(),
            ));
        }
        let root = crate::filesystem::tree::package_content_root(&instance.tree.root)
            .map_err(|_| SelectionError::InconsistentInstallation(binding.name.to_string()))?;
        sources.push((
            binding.name.clone(),
            CommandOwner::Authenticated(binding.child.clone()),
            root,
            instance.id.name.clone(),
            instance.id.version.clone(),
        ));
    }
    for link in &links.links {
        if package.is_some_and(|package| package != link.name.as_str()) {
            continue;
        }
        sources.push((
            link.name.clone(),
            CommandOwner::Workspace(link.clone()),
            link.source.clone(),
            link.name.clone(),
            link.version.clone(),
        ));
    }
    sources.sort_by(|a, b| a.0.cmp(&b.0));
    if sources.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(SelectionError::InconsistentInstallation(
            "duplicate command owner binding".into(),
        ));
    }
    let mut candidates = Vec::new();
    for (install_name, owner, root, name, version) in sources {
        let bytes =
            crate::filesystem::read::read_bounded_file(&root.join("package.json"), 1024 * 1024)
                .map_err(|_| SelectionError::InconsistentInstallation(install_name.to_string()))?;
        let json = String::from_utf8(bytes)
            .map_err(|_| SelectionError::InconsistentInstallation(install_name.to_string()))?;
        let manifest = PackageManifest::parse(&json).map_err(|error| {
            SelectionError::InvalidMapping(PlanError::InvalidPackageMetadata(error.to_string()))
        })?;
        if manifest.name() != &name || manifest.version() != version {
            return Err(SelectionError::InconsistentInstallation(
                install_name.to_string(),
            ));
        }
        let plan = tapid_linker::plan_shims(
            ManagedRoot::new(&root).map_err(SelectionError::InvalidMapping)?,
            vec![ShimPackage {
                tree_root: root.clone(),
                package_json: json,
                bin_dir: root.clone(),
            }],
            platform,
        )
        .map_err(SelectionError::InvalidMapping)?;
        let Some(bin) = manifest.bin() else { continue };
        let query_path = root.join(".bin").join(command);
        for target in bin.targets() {
            let exported = root.join(".bin").join(&target.command);
            if strategy
                .validate_targets([query_path.as_path(), exported.as_path()])
                .is_ok()
            {
                continue;
            }
            // Shim planning intentionally skips absent targets. Exec must retain
            // the declared mapping and diagnose its absence rather than select
            // another binding by accident.
            let entry = plan
                .entries
                .iter()
                .find(|entry| entry.command == target.command)
                .ok_or_else(|| {
                    SelectionError::InvalidMapping(PlanError::BinTargetMissing(
                        root.join(&target.target),
                    ))
                })?;
            validate_selected_entry(&owner, &entry.source)?;
            candidates.push(SelectedCommand {
                install_name: install_name.clone(),
                owner: owner.clone(),
                relative_entry: target.target.clone(),
            });
        }
    }
    match candidates.len() {
        0 => Err(SelectionError::MissingCommand),
        1 => Ok(candidates.remove(0)),
        _ => Err(SelectionError::Ambiguous(
            candidates
                .into_iter()
                .map(|candidate| candidate.install_name.to_string())
                .collect(),
        )),
    }
}

/// Workspace targets are mutable, but must remain the observed regular path;
/// a redirected parent must not acquire the selected member's identity.
pub(super) fn validate_selected_entry(
    owner: &CommandOwner,
    entry: &std::path::Path,
) -> Result<(), SelectionError> {
    if let CommandOwner::Workspace(link) = owner {
        let relative = entry.strip_prefix(&link.source).map_err(|_| {
            SelectionError::InvalidMapping(PlanError::PathOutsideManagedRoot(entry.to_path_buf()))
        })?;
        let root = std::fs::canonicalize(&link.source).map_err(|_| {
            SelectionError::InconsistentInstallation("workspace source is missing".into())
        })?;
        let canonical = std::fs::canonicalize(entry).map_err(|_| {
            SelectionError::InvalidMapping(PlanError::BinTargetMissing(entry.to_path_buf()))
        })?;
        if canonical != root.join(relative) {
            return Err(SelectionError::InvalidMapping(
                PlanError::PathOutsideManagedRoot(entry.to_path_buf()),
            ));
        }
    }
    tapid_linker::validate_node_entry(entry).map_err(SelectionError::InvalidMapping)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_core::{PackageInstanceId, PeerContext, PlatformContext};
    use tapid_linker::{
        InstanceKey, NamedDependency, NamedLayoutInput, PackageInstance, Platform,
        VerifiedTreeReference,
    };
    use tapid_test_support::TempProject;

    fn fixture() -> (TempProject, NamedLayoutInput) {
        let project = TempProject::new("installed-command-selection").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"actual","version":"1.0.0","bin":{"tool":"cli.js"}}"#,
            )
            .unwrap();
        project.write("cli.js", b"console.log('tool');\n").unwrap();
        let instance = PackageInstance {
            id: PackageInstanceId::new(
                "https://registry.npmjs.org".parse().unwrap(),
                "actual".parse().unwrap(),
                "1.0.0".parse().unwrap(),
            ),
            peer_context: PeerContext::default(),
            platform_context: PlatformContext::new(None, None, None).unwrap(),
            tree: VerifiedTreeReference::new(&format!("sha256-{}", "a".repeat(64)), project.path())
                .unwrap(),
        };
        let binding = |name: &str| NamedDependency {
            name: name.parse().unwrap(),
            child: InstanceKey::from(&instance),
        };
        let input = NamedLayoutInput {
            root_dependencies: vec![binding("second"), binding("alias")],
            instances: vec![instance],
            dependency_edges: vec![],
        };
        (project, input)
    }

    #[test]
    fn workspace_candidates_preserve_mutable_identity_and_collide_with_archive_owners() {
        let (_archive, mut input) = fixture();
        input
            .root_dependencies
            .retain(|binding| binding.name.as_str() == "alias");
        let project = TempProject::new("workspace-command-candidates").unwrap();
        project
            .write(
                "member/package.json",
                br#"{"name":"member","version":"1.0.0","bin":{"tool":"bin/cli.js"}}"#,
            )
            .unwrap();
        project
            .write("member/bin/cli.js", b"console.log(1);\n")
            .unwrap();
        let links = tapid_linker::plan_workspace_links(
            project.path(),
            [tapid_linker::WorkspacePackage {
                root: "member".into(),
                name: "member".parse().unwrap(),
                version: "1.0.0".parse().unwrap(),
            }],
        )
        .unwrap();
        assert!(
            matches!(select_command(&input, &links, None, "tool", Platform::Unix),
            Err(SelectionError::Ambiguous(names)) if names == ["alias", "member"])
        );
        let selected =
            select_command(&input, &links, Some("member"), "tool", Platform::Unix).unwrap();
        assert!(
            matches!(selected.owner, CommandOwner::Workspace(ref link) if link.source == project.path().join("member"))
        );
        assert_eq!(selected.relative_entry, PathBuf::from("bin/cli.js"));
        #[cfg(unix)]
        {
            std::fs::rename(
                project.path().join("member/bin"),
                project.path().join("member/redirected"),
            )
            .unwrap();
            std::os::unix::fs::symlink("redirected", project.path().join("member/bin")).unwrap();
            assert!(
                select_command(&input, &links, Some("member"), "tool", Platform::Unix).is_err(),
                "workspace entry parent redirection must fail before launch"
            );
        }
    }

    #[test]
    fn manifest_scope_excludes_other_members_and_preserves_aliases_and_optional_absence() {
        let (_project, input) = fixture();
        let root = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"alias":"npm:actual@1.0.0"},"optionalDependencies":{"absent":"*"}}"#,
        ).unwrap();
        let member = PackageManifest::parse(
            r#"{"name":"member","version":"1.0.0","devDependencies":{"second":"npm:actual@1.0.0"}}"#,
        ).unwrap();
        let root_command = select_manifest_command(
            &input,
            &Default::default(),
            &root,
            None,
            "tool",
            Platform::Unix,
        )
        .unwrap();
        assert_eq!(root_command.install_name.as_str(), "alias");
        let mut shared = input.clone();
        shared
            .root_dependencies
            .push(input.root_dependencies[1].clone());
        assert_eq!(
            select_manifest_command(
                &shared,
                &Default::default(),
                &root,
                None,
                "tool",
                Platform::Unix
            )
            .unwrap(),
            root_command,
        );
        let both = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"alias":"npm:actual@1.0.0","second":"npm:actual@1.0.0"}}"#,
        ).unwrap();
        assert!(matches!(
            select_manifest_command(&input, &Default::default(), &both, None, "tool", Platform::Unix),
            Err(SelectionError::Ambiguous(names)) if names == ["alias", "second"]
        ));

        let member_command = select_manifest_command(
            &input,
            &Default::default(),
            &member,
            None,
            "tool",
            Platform::Unix,
        )
        .unwrap();
        assert_eq!(member_command.install_name.as_str(), "second");
        assert!(matches!(
            select_manifest_command(
                &input,
                &Default::default(),
                &member,
                Some("alias"),
                "tool",
                Platform::Unix
            ),
            Err(SelectionError::InvalidPackageSelection(_))
        ));
        let mut missing = input.clone();
        missing
            .root_dependencies
            .retain(|binding| binding.name.as_str() != "alias");
        assert!(matches!(
            select_manifest_command(
                &missing,
                &Default::default(),
                &root,
                None,
                "tool",
                Platform::Unix
            ),
            Err(SelectionError::InconsistentInstallation(_))
        ));
    }

    #[test]
    fn aliases_are_distinct_candidates_and_qualification_preserves_owner() {
        let (project, mut input) = fixture();
        assert!(
            matches!(select_command(&input, &Default::default(), None, "tool", Platform::Unix), Err(SelectionError::Ambiguous(candidates)) if candidates == ["alias", "second"])
        );
        let selected = select_command(
            &input,
            &Default::default(),
            Some("alias"),
            "tool",
            Platform::Unix,
        )
        .unwrap();
        assert_eq!(selected.install_name.as_str(), "alias");
        assert!(
            matches!(&selected.owner, CommandOwner::Authenticated(owner) if owner.id.name.as_str() == "actual")
        );
        assert_eq!(selected.relative_entry, std::path::Path::new("cli.js"));
        input.root_dependencies.clear();
        assert!(matches!(
            select_command(&input, &Default::default(), None, "tool", Platform::Unix),
            Err(SelectionError::MissingCommand)
        ));
        assert!(matches!(
            select_command(
                &input,
                &Default::default(),
                Some("actual"),
                "tool",
                Platform::Unix
            ),
            Err(SelectionError::InvalidPackageSelection(_))
        ));
        assert_eq!(
            std::fs::read(project.path().join("cli.js")).unwrap(),
            b"console.log('tool');\n"
        );
    }

    #[test]
    fn reference_metadata_must_match_locked_owner() {
        let (project, input) = fixture();
        project
            .write(
                "package.json",
                br#"{"name":"impostor","version":"1.0.0","bin":{"tool":"cli.js"}}"#,
            )
            .unwrap();
        assert!(matches!(
            select_command(
                &input,
                &Default::default(),
                Some("alias"),
                "tool",
                Platform::Unix
            ),
            Err(SelectionError::InconsistentInstallation(_))
        ));
    }

    #[test]
    fn wrapped_reference_uses_the_materialized_content_root() {
        let (project, input) = fixture();
        std::fs::create_dir(project.path().join("package")).unwrap();
        for name in ["package.json", "cli.js"] {
            std::fs::rename(
                project.path().join(name),
                project.path().join("package").join(name),
            )
            .unwrap();
        }
        let selected = select_command(
            &input,
            &Default::default(),
            Some("alias"),
            "tool",
            Platform::Unix,
        )
        .unwrap();
        assert_eq!(selected.relative_entry, std::path::Path::new("cli.js"));
    }

    #[test]
    fn missing_export_missing_target_and_unsupported_entry_remain_distinct() {
        let (project, input) = fixture();
        assert!(matches!(
            select_command(
                &input,
                &Default::default(),
                Some("alias"),
                "absent",
                Platform::Unix
            ),
            Err(SelectionError::MissingCommand)
        ));
        assert!(matches!(
            select_command(&input, &Default::default(), None, "../tool", Platform::Unix),
            Err(SelectionError::InvalidCommand)
        ));
        project
            .write("cli.js", b"#!/bin/sh\necho hostile\n")
            .unwrap();
        assert!(matches!(
            select_command(
                &input,
                &Default::default(),
                Some("alias"),
                "tool",
                Platform::Unix
            ),
            Err(SelectionError::InvalidMapping(
                PlanError::UnsupportedBinExecutable(_)
            ))
        ));
        std::fs::remove_file(project.path().join("cli.js")).unwrap();
        assert!(matches!(
            select_command(&input, &Default::default(), None, "tool", Platform::Unix),
            Err(SelectionError::InvalidMapping(PlanError::BinTargetMissing(
                _
            )))
        ));
        assert!(!project.path().join("cli.js").exists());
        assert!(!project.path().join(".bin").exists());
    }

    #[test]
    fn windows_equivalent_exports_use_linker_collision_rules() {
        let (_project, mut input) = fixture();
        assert!(
            matches!(select_command(&input, &Default::default(), None, "TOOL", Platform::Windows), Err(SelectionError::Ambiguous(names)) if names == ["alias", "second"])
        );
        input.root_dependencies.reverse();
        assert!(
            matches!(select_command(&input, &Default::default(), None, "TOOL", Platform::Windows), Err(SelectionError::Ambiguous(names)) if names == ["alias", "second"])
        );
        assert!(
            select_command(
                &input,
                &Default::default(),
                Some("alias"),
                "TOOL",
                Platform::Windows
            )
            .is_ok()
        );
        assert!(matches!(
            select_command(
                &input,
                &Default::default(),
                Some("alias"),
                "TOOL",
                Platform::Unix
            ),
            Err(SelectionError::MissingCommand)
        ));
    }

    #[test]
    fn malformed_eligible_mapping_cannot_disappear_from_bare_selection() {
        let (project, mut input) = fixture();
        let invalid = TempProject::new("invalid-command-mapping").unwrap();
        invalid
            .write(
                "package.json",
                br#"{"name":"broken","version":"1.0.0","bin":{"tool":"../escape.js"}}"#,
            )
            .unwrap();
        let mut instance = input.instances[0].clone();
        instance.id.name = "broken".parse().unwrap();
        instance.tree.root = invalid.path().into();
        input.root_dependencies[0].child = InstanceKey::from(&instance);
        input.instances.push(instance);
        assert!(matches!(
            select_command(&input, &Default::default(), None, "tool", Platform::Unix),
            Err(SelectionError::InvalidMapping(
                PlanError::InvalidPackageMetadata(_)
            ))
        ));
        assert!(
            select_command(
                &input,
                &Default::default(),
                Some("alias"),
                "tool",
                Platform::Unix
            )
            .is_ok()
        );
        assert!(!project.path().join(".bin").exists());
        assert!(!invalid.path().join(".bin").exists());
    }
}
