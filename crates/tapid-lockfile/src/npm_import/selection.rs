use super::{ImportedNpmLockfile, NpmImportError, parse};
use crate::{OmissionReason, SelectionProfile, SelectionTarget};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Recorded cuts use declaring npm placements, including the empty root placement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportedSelection {
    pub target: SelectionTarget,
    pub profile: SelectionProfile,
    pub omissions: Vec<ImportedOmission>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportedOmission {
    pub declaring_placement: String,
    pub name: String,
    pub requirement: String,
    pub reason: OmissionReason,
}

impl ImportedSelection {
    pub(super) fn validate(&self, lock: &ImportedNpmLockfile) -> Result<(), NpmImportError> {
        let invalid = || {
            parse::error(
                "/placementSelection",
                "root",
                "selection",
                "invalid imported omission facts",
            )
        };
        if !self.target.is_valid()
            || self.omissions.len() > 100_000
            || self.omissions.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(invalid());
        }
        let graph = lock.graph()?;
        let placements: BTreeMap<_, _> = graph
            .packages
            .keys()
            .chain(graph.workspace_links.keys())
            .map(|p| (p.clone(), ()))
            .collect();
        let mut declarations = BTreeSet::new();
        for omission in &self.omissions {
            let parent = &omission.declaring_placement;
            if (!parent.is_empty()
                && !graph.packages.contains_key(parent)
                && !graph.workspaces.contains_key(parent))
                || !declarations.insert((parent, &omission.name))
                || omission.name.parse::<tapid_core::PackageName>().is_err()
                || omission.requirement.len() > 4096
                || omission
                    .requirement
                    .parse::<tapid_resolver::Requirement>()
                    .is_err()
                || lock.npm_lock["packages"][parent]["optionalDependencies"][&omission.name]
                    .as_str()
                    != Some(&omission.requirement)
                || parse::lookup(&placements, parent, &omission.name, false).is_some()
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

impl ImportedNpmLockfile {
    /// Validate all facts before replacing the legacy imported contract.
    pub fn set_placement_selection(
        &mut self,
        selection: ImportedSelection,
    ) -> Result<(), NpmImportError> {
        selection.validate(self)?;
        self.placement_selection = Some(selection);
        self.lockfile_version = 12;
        Ok(())
    }

    pub fn placement_selection(&self) -> Option<&ImportedSelection> {
        self.placement_selection.as_ref()
    }
}
