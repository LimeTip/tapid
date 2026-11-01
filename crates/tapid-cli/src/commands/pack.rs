use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// One package directory; defaults to the invocation directory.
    #[arg(default_value = ".")]
    directory: PathBuf,
    /// Explicit new archive destination, relative to the invocation directory.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Capture and inspect complete archive bytes without writing output.
    #[arg(long)]
    dry_run: bool,
}
pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    match crate::application::pack::pack(&args.directory, args.output.as_deref(), args.dry_run) {
        Ok(result) => {
            if json {
                return crate::output::json::pack(&result);
            }
            println!(
                "{} {}@{}",
                if result.preview { "Preview" } else { "Created" },
                result.artifact.name(),
                result.artifact.version()
            );
            if let Some(destination) = result.destination {
                println!("Destination: {:?}", destination);
            }
            if result.artifact.gitignore_not_used() {
                println!(
                    ".gitignore is present and does not affect selection; no root .npmignore exists."
                );
            }
            for file in result.artifact.files() {
                println!(
                    "{:?}  {} bytes  {:04o}  {}",
                    file.path, file.size, file.mode, file.digest
                );
            }
            println!(
                "{} files, {} content bytes, {} archive bytes, SHA-256 {}",
                result.artifact.files().len(),
                result.artifact.content_bytes(),
                result.artifact.bytes().len(),
                result.artifact.digest()
            );
            println!(
                "Selection policy: 1; snapshot: not_atomic; lifecycle scripts: never executed"
            );
            ExitCode::SUCCESS
        }
        Err(error) => crate::output::json::failure_or_human(&error, "pack", json),
    }
}
