mod graph;

use crate::application::outcome::{
    ErrorKind, OperationFailure, OperationOutcome, OperationalError,
};
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
    /// Inspect an explicit persisted dependency graph without project or store access.
    Inspect(graph::InspectArgs),
    /// Compare two explicit persisted lock snapshots, separating selections and evidence.
    Compare(graph::CompareArgs),
    /// Check the format and internal consistency of tapid.lock in the current directory.
    #[command(
        long_about = "Read tapid.lock from the current directory and validate its schema and internal consistency. Does not verify package files in the store or compare the lockfile with package.json.",
        after_help = "Example:
  tapid lock verify"
    )]
    Verify,
}

/// Validate lockfile schema and consistency without recovering or changing project outputs.
pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    match args.command {
        Command::Inspect(args) => graph::inspect(args, json),
        Command::Compare(args) => graph::compare(args, json),
        Command::Verify => {
            let path = PathBuf::from("tapid.lock");
            match fs::read_to_string(&path)
                .map_err(|error| {
                    let kind = if error.kind() == std::io::ErrorKind::NotFound {
                        ErrorKind::LockfileMissing
                    } else {
                        ErrorKind::Lockfile
                    };
                    OperationalError::from_source(kind, error)
                })
                .and_then(|input| {
                    if serde_json::from_str::<serde_json::Value>(&input)
                        .ok()
                        .is_some_and(|v| v["lockfileVersion"] == 8)
                    {
                        tapid_lockfile::ImportedNpmLockfile::from_json(&input)
                            .map(|_| ())
                            .map_err(|error| {
                                OperationalError::from_source(ErrorKind::Lockfile, error)
                            })
                    } else {
                        Lockfile::from_json(&input).map(|_| ()).map_err(|error| {
                            OperationalError::from_source(ErrorKind::Lockfile, error)
                        })
                    }
                }) {
                Ok(_) => {
                    if json {
                        return crate::output::json::completed(
                            &OperationOutcome::unchanged(std::path::Path::new(".")),
                            "lock",
                            serde_json::json!({"action":"verify", "valid":true}),
                        );
                    }
                    println!("Valid lockfile: {}", path.display());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    if json {
                        return crate::output::json::failure_or_human(
                            &OperationFailure::unchanged(std::path::Path::new("."), error),
                            "lock",
                            true,
                        );
                    }
                    eprintln!("error: cannot verify {}: {error}", path.display());
                    ExitCode::from(1)
                }
            }
        }
    }
}
