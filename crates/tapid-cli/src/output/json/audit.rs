use super::{emit, envelope};
use crate::application::{audit::AuditReport, outcome::OperationOutcome};
use serde_json::json;
use std::{path::Path, process::ExitCode};

pub(crate) fn audit(project: &Path, report: Result<AuditReport, &'static str>) -> ExitCode {
    let (result, status) = audit_value(project, &report);
    emit(result, status)
}

pub(crate) fn audit_value(
    project: &Path,
    report: &Result<AuditReport, &'static str>,
) -> (serde_json::Value, u8) {
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
    let mut status = 3;
    match report {
        Ok(report) => {
            status = report.outcome.exit_code();
            result["outcome"] = json!(if status == 3 { "partial" } else { "success" });
            if !report.failures.is_empty() {
                result["data"]["live_requests"] = serde_json::Value::Null;
            }
            if !report.evidence.is_empty() {
                result["data"]["assessment"] = json!(match report.outcome {
                    tapid_policy::AuditOutcome::BelowThreshold
                        if report.evidence.iter().all(|e| e.result.findings.is_empty()) =>
                        "no_known_findings",
                    tapid_policy::AuditOutcome::BelowThreshold => "below_threshold",
                    tapid_policy::AuditOutcome::Findings => "findings",
                    tapid_policy::AuditOutcome::Incomplete => "incomplete",
                });
                result["data"]["provider"] = json!("verified_evidence");
                result["data"]["live_requests"] = serde_json::Value::Null;
                result["data"]["coverage"]["complete"] = json!(status != 3);
                result["data"]["evidence"] = json!(
                    report
                        .evidence
                        .iter()
                        .take(100)
                        .map(|e| json!({"key":e.key,"result":e.result,"paths":e.paths,"paths_truncated":e.paths_truncated}))
                        .collect::<Vec<_>>()
                );
                // Findings retain the exact signed request, issuer, revision,
                // check time, expiry and status rather than flattening contexts.
                result["data"]["findings"] = json!(report.evidence.iter().flat_map(|e| e.result.findings.iter().map(move |f| json!({"key":e.key,"subject":e.result.request.subject,"report":f}))).take(100).collect::<Vec<_>>());
                result["data"]["coverage"]["advisory"] =
                    json!(if status == 3 { "incomplete" } else { "checked" });
                result["data"]["coverage"]["reported_malware"] =
                    result["data"]["coverage"]["advisory"].clone();
                result["data"]["freshness"] = json!("per_evidence");
            }
            result["data"]["failures"] =
                json!(report.failures.iter().take(100).collect::<Vec<_>>());
            result["data"]["not_checked"] = json!(report.not_checked);
            let mut truncated = Vec::new();
            if report.failures.len() > 100 {
                truncated.push("/data/failures".to_owned());
            }
            if report.evidence.iter().any(|e| e.paths_truncated) {
                truncated.push("/data/evidence/paths".to_owned());
            }
            if report.evidence.len() > 100 {
                truncated.push("/data/evidence".to_owned());
            }
            if report
                .evidence
                .iter()
                .map(|e| e.result.findings.len())
                .sum::<usize>()
                > 100
            {
                truncated.push("/data/findings".to_owned());
            }
            super::status_text(&mut result["data"], "/data", &mut truncated);
            if !truncated.is_empty() {
                status = 3;
                result["outcome"] = json!("partial");
                result["data"]["coverage"]["complete"] = json!(false);
                truncated.sort_unstable();
                truncated.dedup();
                result["truncated_fields"] = json!(truncated);
            }
            result["data"]["lock_format"] = json!(report.format);
            result["data"]["recorded_subjects"] = json!(report.recorded_subjects);
            result["data"]["reachable_subjects"] = json!(report.reachable_subjects);
            result["data"]["graph_coverage"] = json!({
                "roots_recorded": report.roots_recorded,
                "unresolved_peers": report.unresolved_peers,
                "requestable_occurrences": report.requestable_occurrences,
                "excluded_occurrences": report.excluded_occurrences,
            });
        }
        Err(code) => result["errors"] = json!([{ "code": code }]),
    }
    (result, status)
}
