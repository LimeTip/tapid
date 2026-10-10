use crate::application::outcome::{ErrorKind, OperationFailure, OperationalError};
use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};
use tapid_manifest::DependencyKind;

#[derive(Debug, ClapArgs)]
pub(crate) struct AddArgs {
    /// Packages to add, such as react@^19.0.0. Defaults to * without a requirement.
    #[arg(value_name = "PACKAGE")]
    pub(crate) packages: Vec<String>,
    #[arg(
        long,
        conflicts_with_all = ["optional", "peer"],
        help = "Add dependencies to devDependencies"
    )]
    pub(crate) dev: bool,
    #[arg(
        long,
        conflicts_with_all = ["dev", "peer"],
        help = "Add dependencies to optionalDependencies"
    )]
    pub(crate) optional: bool,
    #[arg(
        long,
        conflicts_with_all = ["dev", "optional"],
        help = "Record requirements in peerDependencies without installing them as regular dependencies"
    )]
    pub(crate) peer: bool,
    #[command(flatten)]
    pub(crate) common: CommonArgs,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct RemoveArgs {
    /// Declared package names to remove, without version requirements.
    #[arg(value_name = "PACKAGE")]
    pub(crate) packages: Vec<String>,
    #[command(flatten)]
    pub(crate) common: CommonArgs,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct UpdateArgs {
    /// Declared package names to select. Omit to select all direct dependencies.
    #[arg(value_name = "PACKAGE")]
    pub(crate) packages: Vec<String>,
    #[arg(
        long,
        help = "Replace selected version requirements with * in package.json and resolve again"
    )]
    pub(crate) latest: bool,
    #[command(flatten)]
    pub(crate) common: CommonArgs,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct OutdatedArgs {
    #[command(flatten)]
    pub(crate) common: CommonArgs,
    /// Maximum JSON entries, default 100. Use 0 to include all entries. Requires --json.
    #[arg(long)]
    pub(crate) json_limit: Option<usize>,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct ReadOnlyArgs {
    #[command(flatten)]
    pub(crate) common: CommonArgs,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct CommonArgs {
    #[arg(
        long,
        value_name = "PATH",
        default_value = ".",
        help = "Project directory containing package.json and tapid.lock"
    )]
    pub(crate) project_dir: PathBuf,
    #[arg(
        long,
        value_name = "NAME",
        help = "Select workspace member by name; default is the manifest in --project-dir"
    )]
    pub(crate) workspace: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Verified package store directory; defaults to tapid/store in the platform cache directory"
    )]
    pub(crate) store_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Read registry metadata from a local JSON fixture for tests or air-gapped development"
    )]
    pub(crate) registry_fixture: Option<PathBuf>,
    #[arg(
        long,
        help = "Permit npm metadata without registry-declared integrity; resulting installs cannot use offline/frozen replay"
    )]
    pub(crate) allow_unverified_registry_artifacts: bool,
}

pub(crate) fn add(args: AddArgs, json: bool) -> ExitCode {
    if args.packages.is_empty() {
        return crate::output::json::failure_or_human(
            &OperationFailure::unchanged(
                &args.common.project_dir,
                OperationalError::new(
                    ErrorKind::InvalidRequest,
                    "add requires at least one package",
                ),
            ),
            "add",
            json,
        );
    }
    let kind = if args.dev {
        DependencyKind::DevDependencies
    } else if args.optional {
        DependencyKind::OptionalDependencies
    } else if args.peer {
        DependencyKind::PeerDependencies
    } else {
        DependencyKind::Dependencies
    };
    let mutations = args
        .packages
        .iter()
        .map(|spec| {
            let (name, requirement) = crate::package_spec::parse(spec);
            crate::application::lifecycle::DependencyMutation {
                name: name.to_owned(),
                requirement: Some(requirement.to_owned()),
                kind,
            }
        })
        .collect::<Vec<_>>();
    let result = mutate_and_install(&args.common, json, |manifest| {
        crate::application::lifecycle::plan_add(manifest, &mutations)
    });
    report(result, json, "add", "Added dependencies")
}

pub(crate) fn remove(args: RemoveArgs, json: bool) -> ExitCode {
    if args.packages.is_empty() {
        return crate::output::json::failure_or_human(
            &OperationFailure::unchanged(
                &args.common.project_dir,
                OperationalError::new(
                    ErrorKind::InvalidRequest,
                    "remove requires at least one package",
                ),
            ),
            "remove",
            json,
        );
    }
    let result = mutate_and_install(&args.common, json, |manifest| {
        crate::application::lifecycle::plan_remove(manifest, &args.packages)
    });
    report(result, json, "remove", "Removed dependencies")
}

pub(crate) fn update(args: UpdateArgs, json: bool) -> ExitCode {
    let result = mutate_and_install(&args.common, json, |manifest| {
        let plan =
            crate::application::lifecycle::plan_update(manifest, &args.packages, args.latest)?;
        if args.latest {
            crate::application::lifecycle::plan_add(manifest, &plan.mutations)
        } else {
            Ok(plan)
        }
    });
    report(result, json, "update", "Updated dependencies")
}

