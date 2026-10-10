//! Version 1 command results. Never render capability error text in this protocol.
use crate::application::{
    install::InstallReport,
    lifecycle::OutdatedReport,
    outcome::{ChangeState, OperationFailure, OperationOutcome, RetryAdvice, Warning},
};
use serde_json::{Value, json};
use std::{io::Write, process::ExitCode};

const MAX_ENTRIES: usize = 100;
const MAX_TEXT_BYTES: usize = 4096;

// Metadata is data, never a diagnostic or recovery instruction. Remove terminal
// controls and redact credential-bearing URLs before bounding each scalar.
fn text(value: impl std::fmt::Display) -> String {
    let value = crate::application::outcome::sanitize(&value.to_string());
    let mut result = String::new();
    for character in value.chars().filter(|c| !c.is_control()) {
        if result.len() + character.len_utf8() > MAX_TEXT_BYTES {
            break;
        }
        result.push(character);
    }
    result
}

fn envelope(operation: &str, outcome: &str, changes: Option<&OperationOutcome>) -> Value {
    let state = changes.map_or("unchanged", |changes| match changes.state {
        ChangeState::Unchanged => "unchanged",
        ChangeState::RolledBack => "rolled_back",
        ChangeState::Committed => "committed",
        ChangeState::CommittedCleanupPending => "committed_cleanup_pending",
        ChangeState::RecoveryRequired => "recovery_required",
    });
    let mut files = changes.map_or_else(Vec::new, |changes| {
        changes
            .changed_files
            .iter()
            .map(|path| text(path.display()))
            .collect::<Vec<_>>()
    });
    files.sort();
    files.dedup();
    let mut warnings = changes.map_or_else(Vec::new, |changes| {
        changes
            .warnings
            .iter()
            .map(|warning| match warning {
                Warning::UnverifiedRegistryArtifactsAllowed => {
                    "UNVERIFIED_REGISTRY_ARTIFACTS_ALLOWED"
                }
                Warning::PreviousTransactionRecovered => "PREVIOUS_TRANSACTION_RECOVERED",
            })
            .collect::<Vec<_>>()
    });
    warnings.sort_unstable();
    warnings.dedup();
    json!({
        "schema_version": 1,
        "operation": operation,
        "outcome": outcome,
        "project": changes.map(|changes| {
            let path = std::fs::canonicalize(&changes.project_dir).unwrap_or_else(|_| {
                if changes.project_dir.is_absolute() { changes.project_dir.clone() }
                else { std::env::current_dir().unwrap_or_default().join(&changes.project_dir) }
            });
            text(path.display())
        }),
        "changes": {"state": state, "files": files},
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

pub(crate) fn outdated(report: &OutdatedReport, operation: &str) -> ExitCode {
    emit(outdated_result(report, operation), 0)
}

fn outdated_result(report: &OutdatedReport, operation: &str) -> Value {
    let partial = report
        .entries
        .iter()
        .any(|entry| entry.diagnostic.is_some());
    let mut result = envelope(
        operation,
        if partial { "partial" } else { "success" },
        Some(&report.outcome),
    );
    let mut entries = report.entries.iter().collect::<Vec<_>>();
    entries.sort_by(|a, b| (&a.identity, &a.kind).cmp(&(&b.identity, &b.kind)));
    let entries = entries
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|entry| {
            json!({
                "identity": text(&entry.identity),
                "kind": text(&entry.kind),
                "declared": text(&entry.declared),
                "locked": entry.locked.as_ref().map(text),
                "newest_compatible": entry.newest_compatible.as_ref().map(text),
                "newest_available": entry.newest_available.as_ref().map(text),
                "error": entry.diagnostic.as_ref().map(|error| json!({"code": error.kind.code()})),
            })
        })
        .collect::<Vec<_>>();
    result["data"] = json!({"entries": entries, "total_entries": report.entries.len(), "truncated": report.entries.len() > MAX_ENTRIES});
    result
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
        let result = outdated_result(&report, "outdated");
        assert_eq!(result["outcome"], "partial");
        assert_eq!(result["data"]["truncated"], true);
        assert_eq!(result["data"]["total_entries"], 101);
        assert_eq!(result["data"]["entries"].as_array().unwrap().len(), 100);
        assert_eq!(result["data"]["entries"][0]["identity"], "package-000");
        assert!(!result.to_string().contains("secret"));
        assert!(!result.to_string().contains("hidden"));
        assert!(!result.to_string().contains("fragment"));
        assert!(text("界".repeat(4096)).len() <= MAX_TEXT_BYTES);
        assert!(!text("a\u{1b}[31m\n").contains('\u{1b}'));
    }
}
