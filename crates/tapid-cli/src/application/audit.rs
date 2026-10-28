//! Read-only audit entry point. Never opens a store or project transaction.
use crate::filesystem::{atomic::digest_bytes, read::read_bounded_file};
use std::path::Path;
use tapid_lockfile::{ImportedNpmLockfile, Lockfile};

pub(crate) struct AuditReport {
    pub(crate) format: &'static str,
    pub(crate) recorded_subjects: usize,
}

/// Inspect recorded inputs only. Provider activation and exact request projection
/// are not available yet, so even an empty graph remains explicitly not checked.
pub(crate) fn inspect(project: &Path) -> Result<AuditReport, &'static str> {
    const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
    let manifest = read_bounded_file(&project.join("package.json"), MAX_INPUT_BYTES)
        .map_err(|_| "AUDIT_MANIFEST_UNREADABLE")?;
    let manifest = std::str::from_utf8(&manifest).map_err(|_| "AUDIT_MANIFEST_INVALID")?;
    tapid_manifest::PackageManifest::parse(manifest).map_err(|_| "AUDIT_MANIFEST_INVALID")?;
    let bytes = read_bounded_file(&project.join("tapid.lock"), MAX_INPUT_BYTES)
        .map_err(|_| "AUDIT_LOCK_UNREADABLE")?;
    let input = std::str::from_utf8(&bytes).map_err(|_| "AUDIT_LOCK_INVALID")?;
    let digest = digest_bytes(manifest.as_bytes());
    if let Ok(lock) = Lockfile::from_json(input) {
        lock.validate_replay(&digest)
            .map_err(|_| "AUDIT_LOCK_MISMATCH")?;
        let graph = lock.graph().map_err(|_| "AUDIT_LOCK_INVALID")?;
        Ok(AuditReport {
            format: "native",
            recorded_subjects: graph.nodes().len(),
        })
    } else {
        let lock = ImportedNpmLockfile::from_json(input).map_err(|_| "AUDIT_LOCK_INVALID")?;
        lock.validate_replay(manifest, &digest)
            .map_err(|_| "AUDIT_LOCK_MISMATCH")?;
        let graph = lock.graph().map_err(|_| "AUDIT_LOCK_INVALID")?;
        Ok(AuditReport {
            format: "imported_npm",
            recorded_subjects: graph.packages.len() + graph.workspaces.len(),
        })
    }
}
