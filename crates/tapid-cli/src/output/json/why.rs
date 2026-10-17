use super::{diagnostic, emit, envelope};
use crate::application::outcome::OperationOutcome;
use serde_json::{Value, json};
use std::process::ExitCode;

pub(crate) fn why_success(
    report: &crate::application::why::WhyReport,
    project: &std::path::Path,
) -> ExitCode {
    let (result, status) = why_result(report, project);
    emit(result, status)
}

fn why_result(
    report: &crate::application::why::WhyReport,
    project: &std::path::Path,
) -> (Value, u8) {
    let outcome = OperationOutcome::unchanged(project);
    let not_found = report.paths.is_empty() && !report.truncated;
    let mut result = envelope(
        "why",
        if report.truncated {
            "partial"
        } else {
            "success"
        },
        Some(&outcome),
    );
    result["data"] = json!({
        "package": &report.package,
        "paths": &report.paths,
        "truncated": report.truncated,
        "not_found": not_found,
        "graph_warnings": &report.warnings,
    });
    (result, u8::from(not_found))
}

pub(crate) fn why_failure(
    project: &std::path::Path,
    error: &crate::application::outcome::OperationalError,
) -> ExitCode {
    let outcome = OperationOutcome::unchanged(project);
    let mut result = envelope("why", "failure", Some(&outcome));
    diagnostic::record(&mut result, error, "operation");
    emit(result, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn truncated_empty_why_result_is_partial_and_successful() {
        let report = crate::application::why::WhyReport {
            schema_version: 1,
            operation: "why",
            outcome: "not_found",
            effective_project: "project".into(),
            changes: Vec::new(),
            warnings: Vec::new(),
            package: "missing".into(),
            paths: Vec::new(),
            truncated: true,
        };
        let (result, status) = why_result(&report, Path::new("project"));
        assert_eq!(result["outcome"], "partial");
        assert_eq!(result["data"]["not_found"], false);
        assert_eq!(status, 0);
    }
}
