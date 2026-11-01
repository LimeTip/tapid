use super::{emit, envelope, recorded_text};
use crate::application::{install::InstallReport, outcome::Warning};
use serde_json::{Value, json};
use std::process::ExitCode;

pub(crate) fn installed(report: &InstallReport, operation: &str) -> ExitCode {
    let mut result = envelope(operation, "success", Some(&report.outcome));
    result["data"] = json!({"package_count": report.package_count, "replayed": report.replayed});
    if let Some(profile) = report.profile {
        result["data"]["profile"] =
            json!({"include_dev":profile.include_dev,"include_optional":profile.include_optional});
    }
    if !report.candidate_exclusions.is_empty() {
        result["data"]["candidate_exclusions"] =
            serde_json::to_value(&report.candidate_exclusions).expect("bounded exclusions");
        let mut fields = Vec::new();
        super::diagnostic::exclusion_truncation(
            &report.candidate_exclusions,
            "/data/candidate_exclusions",
            &mut fields,
        );
        result["truncated_fields"]
            .as_array_mut()
            .unwrap()
            .extend(fields.into_iter().map(Value::String));
    }
    if operation == "update" {
        let mut truncated = Vec::new();
        let changes = report
            .outcome
            .warnings
            .iter()
            .filter_map(|warning| match warning {
                Warning::UpdateGraphChange { package, reason } => Some((package, reason)),
                _ => None,
            })
            .enumerate()
            .map(|(index, (package, reason))| {
                let package = recorded_text(
                    package,
                    &format!("/data/graph_changes/{index}/package"),
                    &mut truncated,
                );
                let reason = recorded_text(
                    reason,
                    &format!("/data/graph_changes/{index}/reason"),
                    &mut truncated,
                );
                json!({"package": package, "reason": reason})
            })
            .collect::<Vec<_>>();
        result["data"]["graph_changes"] = json!(changes);
        let fields = result["truncated_fields"]
            .as_array_mut()
            .expect("envelope truncation list");
        fields.extend(truncated.into_iter().map(Value::String));
        fields.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    }
    let fields = result["truncated_fields"]
        .as_array_mut()
        .expect("truncation list");
    fields.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    fields.dedup();
    emit(result, 0)
}
