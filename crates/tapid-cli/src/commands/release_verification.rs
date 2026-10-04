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
