//! Render only typed, allowlisted context with the common display/path contracts.
use super::*;
use crate::application::outcome::{Declaration, DiagnosticContext, OperationalError};

const MAX_ENTRIES: usize = 100;

fn declaration(value: &Declaration) -> Value {
    let mut result = serde_json::to_value(value).expect("typed declaration");
    result["manifest"] = json!(
        value
            .manifest
            .as_ref()
            .map(|path| path.display().to_string())
    );
    result["manifest_path"] = value
        .manifest
        .as_ref()
        .map_or(Value::Null, |path| paths::encode(path));
    result
}

fn context(value: &DiagnosticContext) -> Value {
    let mut result = serde_json::to_value(value).expect("typed diagnostic context");
    if let Some(location) = &value.declaration {
        result["declaration"] = declaration(location);
    }
    if let Some(peer) = &value.peer
        && let Some(location) = &peer.declaration
    {
        result["peer"]["declaration"] = declaration(location);
    }
    let mut requirements = value.requirements.iter().collect::<Vec<_>>();
    requirements.sort_unstable();
    requirements.dedup();
    let requirement_count = requirements.len();
    result["requirements"] = json!(
        requirements
            .into_iter()
            .take(MAX_ENTRIES)
            .map(|requirement| {
                let mut result = serde_json::to_value(requirement).expect("typed requirement");
                if let Some(location) = &requirement.declaration {
                    result["declaration"] = declaration(location);
                }
                result
            })
            .collect::<Vec<_>>()
    );
    let mut available = value.available.clone();
    available.sort_unstable();
    available.dedup();
    let available_count = available.len();
    available.truncate(MAX_ENTRIES);
    result["available"] = json!(available);
    result["omitted"] = json!({"requirements": requirement_count.saturating_sub(MAX_ENTRIES), "available": available_count.saturating_sub(MAX_ENTRIES)});
    result
}

fn bound(value: &mut Value, pointer: &str, truncated: &mut Vec<String>) {
    match value {
        Value::String(string) => *string = recorded_text(&*string, pointer, truncated),
        Value::Array(values) => {
            for (index, value) in values.iter_mut().enumerate() {
                bound(value, &format!("{pointer}/{index}"), truncated);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                // Native paths are lossless and independently bounded by paths::encode.
                if key != "manifest_path" {
                    bound(value, &format!("{pointer}/{key}"), truncated);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn exclusion_truncation(
    value: &crate::application::outcome::CandidateExclusions,
    pointer: &str,
    truncated: &mut Vec<String>,
) {
    for (index, group) in value.packages.iter().enumerate() {
        let pointer = format!("{pointer}/packages/{index}");
        for field in &group.truncated_fields {
            truncated.push(format!("{pointer}/{field}"));
        }
        for (index, candidate) in group.candidates.iter().enumerate() {
            for field in &candidate.truncated_fields {
                truncated.push(format!("{pointer}/candidates/{index}/{field}"));
            }
        }
    }
}

pub(super) fn error(
    error: &OperationalError,
    phase: Option<&str>,
    pointer: &str,
    truncated: &mut Vec<String>,
) -> Value {
    let mut result =
        json!({"code": error.kind.code(), "context": error.diagnostic.as_deref().map(context)});
    if let Some(phase) = phase {
        result["phase"] = json!(phase);
    }
    if let Some(context) = &error.diagnostic {
        exclusion_truncation(
            &context.candidate_exclusions,
            &format!("{pointer}/context/candidate_exclusions"),
            truncated,
        );
    }
    bound(
        &mut result["context"],
        &format!("{pointer}/context"),
        truncated,
    );
    result
}

pub(super) fn record(result: &mut Value, error: &OperationalError, phase: &str) {
    let index = result["errors"].as_array().expect("error array").len();
    let mut truncated = Vec::new();
    let error = self::error(
        error,
        Some(phase),
        &format!("/errors/{index}"),
        &mut truncated,
    );
    result["errors"].as_array_mut().unwrap().push(error);
    let fields = result["truncated_fields"].as_array_mut().unwrap();
    fields.extend(truncated.into_iter().map(Value::String));
    fields.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    fields.dedup();
}
