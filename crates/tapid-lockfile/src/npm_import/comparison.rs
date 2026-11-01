use super::{ImportedNpmLockfile, NpmImportError};

/// Counts registry placements in the complete persisted imported graph, not work performed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImportedSelectionCounts {
    pub added: usize,
    pub changed: usize,
    pub reused: usize,
    pub removed: usize,
}

/// A comparison is complete or unavailable; partial totals are never returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportedSelectionComparison {
    Available(ImportedSelectionCounts),
    Unavailable(ImportedSelectionUnavailable),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportedSelectionUnavailable {
    EntryPointsChanged,
    WorkspaceTopologyChanged,
}

impl ImportedNpmLockfile {
    /// Compare placement selections, excluding verification receipts and manifest digests.
    /// Validation errors remain errors. Local topology changes cannot be attributed
    /// accurately to registry placement counts and make the whole comparison unavailable.
    pub fn compare_selections(
        &self,
        after: &Self,
    ) -> Result<ImportedSelectionComparison, NpmImportError> {
        let before = self.graph()?;
        let after = after.graph()?;
        if before.roots != after.roots
            || before.optional_roots != after.optional_roots
            || before.member_roots != after.member_roots
        {
            return Ok(ImportedSelectionComparison::Unavailable(
                ImportedSelectionUnavailable::EntryPointsChanged,
            ));
        }
        if before.workspaces != after.workspaces || before.workspace_links != after.workspace_links
        {
            return Ok(ImportedSelectionComparison::Unavailable(
                ImportedSelectionUnavailable::WorkspaceTopologyChanged,
            ));
        }
        let mut counts = ImportedSelectionCounts::default();
        for (path, package) in &after.packages {
            match before.packages.get(path) {
                None => counts.added += 1,
                Some(previous) if same_selection(previous, package) => counts.reused += 1,
                Some(_) => counts.changed += 1,
            }
        }
        counts.removed = before
            .packages
            .keys()
            .filter(|path| !after.packages.contains_key(*path))
            .count();
        Ok(ImportedSelectionComparison::Available(counts))
    }
}

fn same_selection(before: &super::ImportedNpmPackage, after: &super::ImportedNpmPackage) -> bool {
    fn set(values: &[String]) -> std::collections::BTreeSet<&str> {
        values.iter().map(String::as_str).collect()
    }
    before.name == after.name
        && before.version == after.version
        && before.registry == after.registry
        && before.resolved == after.resolved
        && before.integrity == after.integrity
        && before.dependencies == after.dependencies
        && before.optional_dependencies == after.optional_dependencies
        && before.peers == after.peers
        && before.optional_peers == after.optional_peers
        && before.optional == after.optional
        && set(&before.os) == set(&after.os)
        && set(&before.cpu) == set(&after.cpu)
        && set(&before.libc) == set(&after.libc)
}
