//! Shared bounds for advisory and rejected compatibility evidence.
use serde_json::{Value, json};

pub(super) fn attach(result: &mut Value, report: Option<&crate::compatibility::Report>) {
    let Some(report) = report else { return };
    let mut data = serde_json::to_value(report).expect("compatibility facts");
    let facts = data["facts"].as_array_mut().expect("compatibility facts");
    let omitted = facts.len().saturating_sub(1024);
    facts.truncate(1024);
    let mut truncated = Vec::new();
    if omitted > 0 {
        truncated.push("/data/compatibility/facts".into());
    }
    // Native path encodings already have independent lossless bounds. Display text
    // sanitization must not change their bytes or base64/UTF-16 representation.
    let executable_path = data["runtime"]
        .as_object_mut()
        .unwrap()
        .remove("executable_path");
    super::status_text(&mut data, "/data/compatibility", &mut truncated);
    if let Some(path) = executable_path {
        data["runtime"]["executable_path"] = path;
    }
    data["omitted_facts"] = json!(omitted);
    data["details_complete"] = json!(truncated.is_empty());
    if result["data"].is_null() {
        result["data"] = json!({});
    }
    result["data"]["compatibility"] = data;
    result["truncated_fields"]
        .as_array_mut()
        .unwrap()
        .extend(truncated.into_iter().map(Value::String));
}
