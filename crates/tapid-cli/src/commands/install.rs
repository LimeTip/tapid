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
    /// Store root containing verified trees.
    #[arg(long)]
    pub(crate) store_dir: Option<PathBuf>,
    /// Local JSON registry fixture used by tests and air-gapped development.
    #[arg(long)]
    pub(crate) registry_fixture: Option<PathBuf>,
}

pub(crate) fn run(args: Args) -> ExitCode {
    if args.allow_unverified_registry_artifacts && !args.offline && !args.frozen {
        eprintln!(
            "warning: npm artifacts without registry integrity are not authenticated against a registry-declared digest"
        );
    }
    let result = crate::application::install::run(
        &args.project_dir,
        args.package.as_deref(),
        args.store_dir.as_deref(),
        args.offline,
        args.frozen,
        args.registry_fixture.as_deref(),
        args.allow_unverified_registry_artifacts,
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
