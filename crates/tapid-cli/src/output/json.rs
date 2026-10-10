//! Version 1 command results. Never render capability error text in this protocol.
use crate::application::{
    install::InstallReport,
    lifecycle::OutdatedReport,
    outcome::{ChangeState, OperationFailure, OperationOutcome, RetryAdvice, Warning},
};
mod paths;

use serde_json::{Value, json};
use std::{io::Write, process::ExitCode};

const MAX_TEXT_BYTES: usize = 4096;

// Metadata is data, never a diagnostic or recovery instruction. Remove terminal
// controls and redact credential-bearing URLs before bounding each scalar.
fn text(value: impl std::fmt::Display) -> (String, bool) {
    let value = crate::application::outcome::sanitize(&value.to_string());
    let mut result = String::new();
    for character in value.chars().filter(|c| !c.is_control()) {
        if result.len() + character.len_utf8() > MAX_TEXT_BYTES {
            return (result, true);
        }
        result.push(character);
    }
    (result, false)
}

fn recorded_text(
    value: impl std::fmt::Display,
    pointer: &str,
    truncated_fields: &mut Vec<String>,
) -> String {
    let (value, truncated) = text(value);
    if truncated {
        truncated_fields.push(pointer.to_owned());
    }
    value
}

fn envelope(operation: &str, outcome: &str, changes: Option<&OperationOutcome>) -> Value {
    let state = changes.map_or("unchanged", |changes| match changes.state {
        ChangeState::Unchanged => "unchanged",
        ChangeState::RolledBack => "rolled_back",
        ChangeState::Committed => "committed",
        ChangeState::CommittedCleanupPending => "committed_cleanup_pending",
        ChangeState::RecoveryRequired => "recovery_required",
    });
    let mut truncated_fields = Vec::new();
    let project_path = changes.map(|changes| {
        std::fs::canonicalize(&changes.project_dir).unwrap_or_else(|_| {
            if changes.project_dir.is_absolute() {
                changes.project_dir.clone()
            } else {
                std::env::current_dir()
                    .unwrap_or_default()
                    .join(&changes.project_dir)
            }
        })
    });
    let project = project_path
        .as_ref()
        .map(|path| recorded_text(path.display(), "/project", &mut truncated_fields));
    let mut native_files = changes.map_or_else(Vec::new, |changes| changes.changed_files.clone());
    native_files.sort();
    native_files.dedup();
    let mut display_files = std::collections::BTreeMap::<String, bool>::new();
    for path in &native_files {
        let (value, truncated) = text(path.display());
        *display_files.entry(value).or_default() |= truncated;
    }
    let files = display_files
        .into_iter()
        .enumerate()
        .map(|(index, (value, truncated))| {
            if truncated {
                truncated_fields.push(format!("/changes/files/{index}"));
            }
            value
        })
        .collect::<Vec<_>>();
    let native_files = native_files
        .iter()
        .map(|path| paths::encode(path))
        .collect::<Vec<_>>();
    truncated_fields.sort_unstable();
    let mut warnings = changes.map_or_else(Vec::new, |changes| {
        changes
            .warnings
            .iter()
            .map(|warning| match warning {
                Warning::UnverifiedRegistryArtifactsAllowed => {
                    "UNVERIFIED_REGISTRY_ARTIFACTS_ALLOWED"
                }
                Warning::PreviousTransactionRecovered => "PREVIOUS_TRANSACTION_RECOVERED",
                Warning::DependencyLifecycleHookSkipped { .. } => {
                    "DEPENDENCY_LIFECYCLE_HOOK_SKIPPED"
                }
                Warning::DependencyLifecycleDiscoveryFailed { .. } => {
                    "DEPENDENCY_LIFECYCLE_DISCOVERY_FAILED"
                }
            })
            .collect::<Vec<_>>()
    });
    warnings.sort_unstable();
    warnings.dedup();
    json!({
        "schema_version": 1,
        "operation": operation,
        "outcome": outcome,
        "project": project,
        "project_path": project_path.as_ref().map(|path| paths::encode(path)),
        "changes": {"state": state, "files": files, "paths": native_files},
        "truncated_fields": truncated_fields,
        "warnings": warnings,
        "errors": [],
        "retry": null,
        "data": null,
    })
}

fn emit(result: Value, status: u8) -> ExitCode {
    // One newline-terminated object. A broken output channel is an I/O failure.
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &result).is_err() || stdout.write_all(b"\n").is_err() {
        return ExitCode::from(1);
    }
    ExitCode::from(status)
}

/// Informational data comes from the CLI definition, never from project metadata.
pub(crate) fn information(operation: &str, data: Value) -> ExitCode {
    let mut result = envelope(operation, "success", None);
    result["data"] = data;
    emit(result, 0)
}

pub(crate) fn protocol_error(operation: &str, code: &str, status: u8) -> ExitCode {
    let mut result = envelope(operation, "failure", None);
    result["errors"] = json!([{"code": code}]);
    emit(result, status)
}

