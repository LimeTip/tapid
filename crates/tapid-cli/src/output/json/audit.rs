use super::{emit, envelope};
use crate::application::{audit::AuditReport, outcome::OperationOutcome};
use serde_json::json;
use std::{path::Path, process::ExitCode};

pub(crate) fn audit(project: &Path, report: Result<AuditReport, &'static str>) -> ExitCode {
    let changes = OperationOutcome::unchanged(project);
    let mut result = envelope(
        "audit",
        if report.is_ok() { "partial" } else { "failure" },
        Some(&changes),
    );
    result["data"] = json!({
        "assessment": "not_checked", "provider": "unavailable", "live_requests": 0,
        "coverage": { "complete": false, "advisory": "not_checked", "reported_malware": "not_checked", "artifact": "not_checked" },
        "findings": [], "checked_at": null, "freshness": "not_checked",
    });
    match report {
        Ok(report) => {
            result["data"]["lock_format"] = json!(report.format);
            result["data"]["recorded_subjects"] = json!(report.recorded_subjects);
            result["data"]["reachable_subjects"] = json!(report.reachable_subjects);
            result["data"]["graph_coverage"] = json!({
                "roots_recorded": report.roots_recorded,
                "unresolved_peers": report.unresolved_peers,
            });
        }
        Err(code) => result["errors"] = json!([{ "code": code }]),
    }
    emit(result, 3)
}
