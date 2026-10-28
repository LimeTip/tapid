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

pub(crate) fn cache_fetch(
    project: &std::path::Path,
    root: &std::path::Path,
    report: crate::application::cache_fetch::Report,
    json_output: bool,
) -> ExitCode {
    let success = report.result.is_ok();
    let target = format!(
        "{}/{}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        crate::online::current_libc().unwrap_or("unspecified")
    );
    let counters = json!({"already_verified_trees":report.observations.count("already_verified"), "fetched_verified_trees":report.observations.count("fetched_verified"),"newly_published_trees":report.newly_published,"network_retrievals":report.observations.network_retrievals,"fixture_reads":report.observations.fixture_reads,"local_reads":report.observations.local_reads,"unsupported_instances":report.count("unsupported"),"unavailable_instances":report.count("unavailable"),"failed_instances":report.count("failed"),"unprocessed_instances":report.count("unprocessed")});
    let code = if success { 0 } else { 1 };
    if !json_output {
        println!(
            "Source cache preparation: {}",
            if success { "complete" } else { "failed" }
        );
        println!("Project: {}", super::text(project.display()).0);
        println!("Package cache: {}", super::text(root.display()).0);
        println!("Target: {target}");
        println!(
            "Scope: complete_locked_graph; verified sources only. Lifecycle outputs, keys, policy, runtime and toolchain readiness are not prepared."
        );
        println!(
            "{} locked instances, {} unique trees; available at completion: {}",
            report.logical_instances,
            report
                .unique_trees
                .map_or_else(|| "unknown".to_owned(), |n| n.to_string()),
            report
                .available
                .map_or_else(|| "unknown".to_owned(), |n| n.to_string())
        );
        println!("Completed work: {counters}");
        if let Err(error) = report.result {
            eprintln!(
                "error: {error}\ndiagnostic: {}",
                report.reason.unwrap_or(error.kind.code())
            );
        }
        return ExitCode::from(code);
    }
    let mut result = envelope("cache", if success { "success" } else { "failure" }, None);
    result["data"] = json!({"action":"fetch", "project_path":paths::encode(project), "store_path":paths::encode(root), "scope":"complete_locked_graph", "target":target, "readiness":"verified_sources_only", "lifecycle_ready":false, "logical_instances":report.logical_instances,"unique_trees":report.unique_trees,"available_at_completion":report.available});
    result["data"]["counters"] = counters;
    result["data"]["sources_available"] = json!(success);
    result["data"]["store_effects"] = json!({"newly_committed_trees":report.newly_published});
    let mut truncated = Vec::new();
    let entries = report.entries.iter().take(100).enumerate().map(|(index, entry)| json!({"package":recorded_text(&entry.key, &format!("/data/entries/{index}/package"), &mut truncated),"tree_digest":entry.digest.as_ref().map(|digest| recorded_text(digest, &format!("/data/entries/{index}/tree_digest"), &mut truncated)),"state":report.entry_state(entry)})).collect::<Vec<_>>();
    result["data"]["entries"] = json!(entries);
    result["data"]["entries_total"] = json!(report.entries.len());
    result["data"]["entries_emitted"] = json!(entries.len());
    result["data"]["entries_omitted"] = json!(report.entries.len() - entries.len());
    truncated.sort_unstable();
    result["truncated_fields"] = json!(truncated);
    if let Err(error) = report.result {
        result["errors"] =
            json!([{"code":report.reason.unwrap_or(error.kind.code()),"phase":"operation"}]);
        if error.kind == crate::application::outcome::ErrorKind::StoreBusy {
            result["retry"] = json!("after_contention");
        }
    }
    emit(result, code)
}
