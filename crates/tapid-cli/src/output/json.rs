//! Version 1 command results. Never render capability error text in this protocol.
use crate::application::outcome::{
    ChangeState, OperationFailure, OperationOutcome, RetryAdvice, Warning,
};
mod cache;
mod dependency;
mod explain;
mod outdated;
mod preview;
mod tools;
mod why;

pub(crate) use cache::cache_result;
pub(crate) use dependency::installed;
pub(crate) use explain::explain;
pub(crate) use outdated::outdated;
pub(crate) use preview::preview;
pub(crate) use tools::tools;
pub(crate) use why::{why_failure, why_success};

mod diagnostic;
mod paths;

use serde_json::{Value, json};
use std::{io::Write, process::ExitCode};

const MAX_TEXT_BYTES: usize = 4096;

// Metadata is data, never a diagnostic or recovery instruction. Remove terminal
// controls and redact credential-bearing URLs before bounding each scalar.
pub(crate) fn text(value: impl std::fmt::Display) -> (String, bool) {
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
            .map(Warning::code)
            .collect::<Vec<_>>()
    });
    let mut warning_details = changes.map_or_else(Vec::new, |changes| {
        changes
            .warnings
            .iter()
            .take(1024)
            .map(|warning| match warning {
                Warning::DependencyLifecycleHookSkipped { package, hook } => json!({
                    "code": "DEPENDENCY_LIFECYCLE_HOOK_SKIPPED",
                    "package": lifecycle_identity(package), "hook": hook,
                }),
                Warning::DependencyLifecycleDiscoveryFailed { package } => json!({
                    "code": "DEPENDENCY_LIFECYCLE_DISCOVERY_FAILED",
                    "package": lifecycle_identity(package),
                }),
                Warning::UpdateGraphChange { package, reason } => {
                    json!({"code": warning.code(), "package": package, "reason": reason})
                }
                _ => json!({"code": warning.code()}),
            })
            .collect::<Vec<_>>()
    });
    let warning_truncation_start = truncated_fields.len();
    for (index, value) in warning_details.iter_mut().enumerate() {
        status_text(
            value,
            &format!("/warning_details/{index}"),
            &mut truncated_fields,
        );
    }
    let warning_details_complete = truncated_fields.len() == warning_truncation_start
        && changes.is_none_or(|c| c.omitted_warnings == 0 && c.warnings.len() <= 1024);
    if let Some(changes) = changes {
        warnings.extend(changes.omitted_warning_codes.iter().copied());
    }
    let mut lifecycle = changes
        .and_then(|c| c.lifecycle.as_ref())
        .map(lifecycle_report)
        .unwrap_or(Value::Null);
    let before = truncated_fields.len();
    status_text(&mut lifecycle, "/lifecycle", &mut truncated_fields);
    if !lifecycle.is_null() && truncated_fields.len() != before {
        lifecycle["details_complete"] = json!(false);
    }
    warnings.sort_unstable();

    warnings.dedup();
    truncated_fields.sort_unstable();
    json!({
        "schema_version": 1,
        "operation": operation,
        "outcome": outcome,
        "project": project,
        "project_path": project_path.as_ref().map(|path| paths::encode(path)),
        "changes": {"state": state, "files": files, "paths": native_files},
        "truncated_fields": truncated_fields,
        "warnings": warnings,
        "warning_details": warning_details,
        "warning_details_complete": warning_details_complete,
        "warning_details_omitted": changes.map_or(0, |c| c.omitted_warnings + c.warnings.len().saturating_sub(1024)),
        "lifecycle": lifecycle,
        "errors": [],
        "retry": null,
        "data": null,
    })
}

fn lifecycle_report(report: &crate::application::outcome::LifecycleReport) -> Value {
    let mut packages = report.packages.iter().collect::<Vec<_>>();
    packages.sort_by(|a, b| a.identity.key.cmp(&b.identity.key));
    json!({"coverage": report.coverage, "reason": report.reason,
        "selected_packages": report.selected_packages, "inspected_packages": report.inspected_packages,
        "details_complete": report.omitted_packages == 0 && report.packages.iter().all(|p| p.omitted_hooks == 0),
        "omitted_packages": report.omitted_packages,
        "hooks_executed": report.executed, "generated_outputs_reused": report.reused, "hooks_skipped": report.skipped,
        "packages": packages.into_iter().map(|p| {
            let mut hooks = p.hooks.iter().collect::<Vec<_>>();
            hooks.sort_by(|a,b| a.hook.cmp(&b.hook));
            json!({"identity": lifecycle_identity(&p.identity), "discovery": p.discovery,
                "omitted_hooks": p.omitted_hooks,
                "hooks": hooks.into_iter().map(|h| json!({"hook": h.hook, "authorization": h.authorization,
                    "disposition": h.disposition, "reason": h.reason})).collect::<Vec<_>>()})
        }).collect::<Vec<_>>()})
}

