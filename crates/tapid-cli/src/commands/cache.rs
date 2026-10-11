use clap::{Args as ClapArgs, Subcommand};
use std::{path::PathBuf, process::ExitCode};
use tapid_store::Store;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Option<CacheCommand>,
    /// Package store directory. Defaults to Tapid's platform cache location.
    #[arg(long, global = true)]
    store_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Show published package cache counts and logical byte sizes without network access.
    Info,
    /// Preview cache removal, or remove published package data with --yes.
    Clean(CleanOptions),
}

#[derive(Debug, ClapArgs)]
pub(crate) struct CleanArgs {
    /// Package store directory. Defaults to Tapid's platform cache location.
    #[arg(long)]
    store_dir: Option<PathBuf>,
    #[command(flatten)]
    options: CleanOptions,
}

#[derive(Debug, ClapArgs)]
struct CleanOptions {
    /// Remove the previewed scope without prompting. Offline replay will need cached trees again.
    #[arg(long, conflicts_with = "dry_run")]
    yes: bool,
    /// Show what cleaning would remove. This is also the default without --yes.
    #[arg(long)]
    dry_run: bool,
}

pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    match args.command {
        Some(CacheCommand::Clean(options)) => {
            execute(args.store_dir, true, options.yes, json, "cache")
        }
        _ => execute(args.store_dir, false, false, json, "cache"),
    }
}

pub(crate) fn clean(args: CleanArgs, json: bool) -> ExitCode {
    execute(args.store_dir, true, args.options.yes, json, "clean")
}

fn execute(
    root: Option<PathBuf>,
    clean: bool,
    remove: bool,
    json: bool,
    operation: &str,
) -> ExitCode {
    let root = match root.map_or_else(crate::application::install::default_store_root, Ok) {
        Ok(root) => root,
        Err(error) => {
            if json {
                return crate::output::json::operation_error(operation, "CACHE_PATH_INVALID");
            }
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let store = Store::new(&root);
    let result = if remove {
        store.clean_cache()
    } else {
        store.cache_info()
    };
    if json {
        return crate::output::json::cache_result(operation, &root, clean, remove, result);
    }
    match result {
        Ok(summary) => {
            println!("Package cache: {}", root.display());
            println!(
                "Artifacts: {} entries, {} bytes",
                summary.artifacts.entries, summary.artifacts.bytes
            );
            println!(
                "Trees: {} entries, {} bytes",
                summary.trees.entries, summary.trees.bytes
            );
            println!(
                "Preserved unrecognized entries: {}",
                summary.preserved_entries
            );
            println!(
                "Scope: published package data only; staging, lifecycle keys, recovery state, and project files are preserved."
            );
            if clean {
                if remove {
                    println!(
                        "Removed the reported cache entries. Offline installs may require repopulating the cache."
                    );
                } else {
                    println!(
                        "Preview only. Pass --yes to remove these entries. Offline installs may require repopulating the cache."
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            if matches!(error, tapid_store::IngestError::CacheCleanup(_)) {
                eprintln!(
                    "Some cache entries may already have been removed. Inspection and retry are safe."
                );
            }
            ExitCode::from(1)
        }
    }
}