pub(crate) fn outdated(args: OutdatedArgs, json: bool) -> ExitCode {
    match crate::application::lifecycle::outdated_report(
        &args.common.project_dir,
        args.common.workspace.as_deref(),
        args.common.registry_fixture.as_deref(),
    ) {
        Ok(report) => {
            if json {
                return crate::output::json::outdated(
                    &report,
                    "outdated",
                    args.json_limit.unwrap_or(100),
                );
            }
            crate::output::report_warnings(&report.outcome.warnings);
            for entry in report.entries {
                println!(
                    "{}",
                    crate::application::lifecycle::format_outdated_entry(&entry)
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => crate::output::json::failure_or_human(&error, "outdated", json),
    }
}

pub(crate) fn prune(args: ReadOnlyArgs, json: bool) -> ExitCode {
    let selection = match crate::application::lifecycle::resolve_workspace(
        &args.common.project_dir,
        args.common.workspace.as_deref(),
    ) {
        Ok(selection) => selection,
        Err(error) => {
            return crate::output::json::failure_or_human(
                &OperationFailure::unchanged(&args.common.project_dir, error),
                "prune",
                json,
            );
        }
    };
    match crate::application::install::run_with_manifest_target(
        &selection.root_dir,
        &selection.manifest_path,
        None,
        None,
        args.common.store_dir.as_deref(),
        crate::application::install::InstallMode::Frozen,
        None,
        false,
        |event| crate::output::report_progress(event, json),
    ) {
        Ok(report) => {
            if json {
                return crate::output::json::installed(&report, "prune");
            }
            crate::output::report_warnings(&report.outcome.warnings);
            println!("Pruned ({} package(s))", report.package_count);
            ExitCode::SUCCESS
        }
        Err(error) => crate::output::json::failure_or_human(&error, "prune", json),
    }
}

fn mutate_and_install(
    common: &CommonArgs,
    json: bool,
    planner: impl FnOnce(
        &tapid_manifest::PackageManifest,
    ) -> Result<crate::application::lifecycle::LifecyclePlan, OperationalError>,
) -> Result<crate::application::install::InstallReport, OperationFailure> {
    let selection = crate::application::lifecycle::resolve_workspace(
        &common.project_dir,
        common.workspace.as_deref(),
    )
    .map_err(|error| OperationFailure::unchanged(&common.project_dir, error))?;
    let plan = planner(&selection.manifest)
        .map_err(|error| OperationFailure::unchanged(&selection.root_dir, error))?;
    crate::application::install::run_with_manifest_target(
        &selection.root_dir,
        &selection.manifest_path,
        Some(&plan.manifest),
        None,
        common.store_dir.as_deref(),
        crate::application::install::InstallMode::Online,
        common.registry_fixture.as_deref(),
        common.allow_unverified_registry_artifacts,
        |event| crate::output::report_progress(event, json),
    )
}

fn report(
    result: Result<crate::application::install::InstallReport, OperationFailure>,
    json: bool,
    operation: &str,
    action: &str,
) -> ExitCode {
    match result {
        Ok(report) => {
            if json {
                return crate::output::json::installed(&report, operation);
            }
            crate::output::report_warnings(&report.outcome.warnings);
            println!("{action} ({} package(s))", report.package_count);
            ExitCode::SUCCESS
        }
        Err(error) => crate::output::json::failure_or_human(&error, operation, json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn peer_add_records_peer_without_installing_it_as_a_regular_root() {
        let project = std::env::temp_dir().join(format!(
            "tapid-peer-add-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&project).unwrap();
        let package = project.join("package.json");
        let fixture = project.join("registry.json");
        fs::write(&package, r#"{"name":"app","version":"1.0.0"}"#).unwrap();
        fs::write(
            &fixture,
            r#"{"packages":[{"registry":"https://jsr.io","name":"@scope/peer","version":"1.0.0","artifact":"https://jsr.io/@scope/peer/1.0.0.tgz"}]}"#,
        )
        .unwrap();

        let result = add(
            AddArgs {
                packages: vec!["react@^18.0.0".into()],
                dev: false,
                optional: false,
                peer: true,
                common: CommonArgs {
                    project_dir: project.clone(),
                    workspace: None,
                    store_dir: Some(project.join("store")),
                    registry_fixture: Some(fixture),
                    allow_unverified_registry_artifacts: false,
                },
            },
            false,
        );

        assert_eq!(result, ExitCode::SUCCESS);
        let manifest = crate::commands::manifest::read_manifest(&package).unwrap();
        assert!(!manifest.dependencies().contains_key("react"));
        assert_eq!(
            manifest
                .peer_dependencies()
                .get("react")
                .map(String::as_str),
            Some("^18.0.0")
        );
        assert!(project.join("tapid.lock").is_file());
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn formats_all_outdated_fields_without_fabricating_missing_versions() {
        let entry = crate::application::lifecycle::OutdatedEntry {
            identity: "npm:foo".into(),
            kind: "dependencies".into(),
            declared: "^1.0.0".into(),
            locked: Some("1.0.0".into()),
            newest_compatible: Some("1.4.0".into()),
            newest_available: None,
            diagnostic: Some("registry metadata unavailable".into()),
        };
        assert_eq!(
            crate::application::lifecycle::format_outdated_entry(&entry),
            "npm:foo [dependencies] declared=^1.0.0 locked=1.0.0 compatible=1.4.0 available=unavailable diagnostic=registry metadata unavailable"
        );
    }
}
