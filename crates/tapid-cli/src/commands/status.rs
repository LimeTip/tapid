use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Workspace root containing package.json and tapid.lock.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub(crate) project_dir: PathBuf,
    /// Select a workspace member by exact package name.
    #[arg(long, value_name = "NAME")]
    pub(crate) workspace: Option<String>,
}

/// Render the offline report as sanitized human text or a single JSON envelope.
/// Return failure for blockers or truncation without installing or attempting recovery.
pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    let report = crate::application::status::inspect(&args.project_dir, args.workspace.as_deref());
    if json {
        return crate::output::json::status_result(&report);
    }
    crate::output::report_compatibility(Some(&report.compatibility));
    println!(
        "Project: {}",
        crate::output::json::text(report.project.display()).0
    );
    println!("Local assessment: {}", report.assessment);
    println!(
        "Workspace selection: {}",
        crate::output::json::text(report.workspace["selected"].as_str().unwrap_or("root")).0
    );
    println!(
        "Workspace manifest: {}",
        crate::output::json::text(
            report.workspace["manifest"]
                .as_str()
                .unwrap_or("unavailable")
        )
        .0
    );
    println!("Workspace linking: not verified");
    println!(
        "Manifest/lock agreement: {}",
        report.manifest_lock_agreement
    );
    for installation in &report.installation {
        println!(
            "Installation {}: {}",
            crate::output::json::text(installation["path"].as_str().unwrap_or("?")).0,
            installation["state"].as_str().unwrap_or("unknown")
        );
    }
    println!("Project recovery: {}", report.recovery);
    if let Some(guidance) = report.recovery_guidance {
        println!("{guidance}");
    }
    println!("Registry configuration: {}", report.registries);
    for blocker in &report.blockers {
        println!(
            "Blocker {}: {}",
            blocker.code,
            crate::output::json::text(&blocker.location).0
        );
    }
    if report.truncated {
        println!("Inspection truncated at safety bounds.");
    }
    println!("Verification depth: {}", report.verification_depth);
    println!("Snapshot: not atomic; concurrent writes can escape detection");
    for file in report.snapshot["files"].as_array().into_iter().flatten() {
        println!(
            "  {}: {}",
            crate::output::json::text(file["path"].as_str().unwrap_or("?")).0,
            file["identity"].as_str().unwrap_or("unknown")
        );
    }
    println!("Experimental capabilities: {}", report.capabilities);
    println!("Checks not performed:");
    for check in &report.checks_not_performed {
        println!("  {check}");
    }
    if report.assessment == "blocked" {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
