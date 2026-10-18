use super::{diagnostic, emit, envelope, recorded_text};
use crate::application::lifecycle::OutdatedReport;
use serde_json::{Value, json};
use std::process::ExitCode;

pub(crate) fn outdated(report: &OutdatedReport, operation: &str, limit: usize) -> ExitCode {
    emit(outdated_result(report, operation, limit), 0)
}

fn outdated_result(report: &OutdatedReport, operation: &str, limit: usize) -> Value {
    let partial = report
        .entries
        .iter()
        .any(|entry| entry.diagnostic.is_some());
    let mut result = envelope(
        operation,
        if partial { "partial" } else { "success" },
        Some(&report.outcome),
    );
    let mut truncated_fields = result["truncated_fields"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut entries = report.entries.iter().collect::<Vec<_>>();
    entries.sort_by(|a, b| (&a.identity, &a.kind).cmp(&(&b.identity, &b.kind)));
    let entries = entries
        .into_iter()
        .take(if limit == 0 { usize::MAX } else { limit })
        .enumerate()
        .map(|(index, entry)| {
            let mut field = |value: &str, key: &str| recorded_text(value, &format!("/data/entries/{index}/{key}"), &mut truncated_fields);
            json!({
                "identity": field(&entry.identity, "identity"),
                "kind": field(&entry.kind, "kind"),
                "declared": field(&entry.declared, "declared"),
                "locked": entry.locked.as_ref().map(|value| field(value, "locked")),
                "newest_compatible": entry.newest_compatible.as_ref().map(|value| field(value, "newest_compatible")),
                "newest_available": entry.newest_available.as_ref().map(|value| field(value, "newest_available")),
                "compatible_impact": entry.impact(entry.newest_compatible.as_deref()),
                "available_impact": entry.impact(entry.newest_available.as_deref()),
                "error": entry.diagnostic.as_ref().map(|error| diagnostic::error(error, None, &format!("/data/entries/{index}/error"), &mut truncated_fields)),
            })
        })
        .collect::<Vec<_>>();
    truncated_fields.sort_unstable();
    result["truncated_fields"] = json!(truncated_fields);
    result["data"] = json!({"entries": entries, "total_entries": report.entries.len(), "truncated": limit != 0 && report.entries.len() > limit});
    result
}

#[cfg(test)]
mod tests {
    use super::super::{MAX_TEXT_BYTES, text};
    use super::*;
    use crate::application::outcome::{ErrorKind, OperationOutcome, OperationalError};
    use std::path::Path;

    #[test]
    fn outdated_is_sorted_bounded_and_partial_even_when_error_is_beyond_limit() {
        let entries = (0..101)
            .rev()
            .map(|index| crate::application::lifecycle::OutdatedEntry {
                identity: format!("package-{index:03}"),
                kind: "dependencies".into(),
                declared: format!(
                    "https://user:secret@example.com/path?token=hidden#fragment{}",
                    "界".repeat(4096)
                ),
                locked: None,
                newest_compatible: None,
                newest_available: None,
                diagnostic: (index == 100)
                    .then(|| OperationalError::new(ErrorKind::RegistryMetadata, "secret")),
            })
            .collect();
        let report = OutdatedReport {
            entries,
            outcome: OperationOutcome::unchanged(Path::new("project")),
        };
        let result = outdated_result(&report, "outdated", 100);
        assert_eq!(result["outcome"], "partial");
        assert_eq!(result["data"]["truncated"], true);
        assert_eq!(result["data"]["total_entries"], 101);
        assert_eq!(result["data"]["entries"].as_array().unwrap().len(), 100);
        assert_eq!(result["data"]["entries"][0]["identity"], "package-000");
        assert!(!result.to_string().contains("secret"));
        assert!(!result.to_string().contains("hidden"));
        assert!(!result.to_string().contains("fragment"));
        assert!(text("界".repeat(4096)).0.len() <= MAX_TEXT_BYTES);
        assert!(!text("x".repeat(MAX_TEXT_BYTES)).1);
        assert!(!text("a\u{1b}[31m\n").0.contains('\u{1b}'));
    }

    #[test]
    fn shortened_scalars_are_marked_and_recovery_paths_remain_complete() {
        let project = tapid_test_support::TempProject::new("json-path-display").unwrap();
        let long_path = project.path().join("界".repeat(2000));
        let mut outcome = OperationOutcome::unchanged(&long_path);
        for name in ["node_modules", "package.json", "tapid.lock"] {
            outcome.changed_files.push(long_path.join(name));
        }
        let result = envelope("install", "failure", Some(&outcome));
        assert_eq!(result["project_path"]["encoding"], "utf8");
        assert_eq!(result["project_path"]["value"], long_path.to_str().unwrap());
        assert_eq!(
            result["changes"]["paths"][0]["value"],
            long_path.join("node_modules").to_str().unwrap()
        );
        assert_eq!(result["changes"]["paths"].as_array().unwrap().len(), 3);
        let shortened = result["truncated_fields"].as_array().unwrap();
        assert!(shortened.contains(&json!("/project")));
        assert!(shortened.contains(&json!("/changes/files/0")));
        assert!(result["project"].as_str().unwrap().len() <= MAX_TEXT_BYTES);

        let report = OutdatedReport {
            entries: vec![crate::application::lifecycle::OutdatedEntry {
                identity: "x".repeat(5000),
                kind: "dependencies".into(),
                declared: "*".into(),
                locked: None,
                newest_compatible: None,
                newest_available: None,
                diagnostic: None,
            }],
            outcome,
        };
        let result = outdated_result(&report, "outdated", 100);
        let shortened = result["truncated_fields"].as_array().unwrap();
        assert!(shortened.contains(&json!("/project")));
        assert!(shortened.contains(&json!("/data/entries/0/identity")));
    }
}
