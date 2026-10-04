use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Optional package name to add to dependencies before installation.
    pub(crate) package: Option<String>,
    #[arg(long)]
    pub(crate) offline: bool,
    #[arg(long)]
    pub(crate) frozen: bool,
    /// Permit npm metadata without registry-declared integrity. Not allowed with --offline or --frozen.
    #[arg(long)]
    pub(crate) allow_unverified_registry_artifacts: bool,
    #[arg(long, default_value = ".")]
    pub(crate) project_dir: PathBuf,
    /// Select a workspace member while installing the root workspace graph.
    #[arg(long, value_name = "NAME")]
    pub(crate) workspace: Option<String>,
    /// Store root containing verified trees.
    #[arg(long)]
    pub(crate) store_dir: Option<PathBuf>,
    /// Local JSON registry fixture used by tests and air-gapped development.
    #[arg(long)]
    pub(crate) registry_fixture: Option<PathBuf>,
}

pub(crate) fn run(args: Args) -> ExitCode {
    let target_manifest_path = if let Some(name) = args.workspace.as_deref() {
        let workspace = match tapid_manifest::Workspace::discover(&args.project_dir) {
            Ok(workspace) => workspace,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        match workspace.select_path(Some(name)) {
            Ok(path) => path.to_path_buf(),
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        }
    } else {
        PathBuf::from("package.json")
    };
    if args.allow_unverified_registry_artifacts && !args.offline && !args.frozen {
        eprintln!(
            "warning: npm artifacts without registry integrity are not authenticated against a registry-declared digest"
        );
    }
    let mode = if args.offline {
        crate::application::install::InstallMode::Offline
    } else if args.frozen {
        crate::application::install::InstallMode::Frozen
    } else {
        crate::application::install::InstallMode::Online
    };
    let result = crate::application::install::run_with_manifest_target(
        &args.project_dir,
        &target_manifest_path,
        None,
        args.package.as_deref(),
        args.store_dir.as_deref(),
        mode,
        args.registry_fixture.as_deref(),
        args.allow_unverified_registry_artifacts,
        |completed, total| eprintln!("Replay snapshot progress: {completed}/{total}"),
    );
    match result {
        Ok(report) => {
            if report.replayed {
                println!("Replayed lockfile: {} package(s)", report.package_count);
            } else {
                println!("Installed {} package(s)", report.package_count);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}
