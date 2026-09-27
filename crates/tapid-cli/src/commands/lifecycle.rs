use clap::Args as ClapArgs;
use std::{fs, path::PathBuf, process::ExitCode};
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
    match crate::application::lifecycle::parse_workspace_selector(args.common.workspace.as_deref())
        .and_then(|_| {
            crate::commands::manifest::read_manifest(&args.common.project_dir.join("package.json"))
        }) {
        Ok(manifest) => {
            for (name, requirement) in manifest
                .dependencies()
                .iter()
                .chain(manifest.dev_dependencies().iter())
                .chain(manifest.optional_dependencies().iter())
                .chain(manifest.peer_dependencies().iter())
            {
                println!(
                    "{name}\t{:?}\t{requirement}\tunknown\tunknown\tunknown",
                    manifest.dependency_kind(name)
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
    match crate::application::lifecycle::parse_workspace_selector(args.common.workspace.as_deref())
    {
        Ok(None) => match crate::application::install::run(
            &args.common.project_dir,
            None,
            args.common.store_dir.as_deref(),
            crate::application::install::InstallMode::Offline,
            None,
            false,
            |_, _| {},
        ) {
            Ok(report) => {
                println!(
                    "Pruned materialized dependencies ({} package(s))",
                    report.package_count
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        },
        Ok(Some(_)) => unreachable!(),
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
    let original = fs::read(&path).map_err(|error| format!("cannot read package.json: {error}"))?;
    let manifest = crate::commands::manifest::read_manifest(&path)?;
    let plan = planner(&manifest)?;
    fs::write(&path, plan.manifest.to_json())
        .map_err(|error| format!("cannot update package.json: {error}"))?;
    let result = crate::application::install::run(
        &common.project_dir,
        None,
        common.store_dir.as_deref(),
        crate::application::install::InstallMode::Online,
        common.registry_fixture.as_deref(),
        common.allow_unverified_registry_artifacts,
        |_, _| {},
    );
    if result.is_err() {
        let _ = fs::write(&path, original);
    }
    result
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
