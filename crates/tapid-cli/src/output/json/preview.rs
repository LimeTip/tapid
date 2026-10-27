use super::{emit, envelope, recorded_text};
use serde_json::{Value, json};
use std::process::ExitCode;

pub(crate) fn preview(
    report: &crate::application::preview::PreviewReport,
    operation: &str,
) -> ExitCode {
    let mut result = envelope(operation, "success", Some(&report.outcome));
    let mut data = serde_json::to_value(report).expect("preview serialization");
    let mut truncated = result["truncated_fields"]
        .as_array()
        .expect("truncation list")
        .iter()
        .map(|value| value.as_str().expect("truncation pointer").to_owned())
        .collect::<Vec<_>>();
    super::diagnostic::exclusion_truncation(
        &report.candidate_exclusions,
        "/data/candidate_exclusions",
        &mut truncated,
    );
    sanitize_preview(&mut data, "/data", &mut truncated);
    result["data"] = data;
    truncated.sort_unstable();
    truncated.dedup();
    result["truncated_fields"] = json!(truncated);
    emit(result, 0)
}

fn sanitize_preview(value: &mut Value, pointer: &str, truncated: &mut Vec<String>) {
    match value {
        Value::String(value) => *value = recorded_text(&*value, pointer, truncated),
        Value::Array(values) => {
            for (index, value) in values.iter_mut().enumerate() {
                sanitize_preview(value, &format!("{pointer}/{index}"), truncated);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                let key = key.replace('~', "~0").replace('/', "~1");
                sanitize_preview(value, &format!("{pointer}/{key}"), truncated);
            }
        }
        _ => {}
    }
}
