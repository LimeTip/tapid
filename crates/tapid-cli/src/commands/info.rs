use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// npm package with optional exact version, range, or tag. Bare names use latest.
    pub(crate) package: String,
    /// Directory supplying registry configuration only; no manifest is read.
    #[arg(long, default_value = ".")]
    pub(crate) project_dir: PathBuf,
    /// Read a bounded full metadata snapshot; never contact the network.
    #[arg(long)]
    pub(crate) registry_metadata: Option<PathBuf>,
    /// Disable network; requires --registry-metadata.
    #[arg(long)]
    pub(crate) offline: bool,
    /// Compare only the reported Node requirement against this concrete version.
    #[arg(long)]
    pub(crate) node_version: Option<tapid_core::PackageVersion>,
    /// Maximum entries per version/tag page (each request reads full metadata).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub(crate) page_size: u16,
    /// Continue supported version enumeration from a previous response.
    #[arg(long)]
    pub(crate) versions_after: Option<String>,
    /// Continue tag enumeration independently of versions.
    #[arg(long)]
    pub(crate) tags_after: Option<String>,
}

pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    let report = crate::application::info::inspect(
        &args.package,
        &args.project_dir,
        args.registry_metadata.as_deref(),
        args.offline,
        args.node_version,
        crate::application::info::PageRequest {
            size: usize::from(args.page_size),
            versions_after: args.versions_after.as_deref(),
            tags_after: args.tags_after.as_deref(),
        },
    );
    let result = crate::output::json::info(report.as_ref());
    if json {
        return crate::output::json::emit(
            result.0,
            if result.1 == ExitCode::SUCCESS { 0 } else { 1 },
        );
    }
    // Both presentations consume the same sanitized, bounded report.
    if result.0["data"]["package"].is_string() {
        let data = &result.0["data"];
        println!(
            "{} — {}",
            data["package"].as_str().unwrap_or_default(),
            data["source"]["origin"].as_str().unwrap_or_default()
        );
        println!(
            "Selection: {} ({})",
            data["selection"]["status"].as_str().unwrap_or_default(),
            data["selection"]["version"].as_str().unwrap_or("none")
        );
        println!(
            "Versions: {} returned, {} remaining, {} unsupported",
            data["versions"]["returned"],
            data["versions"]["known_omitted"],
            data["versions"]["unsupported"]
        );
        println!(
            "Tags: {} returned, {} remaining",
            data["tags"]["returned"], data["tags"]["known_omitted"]
        );
        println!("Registry declarations (unverified): {}", data["facts"]);
        println!("Tapid checks: {}", data["checks"]);
        println!("Use --json for structured pages and continuation tokens.");
    } else {
        println!("Inspection failed: {}", result.0["errors"]);
    }
    result.1
}
