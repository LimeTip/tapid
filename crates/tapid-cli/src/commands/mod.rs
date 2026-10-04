use clap::{Parser, Subcommand};
use std::process::ExitCode;

pub(crate) mod init;
pub(crate) mod install;
pub(crate) mod lifecycle;
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
    /// Validate package.json manifests.
    Manifest(manifest::Args),
    Lock(lock::Args),
    /// Run a root package script.
    Run(run::Args),

    /// Install dependencies, optionally adding one package first.
    #[command(visible_alias = "i")]
    Install(install::Args),
    /// Add direct dependencies and resolve the resulting graph.
    Add(lifecycle::AddArgs),
    /// Remove direct dependencies and resolve the resulting graph.
    Remove(lifecycle::RemoveArgs),
    /// Re-resolve direct dependencies without changing declared ranges by default.
    Update(lifecycle::UpdateArgs),
    /// Print a deterministic, read-only dependency status report.
    Outdated(lifecycle::ReadOnlyArgs),
    /// Replay the lockfile to remove stale materialized output.
    Prune(lifecycle::ReadOnlyArgs),
    /// Upgrade Tapid to the latest stable release with checksum verification.
    Upgrade(upgrade::Args),
}

/// Routes a parsed command to its handler, or prints usage guidance when no command is given.
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

        Some(Command::Install(args)) => install::run(args),
        Some(Command::Add(args)) => lifecycle::add(args),
        Some(Command::Remove(args)) => lifecycle::remove(args),
        Some(Command::Update(args)) => lifecycle::update(args),
        Some(Command::Outdated(args)) => lifecycle::outdated(args),
        Some(Command::Prune(args)) => lifecycle::prune(args),
        Some(Command::Upgrade(args)) => upgrade::run(args),
    }
}