pub(crate) fn failure_or_human(
    failure: &OperationFailure,
    operation: &str,
    json_output: bool,
) -> ExitCode {
    if !json_output {
        super::report_failure(failure);
        return ExitCode::from(1);
    }
    emit(failure_result(failure, operation), 1)
}

fn failure_result(failure: &OperationFailure, operation: &str) -> Value {
    let mut result = envelope(operation, "failure", Some(&failure.outcome));
    let mut errors = vec![json!({"code": failure.error.kind.code(), "phase": "operation"})];
    if let Some(error) = &failure.recovery_error {
        errors.push(json!({"code": error.kind.code(), "phase": "recovery"}));
    }
    result["errors"] = json!(errors);
    result["retry"] = json!(match failure.retry {
        RetryAdvice::AfterCorrection => "after_correction",
        RetryAdvice::AfterContention => "after_contention",
        RetryAdvice::DoNotRepeat => "do_not_repeat",
        RetryAdvice::RecoverFirst => "recover_first",
    });
    result
}

pub(crate) fn installed(report: &InstallReport, operation: &str) -> ExitCode {
    let mut result = envelope(operation, "success", Some(&report.outcome));
    result["data"] = json!({"package_count": report.package_count, "replayed": report.replayed});
    emit(result, 0)
}

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
                "error": entry.diagnostic.as_ref().map(|error| json!({"code": error.kind.code()})),
            })
        })
        .collect::<Vec<_>>();
    truncated_fields.sort_unstable();
    result["truncated_fields"] = json!(truncated_fields);
    result["data"] = json!({"entries": entries, "total_entries": report.entries.len(), "truncated": limit != 0 && report.entries.len() > limit});
    result
}

pub(crate) fn cache_result(
    operation: &str,
    root: &std::path::Path,
    clean: bool,
    remove: bool,
    report: Result<tapid_store::CacheSummary, tapid_store::IngestError>,
) -> ExitCode {
    let mut result = envelope(
        operation,
        if report.is_ok() { "success" } else { "failure" },
        None,
    );
    let mut truncated = Vec::new();
    result["data"] = json!({
        "scope": "published_package_data",
        "store": recorded_text(root.display(), "/data/store", &mut truncated),
        "store_path": paths::encode(root),
        "action": if remove { "clean" } else if clean { "preview" } else { "info" },
        "summary": null,
    });
    result["truncated_fields"] = json!(truncated);
    match report {
        Ok(summary) => {
            result["data"]["summary"] = json!({
                "artifacts": {"entries": summary.artifacts.entries, "bytes": summary.artifacts.bytes},
                "trees": {"entries": summary.trees.entries, "bytes": summary.trees.bytes},
                "preserved_entries": summary.preserved_entries,
            });
            if remove && (summary.artifacts.entries != 0 || summary.trees.entries != 0) {
                result["changes"]["state"] = json!("committed");
            }
            emit(result, 0)
        }
        Err(error) => {
            let busy = matches!(&error, tapid_store::IngestError::Io(e) if e.kind() == std::io::ErrorKind::WouldBlock);
            result["errors"] =
                json!([{"code": if busy { "CACHE_BUSY" } else { "CACHE_MAINTENANCE_FAILED" }}]);
            if busy {
                result["retry"] = json!("after_contention");
            } else if matches!(error, tapid_store::IngestError::CacheCleanup(_)) {
                // A deletion failure can leave a partially cleared cache. No
                // project state or recovery transaction is involved.
                result["changes"]["state"] = json!("committed_cleanup_pending");
            }
            emit(result, 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::outcome::{ErrorKind, OperationalError};
    use std::path::Path;

    #[test]
    fn recovery_instructions_come_only_from_typed_state() {
        for (state, retry) in [
            (ChangeState::Unchanged, "after_correction"),
            (ChangeState::RolledBack, "after_correction"),
            (ChangeState::Committed, "do_not_repeat"),
            (ChangeState::CommittedCleanupPending, "do_not_repeat"),
            (ChangeState::RecoveryRequired, "recover_first"),
        ] {
            let mut outcome = OperationOutcome::unchanged(Path::new("project"));
            outcome.state = state;
            let failure = OperationFailure::new(
                OperationalError::new(ErrorKind::Transaction, "secret; repeat the operation"),
                outcome,
                Some(OperationalError::new(
                    ErrorKind::Recovery,
                    "untrusted instruction",
                )),
            );
            let result = failure_result(&failure, "add");
            assert_eq!(result["retry"], retry);
            assert_eq!(result["errors"][1]["phase"], "recovery");
            assert!(!result.to_string().contains("secret"));
            assert!(!result.to_string().contains("untrusted instruction"));
            if state == ChangeState::RecoveryRequired {
                assert_eq!(result["changes"]["files"].as_array().unwrap().len(), 3);
            }
        }
        let failure = OperationFailure::unchanged(
            Path::new("project"),
            OperationalError::new(ErrorKind::ProjectBusy, "busy"),
        );
        assert_eq!(
            failure_result(&failure, "install")["retry"],
            "after_contention"
        );
    }

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
