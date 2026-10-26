use super::*;
use crate::application::install::InstallReport;

pub(crate) fn dependency_edits(
    result: &Result<InstallReport, OperationFailure>,
    edits: Value,
) -> ExitCode {
    emit(render(result, edits), u8::from(result.is_err()))
}

fn render(result: &Result<InstallReport, OperationFailure>, mut edits: Value) -> Value {
    let (mut output, state) = match result {
        Ok(report) => (
            envelope("edit-dependencies", "success", Some(&report.outcome)),
            report.outcome.state,
        ),
        Err(failure) => (
            failure_result(failure, "edit-dependencies"),
            failure.outcome.state,
        ),
    };
    let disposition = match state {
        ChangeState::Committed | ChangeState::CommittedCleanupPending => "committed",
        ChangeState::RecoveryRequired => "unknown",
        _ => "not_committed",
    };
    for (index, edit) in edits
        .as_array_mut()
        .expect("edits array")
        .iter_mut()
        .enumerate()
    {
        edit["index"] = json!(index);
        edit["disposition"] = json!(disposition);
    }
    let graph = match result
        .as_ref()
        .ok()
        .and_then(|report| report.package_changes.as_ref())
    {
        Some(changes) => {
            json!({"complete": changes.omitted == 0, "changes": changes.records, "omitted": changes.omitted})
        }
        None => json!({"complete": false, "changes": [], "omitted": null}),
    };
    output["data"] = json!({"edits": edits, "graph": graph});
    let mut truncated = Vec::new();
    status_text(&mut output["data"], "/data", &mut truncated);
    if truncated
        .iter()
        .any(|field| field.starts_with("/data/graph/"))
    {
        output["data"]["graph"]["complete"] = json!(false);
    }
    output["truncated_fields"]
        .as_array_mut()
        .expect("truncation list")
        .extend(truncated.into_iter().map(Value::String));
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{
        install::{GraphChange, PackageChanges},
        outcome::{ErrorKind, OperationalError},
    };

    #[test]
    fn dependency_edit_dispositions_follow_durable_outcome_even_on_failure() {
        let project = tapid_test_support::TempProject::new("edit-render-states").unwrap();
        for (state, disposition) in [
            (ChangeState::Unchanged, "not_committed"),
            (ChangeState::RolledBack, "not_committed"),
            (ChangeState::Committed, "committed"),
            (ChangeState::CommittedCleanupPending, "committed"),
            (ChangeState::RecoveryRequired, "unknown"),
        ] {
            let mut outcome = OperationOutcome::unchanged(project.path());
            outcome.state = state;
            let result = Err(OperationFailure::new(
                OperationalError::new(ErrorKind::Transaction, "injected"),
                outcome,
                None,
            ));
            let output = render(
                &result,
                json!([{"op":"remove","section":"dependencies","name":"old"}]),
            );
            assert_eq!(output["outcome"], "failure");
            assert_eq!(output["data"]["edits"][0]["disposition"], disposition);
            assert_eq!(output["data"]["graph"]["complete"], false);
            assert!(output["data"]["graph"]["omitted"].is_null());
        }
    }

    #[test]
    fn dependency_edit_graph_presentation_exposes_omissions_and_scalar_truncation() {
        let project = tapid_test_support::TempProject::new("edit-render-bounds").unwrap();
        for (omitted, long) in [(0, false), (1, false), (0, true)] {
            let changes = PackageChanges {
                records: vec![GraphChange::Package {
                    identity: if long {
                        "x".repeat(5000)
                    } else {
                        "package".into()
                    },
                    action: "added",
                }],
                omitted,
                ..Default::default()
            };
            let report = Ok(InstallReport {
                package_count: 1,
                replayed: false,
                package_changes: Some(changes),
                outcome: OperationOutcome::unchanged(project.path()),
            });
            let output = render(&report, json!([]));
            assert_eq!(output["data"]["graph"]["complete"], omitted == 0 && !long);
            assert_eq!(output["data"]["graph"]["omitted"], omitted);
            assert_eq!(
                output["truncated_fields"].as_array().unwrap().len(),
                usize::from(long)
            );
        }
    }
}
