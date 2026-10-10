use clap::{Parser, Subcommand};
use std::process::ExitCode;

pub(crate) mod init;
pub(crate) mod install;
mod license;
pub(crate) mod lifecycle;
pub(crate) mod lock;
pub(crate) mod manifest;
mod release_verification;
pub(crate) mod run;
pub(crate) mod upgrade;

#[cfg(test)]
mod documentation;

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
    /// Print the Apache-2.0 license and copyright attribution.
    License,
    #[command(name = "__verify-release-record", hide = true)]
    VerifyReleaseRecord(release_verification::Args),
    #[command(name = "__prepare-release-install", hide = true)]
    PrepareReleaseInstall(release_verification::InstallArgs),
    /// Create a private package.json manifest.
    #[command(
        long_about = "Create a private package.json in an existing directory. Uses the directory name as the package name and sets version 0.1.0. Refuses to overwrite an existing package.json.",
        after_help = "Examples:
  tapid init
  tapid init ./my-project"
    )]
    Init(init::Args),
    /// Validate package.json manifests.
    Manifest(manifest::Args),
    /// Validate tapid.lock.
    Lock(lock::Args),
    /// Run a root package script.
    #[command(
        long_about = "Run a script from the root package.json using Node.js. Requires tapid.toml with an explicit [run.scripts.<name>] profile. Unsupported containment settings fail before the script starts. Pass script arguments after --.",
        after_help = "Examples:
  tapid run test
  tapid run dev -- --hostname 127.0.0.1 --port 3001"
    )]
    Run(run::Args),

    /// Install dependencies, optionally adding one package first.
    #[command(
        visible_alias = "i",
        long_about = "Install dependencies into node_modules and write tapid.lock. Optionally add one package to dependencies in package.json first.\n\nOnline installs resolve dependencies using registry metadata. Offline and frozen installs replay the existing lockfile from the verified store without network access. Dependency lifecycle scripts are denied by default. Exact tapid.lifecycle.toml approvals can build verified derived outputs on supported Linux ManagedTree hosts; offline/frozen replay never executes hooks.",
        after_help = "Examples:
  tapid install
  tapid install 'react@^19.0.0'
  tapid install --frozen
  tapid install --offline --store-dir ./verified-store"
    )]
    Install(install::Args),
    /// Add packages to package.json and install dependencies.
    #[command(
        long_about = "Add one or more packages to dependencies in package.json, then resolve and install the dependency graph and write tapid.lock.\n\nUse --dev, --optional, or --peer to select another dependency section. A package without a version requirement uses *.",
        after_help = "Examples:
  tapid add 'react@^19.0.0'
  tapid add --dev 'typescript@^5.0.0'
  tapid add --peer 'react@^19.0.0'"
    )]
    Add(lifecycle::AddArgs),
    /// Remove packages from package.json and reinstall dependencies.
    #[command(
        long_about = "Remove one or more declared packages from package.json, then resolve and install the remaining dependency graph and write tapid.lock. Pass package names as declared in the manifest, without version requirements.",
        after_help = "Examples:
  tapid remove react
  tapid remove eslint typescript"
    )]
    Remove(lifecycle::RemoveArgs),
    /// Resolve and reinstall dependencies using the declared version ranges.
    #[command(
        long_about = "Resolve and install the dependency graph again and write tapid.lock. Declared version ranges stay unchanged unless --latest is used.\n\nWith --latest, selected requirements become * in package.json. Package selection controls which declarations change; the entire graph is re-resolved on every update.",
        after_help = "Examples:
  tapid update
  tapid update react
  tapid update --latest react"
    )]
    Update(lifecycle::UpdateArgs),
    /// Compare declared and locked dependencies with registry versions.
    #[command(
        long_about = "Report each direct dependency's declared requirement, locked version, newest compatible version, and newest available version.\n\nRequires package.json and tapid.lock. Reads live registry metadata unless --registry-fixture is supplied. Does not update dependency declarations or install packages, but may recover an interrupted transaction.\n\n--store-dir and --allow-unverified-registry-artifacts have no effect on this command.",
        after_help = "Examples:
  tapid outdated
  tapid outdated --workspace web"
    )]
    Outdated(lifecycle::ReadOnlyArgs),
    /// Rebuild node_modules from tapid.lock to remove stale packages.
    #[command(
        long_about = "Replay tapid.lock from the verified store to replace managed node_modules and remove stale packages.\n\nRequires a matching package.json, a valid lockfile, and all referenced verified trees. Uses frozen replay without network access. Does not delete cached trees from the store.\n\n--registry-fixture and --allow-unverified-registry-artifacts have no effect on this command.",
        after_help = "Examples:
  tapid prune
  tapid prune --store-dir ./verified-store"
    )]
    Prune(lifecycle::ReadOnlyArgs),
    /// Download, verify, and install the latest stable Tapid release.
    #[command(
        long_about = "Download and verify the latest stable Tapid release for this platform, then replace the current executable or --destination.\n\nUses the signed release record from tapid.dev by default. If release discovery is unavailable, a cached release may be used for recovery; the command reports when it cannot check the latest version.",
        after_help = "Examples:
  tapid upgrade --dry-run
  tapid upgrade"
    )]
    Upgrade(upgrade::Args),
}

/// Routes a parsed command to its handler, or prints usage guidance when no command is given.
pub(crate) fn dispatch(command: Option<Command>) -> ExitCode {
    match command {
        Some(Command::License) => license::run(),
        Some(Command::VerifyReleaseRecord(args)) => release_verification::run(args),
        Some(Command::PrepareReleaseInstall(args)) => release_verification::prepare_install(args),
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
