use super::{emit, envelope, paths, recorded_text};
use serde_json::json;
use std::process::ExitCode;

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
            let invalid_path = matches!(
                &error,
                tapid_store::IngestError::InvalidRoot | tapid_store::IngestError::CachePath(_)
            );
            result["errors"] = json!([{"code": if busy { "CACHE_BUSY" } else if invalid_path { "CACHE_PATH_INVALID" } else { "CACHE_MAINTENANCE_FAILED" }, "phase": "operation"}]);
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
