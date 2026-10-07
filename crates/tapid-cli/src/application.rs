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
    commands::dispatch(Cli::parse().command)
}
