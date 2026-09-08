use crate::commands::{self, Cli};
use clap::Parser;
use std::process::ExitCode;

pub(crate) mod install;
pub(crate) mod replay;
pub(crate) mod upgrade;

pub(crate) fn run() -> ExitCode {
    commands::dispatch(Cli::parse().command)
}
