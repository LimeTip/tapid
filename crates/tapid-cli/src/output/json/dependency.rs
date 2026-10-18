use super::{emit, envelope, recorded_text};
use crate::application::{install::InstallReport, outcome::Warning};
use serde_json::{Value, json};
use std::process::ExitCode;

pub(crate) fn installed(report: &InstallReport, operation: &str) -> ExitCode {
    let mut result = envelope(operation, "success", Some(&report.outcome));
    result["data"] = json!({"package_count": report.package_count, "replayed": report.replayed});
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
    emit(result, 0)
}
