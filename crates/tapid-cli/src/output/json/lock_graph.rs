//! Paginated, protected persisted observations. Display shortening is not failure.
use super::{emit, envelope, status_text};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::process::ExitCode;

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

pub(crate) fn lock_graph(
    mut data: Value,
    records: &[impl Serialize],
    binding: &str,
    cursor: Option<&str>,
    limit: usize,
    supported: bool,
) -> ExitCode {
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("tapid-lock-page-v1\0{binding}"))
    );
    let offset = match cursor {
        None => 0,
        Some(cursor) => {
            let mut fields = cursor.split('.');
            let offset = match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some("v1"), Some(offset), Some(hash), None) if hash == digest => {
                    offset.parse::<usize>().ok()
                }
                _ => None,
            };
            match offset.filter(|offset| *offset > 0 && *offset < records.len()) {
                Some(offset) => offset,
                None => return super::operation_error("lock", "LOCK_GRAPH_INVALID_CURSOR", None),
            }
        }
    };
    let end = offset.saturating_add(limit).min(records.len());
    let mut page = Vec::new();
    for record in &records[offset..end] {
        match serde_json::to_value(record) {
            Ok(record) => page.push(record),
            Err(_) => return super::operation_error("lock", "LOCK_GRAPH_OUTPUT_LIMIT", None),
        }
    }
    loop {
        let next = offset + page.len();
        data["records"] = json!(page);
        data["detail_coverage"] = json!({"offset":offset,"returned":page.len(),"total":records.len(),"omitted":records.len()-page.len(),"remaining":records.len()-next,"complete":offset==0 && next==records.len()});
        data["continuation"] = if next < records.len() {
            json!(format!("v1.{next}.{digest}"))
        } else {
            Value::Null
        };
        let mut result = envelope("lock", if supported { "success" } else { "failure" }, None);
        let mut protected = data.clone();
        let mut truncated = Vec::new();
        status_text(&mut protected, "/data", &mut truncated);
        result["data"] = protected;
        result["truncated_fields"] = json!(truncated);
        if !supported {
            result["errors"] = json!([{"code":"LOCK_GRAPH_UNSUPPORTED_COMPARISON"}]);
        }
        if serde_json::to_vec(&result).is_ok_and(|bytes| bytes.len() < MAX_RESPONSE_BYTES) {
            return emit(result, u8::from(!supported));
        }
        if page.len() <= 1 {
            return super::operation_error("lock", "LOCK_GRAPH_OUTPUT_LIMIT", None);
        }
        page.truncate(page.len() / 2);
    }
}
