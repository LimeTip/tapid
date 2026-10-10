use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Project root containing package.json and tapid.lock.
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// Verified package store directory.
    #[arg(long)]
    store_dir: Option<PathBuf>,
    /// Require all verified trees in the store; do not download missing packages.
    #[arg(long)]
    offline: bool,
    /// Local JSON registry fixture for tests and air-gapped development.
    #[arg(long)]
    registry_fixture: Option<PathBuf>,
}

pub(crate) fn run(args: Args) -> ExitCode {
    let result = crate::application::install::run(
        &args.project_dir,
        None,
        args.store_dir.as_deref(),
        if args.offline {
            crate::application::install::InstallMode::CiOffline
        } else {
            crate::application::install::InstallMode::Ci
        },
        args.registry_fixture.as_deref(),
        false,
        |completed, total| eprintln!("Locked install progress: {completed}/{total}"),
    );
    match result {
        Ok(report) => {
            crate::output::report_warnings(&report.outcome.warnings);
            println!(
                "Installed from lockfile: {} package(s)",
                report.package_count
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            crate::output::report_failure(&error);
            ExitCode::from(1)
        }
    }
}
