use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Package to add to dependencies, such as react@^19.0.0. Defaults to * without a requirement. Online only.
    #[arg(value_parser = parse_package_argument)]
    pub(crate) package: Option<String>,
    /// Replay tapid.lock without network access. Requires a matching manifest and all verified trees in the store.
    #[arg(long)]
    pub(crate) offline: bool,
    /// Require lockfile replay without re-resolution or network access. Currently the same behavior as --offline.
    #[arg(long)]
    pub(crate) frozen: bool,
    /// Permit npm metadata without registry-declared integrity. Not allowed with --offline or --frozen.
    #[arg(long)]
    pub(crate) allow_unverified_registry_artifacts: bool,
    /// Project directory containing package.json and tapid.lock.
    #[arg(long, default_value = ".")]
    pub(crate) project_dir: PathBuf,
    /// Verified package store directory. Defaults to tapid/store in the platform cache directory.
    #[arg(long)]
    pub(crate) store_dir: Option<PathBuf>,
    /// Local JSON registry fixture used by tests and air-gapped development.
    #[arg(long)]
    pub(crate) registry_fixture: Option<PathBuf>,
}

/// Rejects bare command words before project access while preserving explicit package specs.
fn parse_package_argument(value: &str) -> Result<String, String> {
    match value.trim() {
        "help" => Err(
            "'help' is ambiguous here. Use 'tapid install --help' or 'tapid help install' for usage; use an explicit package spec such as 'help@1.0.0' to install that package."
                .into(),
        ),
        "install" => Err(
            "'install' is ambiguous here. Use 'tapid install' to install project dependencies; use an explicit package spec such as 'install@1.0.0' to install that package."
                .into(),
        ),
        _ => Ok(value.into()),
    }
}

/// Runs installation or lockfile replay and reports progress, warnings, and the outcome.
pub(crate) fn run(args: Args) -> ExitCode {
    let mode = if args.offline {
        crate::application::install::InstallMode::Offline
    } else if args.frozen {
        crate::application::install::InstallMode::Frozen
    } else {
        crate::application::install::InstallMode::Online
    };
    let result = crate::application::install::run(
        &args.project_dir,
        args.package.as_deref(),
        args.store_dir.as_deref(),
        mode,
        args.registry_fixture.as_deref(),
        args.allow_unverified_registry_artifacts,
        |completed, total| eprintln!("Replay snapshot progress: {completed}/{total}"),
    );
    match result {
        Ok(report) => {
            crate::output::report_warnings(&report.outcome.warnings);
            if report.replayed {
                println!("Replayed lockfile: {} package(s)", report.package_count);
            } else {
                println!("Installed {} package(s)", report.package_count);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            crate::output::report_failure(&error);
            ExitCode::from(1)
        }
    }
}
