use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(flatten)]
    profile: super::profile::ProfileArgs,
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

/// Replay native locked selections and suppress all progress when returning JSON.
pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    let result = crate::application::install::run(
        crate::application::install::InstallRequest {
            profile: args.profile.effective(),
            project_dir: &args.project_dir,
            target_manifest_path: std::path::Path::new("package.json"),
            mutation: crate::application::install::ManifestMutation::None,
            store_root: args.store_dir.as_deref(),
            mode: if args.offline {
                crate::application::install::InstallMode::CiOffline
            } else {
                crate::application::install::InstallMode::Ci
            },
            registry_fixture: args.registry_fixture.as_deref(),
            allow_unverified_registry_artifacts: false,
        },
        |event| match event {
            crate::application::install::Progress::Replay(completed, total) if !json => {
                eprintln!("Locked install progress: {completed}/{total}")
            }
            event => crate::output::report_progress(event, json),
        },
    );
    match result {
        Ok(report) => {
            if json {
                return crate::output::json::installed(&report, "ci");
            }
            crate::output::report_warnings(&report.outcome.warnings);
            crate::output::install::report_profile(&report);
            println!(
                "Installed from lockfile: {} package(s)",
                report.package_count
            );
            ExitCode::SUCCESS
        }
        Err(error) => crate::output::json::failure_or_human(&error, "ci", json),
    }
}
