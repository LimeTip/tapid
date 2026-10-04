use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Legacy signed discovery URL. Repeat to try endpoints in order; cannot be combined with --release-url.
    #[arg(long = "endpoint", value_name = "HTTPS_URL")]
    pub(crate) endpoints: Vec<String>,
    /// Signed release record URL. Defaults to tapid.dev; TAPID_RELEASE_RECORD_URL overrides the default.
    #[arg(long, value_name = "HTTPS_URL", conflicts_with_all = ["endpoints", "keyring"])]
    pub(crate) release_url: Option<String>,
    /// Trusted release keyring JSON for controlled testing. Defaults to the embedded production keyring.
    #[arg(long)]
    pub(crate) keyring: Option<PathBuf>,
    /// Executable path to replace. Defaults to the currently running Tapid executable.
    #[arg(long)]
    pub(crate) destination: Option<PathBuf>,
    /// Download and verify the selected release without replacing the executable.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

pub(crate) fn run(args: Args) -> ExitCode {
    match crate::application::upgrade::run(
        &args.endpoints,
        args.release_url.as_deref(),
        args.keyring.as_deref(),
        args.destination.as_deref(),
        args.dry_run,
    ) {
        Ok(report) => {
            if report.recovered {
                println!(
                    "Warning: using the cached release; could not check the latest stable release"
                );
            }
            if report.dry_run {
                println!(
                    "{} Tapid {} for {}; dry-run did not replace {}",
                    if !report.verification_known {
                        "Warning: provenance unknown"
                    } else if report.signature_verified {
                        "Verified"
                    } else {
                        "Warning: checksum-only"
                    },
                    report.version,
                    report.target,
                    report.destination.display()
                );
            } else {
                if !report.verification_known {
                    println!(
                        "Warning: verification provenance is unknown for the recovered artifact"
                    );
                } else if !report.signature_verified {
                    println!(
                        "Release checksum verified; independent signature verification was not performed"
                    );
                }
                if report.recovered {
                    if report.already_current {
                        println!(
                            "Tapid {} already matches the cached release; executable unchanged",
                            report.version
                        );
                    } else {
                        println!("Restored Tapid {} from the cached release", report.version);
                    }
                } else if report.already_current {
                    println!("Tapid {} is already up to date", report.version);
                } else {
                    println!("Upgraded Tapid to {}", report.version);
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}
