use clap::{Parser, Subcommand};
use std::process::ExitCode;

mod ci;
pub(crate) mod init;
pub(crate) mod install;
mod license;
pub(crate) mod lifecycle;
pub(crate) mod lock;
pub(crate) mod manifest;
mod npm_import;
mod release_verification;
pub(crate) mod run;
pub(crate) mod upgrade;
pub(crate) mod why;

#[cfg(test)]
mod documentation;

#[derive(Debug, Parser)]
#[command(
    name = "tapid",
    version,
    about = "A deterministic JavaScript and TypeScript package manager"
)]
pub(crate) struct Cli {
    /// Emit a versioned JSON result for supported package commands.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

impl Cli {
    // Check after Clap propagates global arguments. A subcommand's `requires`
    // check cannot see --json when it appears before the subcommand.
    pub(crate) fn validate_json_options(&self) -> Result<(), clap::Error> {
        if !self.json
            && matches!(&self.command, Some(Command::Outdated(args)) if args.json_limit.is_some())
        {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::MissingRequiredArgument,
                "--json-limit requires --json",
            ));
        }
        Ok(())
    }
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
    /// Import an npm v3 package-lock.json offline without changing selected versions.
    #[command(
        long_about = "Import an npm lockfileVersion 3 lock into tapid.lock without network access or dependency resolution. Validates package.json in the current directory. Linked/workspace entries and unsupported metadata fail before writing. The first frozen install verifies the pinned tarballs; dependency scripts do not run."
    )]
    ImportPackageLock(npm_import::Args),
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
        long_about = "Install dependencies into node_modules and write tapid.lock. Optionally add one package to dependencies in package.json first.\n\nOnline installs resolve dependencies using registry metadata. Offline installs replay verified trees without network access. Frozen installs preserve lockfile selections; imported npm locks can fetch pinned tarballs for their first verification. Dependency lifecycle scripts do not run.",
        after_help = "Examples:
  tapid install
  tapid install 'react@^19.0.0'
  tapid install --frozen
  tapid install --offline --store-dir ./verified-store"
    )]
    Install(install::Args),
    /// Install exact locked dependencies without changing package.json or tapid.lock.
    #[command(
        long_about = "Install the exact dependency graph in tapid.lock. Requires matching project and workspace manifests and download URLs for every registry package, including with --offline. Explicit local registry fixtures can supply artifacts without URLs. Missing verified trees are downloaded and verified without version resolution. Atomically replaces managed node_modules. Dependency lifecycle scripts do not run."
    )]
    Ci(ci::Args),
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
    Outdated(lifecycle::OutdatedArgs),
    /// Rebuild node_modules from tapid.lock to remove stale packages.
    #[command(
        long_about = "Replay tapid.lock from the verified store to replace managed node_modules and remove stale packages.\n\nRequires a matching package.json, a valid lockfile, and all referenced verified trees. Uses frozen replay without network access. Does not delete cached trees from the store.\n\n--registry-fixture and --allow-unverified-registry-artifacts have no effect on this command.",
        after_help = "Examples:
  tapid prune
  tapid prune --store-dir ./verified-store"
    )]
    Prune(lifecycle::ReadOnlyArgs),
    /// Explain which dependency paths bring a package into the project.
    #[command(
        long_about = "Trace a package through the resolved graph in package.json and tapid.lock. Reads local project files only and does not contact registries or change the manifest, lockfile, store, or node_modules. Direct dependency kinds are read from the selected manifest; the lockfile currently does not preserve kinds for transitive edges.",
        after_help = "Examples:\n  tapid why react\n  tapid why @scope/package --project-dir ./app\n  tapid --json why lodash --workspace web"
    )]
    Why(why::Args),
    /// Download, verify, and install the latest stable Tapid release.
    #[command(
        long_about = "Download and verify the latest stable Tapid release for this platform, then replace the current executable or --destination.\n\nUses the signed release record from tapid.dev by default. If release discovery is unavailable, a cached release may be used for recovery; the command reports when it cannot check the latest version.",
        after_help = "Examples:
  tapid upgrade --dry-run
  tapid upgrade"
    )]
    Upgrade(upgrade::Args),
}

impl Command {
    fn operation(&self) -> &'static str {
        match self {
            Self::License => "license",
            Self::VerifyReleaseRecord(_) => "__verify-release-record",
            Self::PrepareReleaseInstall(_) => "__prepare-release-install",
            Self::Init(_) => "init",
            Self::ImportPackageLock(_) => "import-package-lock",
            Self::Manifest(_) => "manifest",
            Self::Lock(_) => "lock",
            Self::Run(_) => "run",
            Self::Install(_) => "install",
            Self::Ci(_) => "ci",
            Self::Add(_) => "add",
            Self::Remove(_) => "remove",
            Self::Update(_) => "update",
            Self::Outdated(_) => "outdated",
            Self::Prune(_) => "prune",
            Self::Why(_) => "why",
            Self::Upgrade(_) => "upgrade",
        }
    }
}

/// Routes a parsed command to its handler, or prints usage guidance when no command is given.
pub(crate) fn dispatch(command: Option<Command>, json: bool) -> ExitCode {
    if json
        && !matches!(
            command,
            Some(
                Command::Install(_)
                    | Command::Add(_)
                    | Command::Remove(_)
                    | Command::Update(_)
                    | Command::Outdated(_)
                    | Command::Prune(_)
                    | Command::Why(_)
            )
        )
    {
        return crate::output::json::protocol_error(
            command.as_ref().map_or("none", Command::operation),
            "JSON_UNSUPPORTED_COMMAND",
            1,
        );
    }
    match command {
        Some(Command::License) => license::run(),
        Some(Command::VerifyReleaseRecord(args)) => release_verification::run(args),
        Some(Command::PrepareReleaseInstall(args)) => release_verification::prepare_install(args),
        None => {
            println!("Run 'tapid --help' for usage");
            ExitCode::SUCCESS
        }
        Some(Command::Init(args)) => init::run(args),
        Some(Command::ImportPackageLock(args)) => npm_import::run(args),
        Some(Command::Manifest(args)) => manifest::run(args),
        Some(Command::Lock(args)) => lock::run(args),
        Some(Command::Run(args)) => run::run(args),

        Some(Command::Install(args)) => install::run(args, json),
        Some(Command::Ci(args)) => ci::run(args),
        Some(Command::Add(args)) => lifecycle::add(args, json),
        Some(Command::Remove(args)) => lifecycle::remove(args, json),
        Some(Command::Update(args)) => lifecycle::update(args, json),
        Some(Command::Outdated(args)) => lifecycle::outdated(args, json),
        Some(Command::Prune(args)) => lifecycle::prune(args, json),
        Some(Command::Why(args)) => why::run(args, json),
        Some(Command::Upgrade(args)) => upgrade::run(args),
    }
}
