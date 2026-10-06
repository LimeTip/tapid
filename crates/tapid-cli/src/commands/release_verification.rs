use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    record: PathBuf,
    sidecar: PathBuf,
    /// Explicit keyring for controlled offline fixtures. Installers never set this.
    #[arg(long, hide = true)]
    keyring: Option<PathBuf>,
}

pub(crate) fn run(args: Args) -> ExitCode {
    match crate::application::release_verification::verify(
        &args.record,
        &args.sidecar,
        args.keyring.as_deref(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("release record signature verification failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, clap::Args)]
pub(crate) struct InstallArgs {
    version: String,
    minimum: String,
    archive: PathBuf,
    destination: PathBuf,
}

pub(crate) fn prepare_install(args: InstallArgs) -> ExitCode {
    match crate::application::installer::prepare(
        &args.version,
        &args.minimum,
        &args.archive,
        &args.destination,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("installer release policy rejected: {error}");
            ExitCode::FAILURE
        }
    }
}
