pub(crate) mod install;
pub(crate) mod json;
pub(crate) mod preview;

use std::process::ExitCode;

/// Convert a child status to the process status used by the CLI.
///
/// Windows needs to preserve the complete status. Unix exposes the low byte
/// through its normal process-status interface.
#[cfg(windows)]
pub(crate) fn child_exit_code(code: i32) -> ExitCode {
    std::process::exit(code);
}

#[cfg(not(windows))]
pub(crate) fn child_exit_code(code: i32) -> ExitCode {
    ExitCode::from(code as u8)
}

pub(crate) fn report_warnings(warnings: &[crate::application::outcome::Warning]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

pub(crate) fn report_failure(failure: &crate::application::outcome::OperationFailure) {
    use crate::application::outcome::{ChangeState, RetryAdvice};
    report_warnings(&failure.outcome.warnings);
    report_compatibility(failure.outcome.compatibility.as_ref());
    eprintln!("error: {failure}");
    eprintln!("diagnostic: {}", failure.error.kind.code());
    match failure.outcome.state {
        ChangeState::Committed => {
            eprintln!(
                "dependency changes committed in {}; do not repeat the operation",
                failure.outcome.project_dir.display()
            );
        }
        ChangeState::CommittedCleanupPending => {
            eprintln!(
                "dependency changes committed in {}; cleanup remains pending; do not repeat the operation",
                failure.outcome.project_dir.display()
            );
        }
        ChangeState::RecoveryRequired => {
            eprintln!(
                "recovery required in {}; inspect {} affected project output(s) before retrying",
                failure.outcome.project_dir.display(),
                failure.outcome.changed_files.len()
            );
        }
        ChangeState::Unchanged | ChangeState::RolledBack => {}
    }
    if failure.retry == RetryAdvice::AfterContention {
        eprintln!("retry after the competing operation finishes");
    }
}

pub(crate) fn report_progress(event: crate::application::install::Progress, json: bool) {
    if json {
        return;
    }
    use crate::application::install::Progress;
    match event {
        Progress::Metadata(count) => {
            eprintln!("Registry metadata progress: {count} package(s) fetched")
        }
        Progress::Artifact(completed, total) => {
            eprintln!("Artifact verification progress: {completed}/{total}")
        }
        Progress::Materialization(completed, total) => {
            eprintln!("Materialization progress: {completed}/{total}");
        }
        Progress::Replay(completed, total) => {
            eprintln!("Replay snapshot progress: {completed}/{total}")
        }
    }
}

/// Human output shares the complete evaluator with JSON; display bounds never affect policy.
pub(crate) fn report_compatibility(report: Option<&crate::compatibility::Report>) {
    let Some(report) = report else { return };
    let runtime = &report.runtime;
    if runtime.state != "not_requested" {
        let executable = runtime
            .executable
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "unavailable".into());
        let version = runtime
            .version
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "unavailable".into());
        eprintln!(
            "Node observation: {} ({}, executable: {}, version: {}).",
            runtime.state,
            runtime.reason,
            json::text(executable).0,
            version
        );
    }
    for fact in report
        .facts
        .iter()
        .filter(|f| f.status != "not_declared")
        .take(100)
    {
        let label = if fact.status == "satisfied" {
            "Compatibility"
        } else {
            "Compatibility warning"
        };
        eprintln!(
            "{label}: {} {}: {} ({}, requirement: {}).",
            json::text(&fact.identity).0,
            json::text(&fact.location).0,
            fact.status,
            fact.reason,
            json::text(fact.requirement.as_deref().unwrap_or("unspecified")).0
        );
    }
    let omitted = report
        .facts
        .iter()
        .filter(|f| f.status != "not_declared")
        .count()
        .saturating_sub(100);
    if omitted > 0 {
        eprintln!("Compatibility details: {omitted} additional fact(s) omitted.");
    }
}
