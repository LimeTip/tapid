use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};
use tapid_manifest::DependencyKind;

#[derive(Debug, ClapArgs)]
pub(crate) struct AddArgs {
    pub(crate) packages: Vec<String>,
    #[arg(long, conflicts_with_all = ["optional", "peer"])]
    pub(crate) dev: bool,
    #[arg(long, conflicts_with_all = ["dev", "peer"])]
    pub(crate) optional: bool,
    #[arg(long, conflicts_with_all = ["dev", "optional"])]
    pub(crate) peer: bool,
    #[command(flatten)]
    pub(crate) common: CommonArgs,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct RemoveArgs {
    pub(crate) packages: Vec<String>,
    #[command(flatten)]
    pub(crate) common: CommonArgs,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct UpdateArgs {
    pub(crate) packages: Vec<String>,
    #[arg(long)]
    pub(crate) latest: bool,
    #[command(flatten)]
    pub(crate) common: CommonArgs,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct ReadOnlyArgs {
    #[command(flatten)]
    pub(crate) common: CommonArgs,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct CommonArgs {
    #[arg(long, default_value = ".")]
    pub(crate) project_dir: PathBuf,
    #[arg(long)]
    pub(crate) workspace: Option<String>,
    #[arg(long)]
    pub(crate) store_dir: Option<PathBuf>,
    #[arg(long)]
    pub(crate) registry_fixture: Option<PathBuf>,
    #[arg(long)]
    pub(crate) allow_unverified_registry_artifacts: bool,
}

pub(crate) fn add(args: AddArgs) -> ExitCode {
    if args.packages.is_empty() {
        eprintln!("error: add requires at least one package");
        return ExitCode::from(1);
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
    let result = mutate_and_install(&args.common, |manifest| {
        crate::application::lifecycle::plan_add(manifest, &mutations)
    });
    report(result, "Added dependencies")
}

pub(crate) fn remove(args: RemoveArgs) -> ExitCode {
    if args.packages.is_empty() {
        eprintln!("error: remove requires at least one package");
        return ExitCode::from(1);
    }
    let result = mutate_and_install(&args.common, |manifest| {
        crate::application::lifecycle::plan_remove(manifest, &args.packages)
    });
    report(result, "Removed dependencies")
}

pub(crate) fn update(args: UpdateArgs) -> ExitCode {
    let result = mutate_and_install(&args.common, |manifest| {
        let plan =
            crate::application::lifecycle::plan_update(manifest, &args.packages, args.latest)?;
        if args.latest {
            crate::application::lifecycle::plan_add(manifest, &plan.mutations)
        } else {
            Ok(plan)
        }
    });
    report(result, "Updated dependencies")
}

pub(crate) fn outdated(args: ReadOnlyArgs) -> ExitCode {
    if let Err(error) =
        crate::application::lifecycle::parse_workspace_selector(args.common.workspace.as_deref())
    {
        eprintln!("error: {error}");
        return ExitCode::from(1);
    }
    match crate::application::lifecycle::outdated_report(
        &args.common.project_dir,
        args.common.registry_fixture.as_deref(),
    ) {
        Ok(entries) => {
            for entry in entries {
                println!(
                    "{}",
                    crate::application::lifecycle::format_outdated_entry(&entry)
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

pub(crate) fn prune(args: ReadOnlyArgs) -> ExitCode {
    if let Err(error) =
        crate::application::lifecycle::parse_workspace_selector(args.common.workspace.as_deref())
    {
        eprintln!("error: {error}");
        return ExitCode::from(1);
    }
    match crate::application::install::run(
        &args.common.project_dir,
        None,
        args.common.store_dir.as_deref(),
        crate::application::install::InstallMode::Frozen,
        None,
        false,
        |_, _| {},
    ) {
        Ok(report) => {
            println!("Pruned ({} package(s))", report.package_count);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn mutate_and_install(
    common: &CommonArgs,
    planner: impl FnOnce(
        &tapid_manifest::PackageManifest,
    ) -> Result<crate::application::lifecycle::LifecyclePlan, String>,
) -> Result<crate::application::install::InstallReport, String> {
    crate::application::lifecycle::parse_workspace_selector(common.workspace.as_deref())?;
    let path = common.project_dir.join("package.json");
    let manifest = crate::commands::manifest::read_manifest(&path)?;
    let plan = planner(&manifest)?;
    crate::application::install::run_with_manifest(
        &common.project_dir,
        Some(&plan.manifest),
        None,
        common.store_dir.as_deref(),
        crate::application::install::InstallMode::Online,
        common.registry_fixture.as_deref(),
        common.allow_unverified_registry_artifacts,
        |_, _| {},
    )
}

fn report(
    result: Result<crate::application::install::InstallReport, String>,
    action: &str,
) -> ExitCode {
    match result {
        Ok(report) => {
            println!("{action} ({} package(s))", report.package_count);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
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
