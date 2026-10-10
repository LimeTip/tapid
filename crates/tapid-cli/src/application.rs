use crate::commands::{self, Cli};
use clap::{CommandFactory, FromArgMatches, Parser};
use std::process::ExitCode;

mod dependency_scripts;
pub(crate) mod explain;
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
    let parsed = if json_requested {
        Cli::command()
            .color(clap::ColorChoice::Never)
            .try_get_matches_from(arguments)
            .and_then(|matches| Cli::from_arg_matches(&matches))
    } else {
        Cli::try_parse_from(arguments)
    };
    let parsed = parsed.and_then(|cli| {
        cli.validate_json_options()?;
        Ok(cli)
    });
    match parsed {
        Ok(cli) => commands::dispatch(cli.command, cli.json),
        Err(error) if json_requested => match error.kind() {
            clap::error::ErrorKind::DisplayHelp => crate::output::json::information(
                "help",
                serde_json::json!({"text": error.to_string()}),
            ),
            clap::error::ErrorKind::DisplayVersion => crate::output::json::information(
                "version",
                serde_json::json!({"name": "tapid", "version": env!("CARGO_PKG_VERSION")}),
            ),
            _ => crate::output::json::protocol_error("parse", "ARGUMENT_INVALID", 2),
        },
        Err(error) => error.exit(),
    }
}
