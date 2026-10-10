use crate::commands::{self, Cli};
use clap::Parser;
use std::process::ExitCode;

pub(crate) mod install;
pub(crate) mod installer;
pub(crate) mod lifecycle;
pub(crate) mod outcome;
mod release_record;
pub(crate) mod release_verification;
pub(crate) mod replay;
pub(crate) mod upgrade;

pub(crate) fn run() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let json_requested = arguments
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json");
    match Cli::try_parse_from(arguments) {
        Ok(cli) => commands::dispatch(cli.command, cli.json),
        Err(error) if json_requested => {
            let informational = matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            );
            crate::output::json::protocol_error(
                "parse",
                if informational {
                    "JSON_HELP_UNSUPPORTED"
                } else {
                    "ARGUMENT_INVALID"
                },
                if informational { 0 } else { 2 },
            )
        }
        Err(error) => error.exit(),
    }
}
