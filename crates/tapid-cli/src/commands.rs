use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Create a private package.json manifest.
    Init { path: Option<PathBuf> },
    Manifest {
        #[command(subcommand)]
        command: ManifestCommand,
    },
    Lock {
        #[command(subcommand)]
        command: LockCommand,
    },
    /// Run a root package script.
    Run {
        /// Root package script name.
        script: String,
        /// Project directory containing package.json.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,
        /// Arguments forwarded after `--` to the script.
        #[arg(last = true)]
        arguments: Vec<String>,
    },
    /// Install dependencies, optionally adding one package first.
    #[command(alias = "i")]
    Install {
        /// Optional package name to add to dependencies before installation.
        package: Option<String>,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        frozen: bool,
        /// Permit npm metadata without registry-declared integrity. Not allowed with --offline or --frozen.
        #[arg(long)]
        allow_unverified_registry_artifacts: bool,
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,
        /// Store root containing verified trees.
        #[arg(long)]
        store_dir: Option<PathBuf>,
        /// Local JSON registry fixture used by tests and air-gapped development.
        #[arg(long)]
        registry_fixture: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ManifestCommand {
    Validate { path: Option<PathBuf> },
}

#[derive(Debug, Subcommand)]
pub(crate) enum LockCommand {
    Verify,
}
