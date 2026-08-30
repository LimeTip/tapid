use clap::{Parser, Subcommand};
use std::process::ExitCode;

pub(crate) mod init;
pub(crate) mod install;
pub(crate) mod lock;
pub(crate) mod manifest;
pub(crate) mod run;
pub(crate) mod upgrade;

#[derive(Debug, Parser)]
#[command(
    name = "tapid",
    version,
    about = "A deterministic JavaScript and TypeScript package manager"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Create a private package.json manifest.
    Init(init::Args),
    Manifest(manifest::Args),
    Lock(lock::Args),
    /// Run a root package script.
    Run(run::Args),
    /// Upgrade Tapid from a verified stable release.
    Upgrade(upgrade::Args),
    /// Install dependencies, optionally adding one package first.
    #[command(alias = "i")]
    Install(install::Args),
}

pub(crate) fn dispatch(command: Option<Command>) -> ExitCode {
    match command {
        None => {
            println!("Run 'tapid --help' for usage");
            ExitCode::SUCCESS
        }
        Some(Command::Init(args)) => init::run(args),
        Some(Command::Manifest(args)) => manifest::run(args),
        Some(Command::Lock(args)) => lock::run(args),
        Some(Command::Run(args)) => run::run(args),
        Some(Command::Upgrade(args)) => upgrade::run(args),
        Some(Command::Install(args)) => install::run(args),
    }
}
