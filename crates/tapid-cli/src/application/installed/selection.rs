use std::path::PathBuf;
use tapid_core::PackageName;
use tapid_linker::{
    InstanceKey, ManagedRoot, NamedLayoutInput, PlanError, Platform, ShimPackage, ShimStrategy,
};
use tapid_manifest::PackageManifest;

/// Static selection facts over authenticated reference trees, not live paths or
/// launch authority. The caller must validate the complete live layout and bind
/// this relative entry to its exact placement before preparing execution.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct SelectedCommand {
    pub install_name: PackageName,
    pub owner: InstanceKey,
    pub relative_entry: PathBuf,
}

#[derive(Debug)]
pub(super) enum SelectionError {
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

/// Only root bindings are eligible; instances reachable solely by dependency
/// edges are deliberately excluded. Each binding is planned independently so
/// qualification can disambiguate aliases without weakening bin validation.
#[allow(dead_code)] // Project snapshot and launch composition is still pending.
pub(super) fn select_command(
    input: &NamedLayoutInput,
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
    let mut candidates = Vec::new();
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
        let bytes =
            crate::filesystem::read::read_bounded_file(&root.join("package.json"), 1024 * 1024)
                .map_err(|_| SelectionError::InconsistentInstallation(binding.name.to_string()))?;
        let json = String::from_utf8(bytes)
            .map_err(|_| SelectionError::InconsistentInstallation(binding.name.to_string()))?;
        let manifest = PackageManifest::parse(&json).map_err(|error| {
            SelectionError::InvalidMapping(PlanError::InvalidPackageMetadata(error.to_string()))
        })?;
        if manifest.name() != &instance.id.name || manifest.version() != instance.id.version {
            return Err(SelectionError::InconsistentInstallation(
                binding.name.to_string(),
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
            tapid_linker::validate_node_entry(&entry.source)
                .map_err(SelectionError::InvalidMapping)?;
            candidates.push(SelectedCommand {
                install_name: binding.name.clone(),
                owner: binding.child.clone(),
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
    fn aliases_are_distinct_candidates_and_qualification_preserves_owner() {
        let (project, mut input) = fixture();
        assert!(
            matches!(select_command(&input, None, "tool", Platform::Unix), Err(SelectionError::Ambiguous(candidates)) if candidates == ["alias", "second"])
        );
        let selected = select_command(&input, Some("alias"), "tool", Platform::Unix).unwrap();
        assert_eq!(selected.install_name.as_str(), "alias");
        assert_eq!(selected.owner.id.name.as_str(), "actual");
        assert_eq!(selected.relative_entry, std::path::Path::new("cli.js"));
        input.root_dependencies.clear();
        assert!(matches!(
            select_command(&input, None, "tool", Platform::Unix),
            Err(SelectionError::MissingCommand)
        ));
        assert!(matches!(
            select_command(&input, Some("actual"), "tool", Platform::Unix),
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
            select_command(&input, Some("alias"), "tool", Platform::Unix),
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
        let selected = select_command(&input, Some("alias"), "tool", Platform::Unix).unwrap();
        assert_eq!(selected.relative_entry, std::path::Path::new("cli.js"));
    }

    #[test]
    fn missing_export_missing_target_and_unsupported_entry_remain_distinct() {
        let (project, input) = fixture();
        assert!(matches!(
            select_command(&input, Some("alias"), "absent", Platform::Unix),
            Err(SelectionError::MissingCommand)
        ));
        assert!(matches!(
            select_command(&input, None, "../tool", Platform::Unix),
            Err(SelectionError::InvalidCommand)
        ));
        project
            .write("cli.js", b"#!/bin/sh\necho hostile\n")
            .unwrap();
        assert!(matches!(
            select_command(&input, Some("alias"), "tool", Platform::Unix),
            Err(SelectionError::InvalidMapping(
                PlanError::UnsupportedBinExecutable(_)
            ))
        ));
        std::fs::remove_file(project.path().join("cli.js")).unwrap();
        assert!(matches!(
            select_command(&input, None, "tool", Platform::Unix),
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
            matches!(select_command(&input, None, "TOOL", Platform::Windows), Err(SelectionError::Ambiguous(names)) if names == ["alias", "second"])
        );
        input.root_dependencies.reverse();
        assert!(
            matches!(select_command(&input, None, "TOOL", Platform::Windows), Err(SelectionError::Ambiguous(names)) if names == ["alias", "second"])
        );
        assert!(select_command(&input, Some("alias"), "TOOL", Platform::Windows).is_ok());
        assert!(matches!(
            select_command(&input, Some("alias"), "TOOL", Platform::Unix),
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
            select_command(&input, None, "tool", Platform::Unix),
            Err(SelectionError::InvalidMapping(
                PlanError::InvalidPackageMetadata(_)
            ))
        ));
        assert!(select_command(&input, Some("alias"), "tool", Platform::Unix).is_ok());
        assert!(!project.path().join(".bin").exists());
        assert!(!invalid.path().join(".bin").exists());
    }
}
