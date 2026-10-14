use clap::{Args as ClapArgs, Subcommand};
use std::{fs, path::PathBuf, process::ExitCode};
use tapid_lockfile::Lockfile;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Check the format and internal consistency of tapid.lock in the current directory.
    #[command(
        long_about = "Read tapid.lock from the current directory and validate its schema and internal consistency. Does not verify package files in the store or compare the lockfile with package.json.",
        after_help = "Example:
  tapid lock verify"
    )]
    Verify,
}

pub(crate) fn run(args: Args) -> ExitCode {
    match args.command {
        Command::Verify => {
            let path = PathBuf::from("tapid.lock");
            match fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|input| {
                    if serde_json::from_str::<serde_json::Value>(&input)
                        .ok()
                        .is_some_and(|v| v["lockfileVersion"] == 8)
                    {
                        tapid_lockfile::ImportedNpmLockfile::from_json(&input)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    } else {
                        Lockfile::from_json(&input)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    }
                }) {
                Ok(_) => {
                    println!("Valid lockfile: {}", path.display());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("error: cannot verify {}: {error}", path.display());
                    ExitCode::from(1)
                }
            }
        }
    }
}