fn lifecycle_identity(identity: &crate::application::outcome::LifecycleIdentity) -> Value {
    let key = &identity.key;
    json!({"key": key.to_string(), "name": key.name.to_string(),
        "version": key.version.to_string(), "source": key.source.package_source().expect("verified artifact source").to_string(),
        "archive_digest": identity.archive_digest,
        "peer_context": key.peer_context,
        "platform_context": key.platform_context})
}

/// Emit an unchanged-project status envelope with bounded, sanitized report strings.
/// Output truncation changes the assessment and exit status to failure, so shortening
/// untrusted metadata cannot silently produce a successful preflight result.
pub(crate) fn status_result(report: &crate::application::status::StatusReport) -> ExitCode {
    let outcome = crate::application::outcome::OperationOutcome::unchanged(&report.project);
    let blocked = report.assessment == "blocked";
    let mut result = envelope(
        "status",
        if blocked { "failure" } else { "success" },
        Some(&outcome),
    );
    let mut data = serde_json::to_value(report).expect("status consists of JSON values");
    let mut truncated = result["truncated_fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    status_text(&mut data, "/data", &mut truncated);
    let shortened = !truncated.is_empty();
    if shortened {
        data["truncated"] = json!(true);
        data["assessment"] = json!("blocked");
        result["outcome"] = json!("failure");
    }
    result["errors"] = json!(
        report
            .blockers
            .iter()
            .map(|b| json!({"code": b.code}))
            .collect::<Vec<_>>()
    );
    if report.truncated || shortened {
        result["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!({"code": "INSPECTION_TRUNCATED"}));
    }
    result["data"] = data;
    truncated.sort_unstable();
    result["truncated_fields"] = json!(truncated);
    emit(result, u8::from(blocked || shortened))
}

/// Sanitize every report string and record shortened fields by escaped JSON pointer.
fn status_text(value: &mut Value, pointer: &str, truncated: &mut Vec<String>) {
    match value {
        Value::String(text) => *text = recorded_text(&*text, pointer, truncated),
        Value::Array(values) => {
            for (index, value) in values.iter_mut().enumerate() {
                status_text(value, &format!("{pointer}/{index}"), truncated);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                status_text(
                    value,
                    &format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1")),
                    truncated,
                );
            }
        }
        _ => {}
    }
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

/// Emit a schema-v1 operation failure envelope and return exit status 1.
/// Callers must supply bounded, non-sensitive structured context rather than raw
/// capability diagnostics.
pub(crate) fn operation_error(operation: &str, code: &str, context: Option<Value>) -> ExitCode {
    let mut result = envelope(operation, "failure", None);
    result["errors"] = json!([{"code": code, "phase": "operation", "context": context}]);
    emit(result, 1)
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
    diagnostic::record(&mut result, &failure.error, "operation");
    if let Some(error) = &failure.recovery_error {
        diagnostic::record(&mut result, error, "recovery");
    }
    result["retry"] = json!(match failure.retry {
        RetryAdvice::AfterCorrection => "after_correction",
        RetryAdvice::AfterContention => "after_contention",
        RetryAdvice::DoNotRepeat => "do_not_repeat",
        RetryAdvice::RecoverFirst => "recover_first",
    });
    result
}

/// Emit a successful version 1 result with typed changes and command-specific data.
pub(crate) fn completed(outcome: &OperationOutcome, operation: &str, data: Value) -> ExitCode {
    let mut result = envelope(operation, "success", Some(outcome));
    result["data"] = data;
    emit(result, 0)
}

pub(crate) fn selection(
    report: &crate::application::selection::SelectionReport,
    operation: &str,
) -> ExitCode {
    let mut result = envelope(operation, "success", Some(&report.outcome));
    let mut data = json!({
        "mode": "lockfile_only", "packages": report.package_count,
        "pending_receipts": report.pending_receipts, "imported": report.imported,
        "metadata_lookups": report.metadata_lookups,
        "copied_source_preparations": report.copied_source_preparations,
        "registry_artifact_requests": 0, "store_writes": 0,
        "hooks_executed": 0, "installed_layout_changed": false,
        "candidate_exclusions": report.candidate_exclusions,
    });
    let mut truncated = result["truncated_fields"]
        .as_array()
        .expect("truncation list")
        .iter()
        .map(|v| v.as_str().expect("pointer").to_owned())
        .collect::<Vec<_>>();
    diagnostic::exclusion_truncation(
        &report.candidate_exclusions,
        "/data/candidate_exclusions",
        &mut truncated,
    );
    status_text(&mut data, "/data", &mut truncated);
    truncated.sort_unstable();
    truncated.dedup();
    result["truncated_fields"] = json!(truncated);
    result["data"] = data;
    emit(result, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::outcome::{ErrorKind, OperationalError};
    use std::path::Path;

    #[test]
    fn typed_failure_context_is_bounded_redacted_and_separate_per_phase() {
        let project = tapid_test_support::TempProject::new("json-error-context").unwrap();
        let error = tapid_resolver::ResolveError::Conflict {
            origin: Box::new(tapid_resolver::RequirementOrigin {
                name: "package".parse().unwrap(),
                package: None,
            }),
            registry: "https://user:fixture-secret@example.test?token=hidden#fragment".into(),
            name: "界".repeat(2000),
            requirements: (0..101).rev().map(|i| format!("^{i}.0.0")).collect(),
            available: vec!["2.0.0".into(), "1.0.0".into(), "1.0.0".into()],
        };
        let mut reordered = error.clone();
        if let tapid_resolver::ResolveError::Conflict {
            requirements,
            available,
            ..
        } = &mut reordered
        {
            requirements.reverse();
            available.reverse();
        }
        let failure = OperationFailure::new(
            OperationalError::from(error),
            OperationOutcome::unchanged(project.path()),
            Some(OperationalError::from(
                tapid_lockfile::LockfileError::RootManifestDigestMismatch {
                    expected: "a".repeat(64),
                    actual: "b".repeat(64),
                },
            )),
        );
        let result = failure_result(&failure, "install");
        let reordered = OperationFailure::new(
            OperationalError::from(reordered),
            OperationOutcome::unchanged(project.path()),
            Some(OperationalError::from(
                tapid_lockfile::LockfileError::RootManifestDigestMismatch {
                    expected: "a".repeat(64),
                    actual: "b".repeat(64),
                },
            )),
        );
        assert_eq!(result, failure_result(&reordered, "install"));
        let context = &result["errors"][0]["context"];
        assert_eq!(context["requirements"].as_array().unwrap().len(), 100);
        assert_eq!(context["omitted"]["requirements"], 1);
        assert_eq!(context["available"], json!(["1.0.0", "2.0.0"]));
        assert!(context["package"]["name"].as_str().unwrap().len() <= 4096);
        assert!(
            result["truncated_fields"]
                .as_array()
                .unwrap()
                .contains(&json!("/errors/0/context/package/name"))
        );
        assert_eq!(result["errors"][1]["phase"], "recovery");
        assert_eq!(
            result["errors"][1]["context"]["comparison"]["expected"],
            "a".repeat(64)
        );
        assert_eq!(
            result["errors"][1]["context"]["comparison"]["observed"],
            "b".repeat(64)
        );
        for secret in ["fixture-secret", "hidden", "fragment"] {
            assert!(!result.to_string().contains(secret));
        }
    }

    #[test]
    fn diagnostic_native_manifest_paths_are_never_shortened() {
        let project = tapid_test_support::TempProject::new("json-diagnostic-native-path").unwrap();
        let path = project.path().join("界".repeat(2000)).join("package.json");
        let failure = OperationFailure::unchanged(
            project.path(),
            OperationalError::new(ErrorKind::Manifest, "read failed").at_manifest(&path),
        );
        let result = failure_result(&failure, "install");
        assert_eq!(
            result["errors"][0]["context"]["declaration"]["manifest_path"]["value"],
            path.to_str().unwrap()
        );
        assert!(
            result["truncated_fields"]
                .as_array()
                .unwrap()
                .contains(&json!("/errors/0/context/declaration/manifest"))
        );
    }

    #[test]
    fn manifest_pointer_does_not_hide_a_credential_url_from_redaction() {
        let project = tapid_test_support::TempProject::new("json-pointer-redaction").unwrap();
        let input = r#"{"name":"app","version":"1.0.0","scripts":{"https://user:fixture-secret@example.test/path?token=hidden#fragment":false}}"#;
        let error = tapid_manifest::PackageManifest::parse(input).unwrap_err();
        let context = crate::application::outcome::DiagnosticContext::manifest_error(
            &error.diagnostic(input),
            &project.path().join("package.json"),
        );
        let failure = OperationFailure::unchanged(
            project.path(),
            OperationalError::from_source(ErrorKind::Manifest, error).with_diagnostic(context),
        );
        let result = failure_result(&failure, "install");
        for secret in ["fixture-secret", "hidden", "fragment"] {
            assert!(!result.to_string().contains(secret), "{result}");
        }
    }

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
}

#[cfg(test)]
mod lifecycle_tests;
