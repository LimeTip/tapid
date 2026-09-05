use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Stable discovery endpoint(s), tried in the given order.
    #[arg(long = "endpoint", value_name = "HTTPS_URL")]
    pub(crate) endpoints: Vec<String>,
    /// Optional trusted release keyring JSON. Uses the embedded production trust root by default.
    #[arg(long)]
    pub(crate) keyring: Option<PathBuf>,
    /// Destination executable (defaults to the current executable).
    #[arg(long)]
    pub(crate) destination: Option<PathBuf>,
    /// Inspect and verify without replacing the executable.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

pub(crate) fn run(args: Args) -> ExitCode {
    match crate::application::upgrade::run(
        &args.endpoints,
        args.keyring.as_deref(),
        args.destination.as_deref(),
        args.dry_run,
    ) {
        Ok(report) => {
            if report.dry_run {
                println!(
                    "Verified stable Tapid {} for {}; dry-run did not replace {}",
                    report.version,
                    report.target,
                    report.destination.display()
                );
            } else {
                println!("Upgraded Tapid to {}", report.version);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}
