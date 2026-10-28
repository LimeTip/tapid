//! Read-only audit entry point. Never opens a store or project transaction.
use crate::filesystem::{atomic::digest_bytes, read::read_bounded_file};
use std::path::Path;
use tapid_lockfile::{ImportedNpmLockfile, Lockfile};

mod collection;
mod requests;
pub(crate) use collection::{AuditEvidence, AuditFailure, Provider};

pub(crate) struct AuditReport {
    pub(crate) failures: Vec<AuditFailure>,
    pub(crate) not_checked: usize,
    pub(crate) evidence: Vec<AuditEvidence>,
    pub(crate) outcome: tapid_policy::AuditOutcome,
    pub(crate) format: &'static str,
    pub(crate) recorded_subjects: usize,
    pub(crate) reachable_subjects: Option<usize>,
    pub(crate) roots_recorded: Option<bool>,
    pub(crate) unresolved_peers: Option<usize>,
    pub(crate) requestable_occurrences: Option<usize>,
    pub(crate) excluded_occurrences: Option<usize>,
}

/// Inspect recorded inputs only. Projected requests do not authorize disclosure;
/// provider activation is unavailable, so empty graphs remain not checked too.
pub(crate) fn inspect(project: &Path) -> Result<AuditReport, &'static str> {
    inspect_with_provider(project, None)
}

pub(crate) fn inspect_with_provider(
    project: &Path,
    provider: Option<&mut Provider<'_>>,
) -> Result<AuditReport, &'static str> {
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
        let reachable = graph.reachable();
        let inventory = requests::native(&graph);
        let (mut evidence, outcome, failures, not_checked) = collection::collect(
            &inventory,
            reachable.roots_recorded && reachable.unresolved_peers.is_empty(),
            provider,
        );
        let roots = graph
            .roots()
            .iter()
            .map(|(name, key)| tapid_lockfile::LockedGraphRoot {
                name: name.clone(),
                key: key.clone(),
                kind: "recorded".into(),
            })
            .collect::<Vec<_>>();
        for entry in &mut evidence {
            let paths = graph.paths_to_key(&roots, &entry.key);
            entry.paths = paths.paths;
            entry.paths_truncated = paths.truncated;
        }
        Ok(AuditReport {
            failures,
            not_checked,
            evidence,
            outcome,
            format: "native",
            recorded_subjects: graph.nodes().len(),
            reachable_subjects: Some(reachable.occurrences.len()),
            roots_recorded: Some(reachable.roots_recorded),
            unresolved_peers: Some(reachable.unresolved_peers.len()),
            requestable_occurrences: Some(inventory.requests.len()),
            excluded_occurrences: Some(inventory.excluded),
        })
    } else {
        let lock = ImportedNpmLockfile::from_json(input).map_err(|_| "AUDIT_LOCK_INVALID")?;
        lock.validate_replay(manifest, &digest)
            .map_err(|_| "AUDIT_LOCK_MISMATCH")?;
        let graph = lock.graph().map_err(|_| "AUDIT_LOCK_INVALID")?;
        Ok(AuditReport {
            failures: Vec::new(),
            not_checked: graph.packages.len() + graph.workspaces.len(),
            evidence: Vec::new(),
            outcome: tapid_policy::AuditOutcome::Incomplete,
            format: "imported_npm",
            recorded_subjects: graph.packages.len() + graph.workspaces.len(),
            reachable_subjects: None,
            roots_recorded: None,
            unresolved_peers: None,
            requestable_occurrences: None,
            excluded_occurrences: None,
        })
    }
}

#[cfg(test)]
mod collection_tests;
