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
        let selected = graph.selected_paths(
            &self.target.os,
            &self.target.cpu,
            self.target.libc.as_deref(),
        )?;
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
            {
                return Err(invalid());
            }
            if let Some(path) = parse::lookup(&placements, parent, &omission.name, false) {
                let package = graph.packages.get(&path).ok_or_else(invalid)?;
                let constraints = tapid_core::PackagePlatform {
                    os: package.os.clone(),
                    cpu: package.cpu.clone(),
                    libc: package.libc.clone(),
                };
                if omission.reason != OmissionReason::IncompatiblePlatform
                    || selected.contains(&path)
                    || constraints.matches(
                        &self.target.os,
                        &self.target.cpu,
                        self.target.libc.as_deref(),
                    )
                {
                    return Err(invalid());
                }
            } else if omission.reason != OmissionReason::ImportedPlacementAbsent {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

impl ImportedNpmLockfile {
    /// Record absent optional placements and target-specific platform cuts.
    /// Required reachability is checked before any selection facts are changed.
    pub fn record_platform_selection(
        &mut self,
        target: SelectionTarget,
        profile: SelectionProfile,
    ) -> Result<(), NpmImportError> {
        let graph = self.graph()?;
        let selected = graph.selected_paths(&target.os, &target.cpu, target.libc.as_deref())?;
        let placements: BTreeMap<_, _> = graph
            .packages
            .keys()
            .chain(graph.workspace_links.keys())
            .map(|p| (p.clone(), ()))
            .collect();
        let mut omissions = Vec::new();
        for parent in std::iter::once("")
            .chain(graph.workspaces.keys().map(String::as_str))
            .chain(selected.iter().map(String::as_str))
        {
            let Some(declarations) =
                self.npm_lock["packages"][parent]["optionalDependencies"].as_object()
            else {
                continue;
            };
            for (name, requirement) in declarations {
                let reason = match parse::lookup(&placements, parent, name, false) {
                    None => OmissionReason::ImportedPlacementAbsent,
                    Some(path)
                        if graph.packages.contains_key(&path) && !selected.contains(&path) =>
                    {
                        OmissionReason::IncompatiblePlatform
                    }
                    Some(_) => continue,
                };
                omissions.push(ImportedOmission {
                    declaring_placement: parent.to_owned(),
                    name: name.clone(),
                    requirement: requirement
                        .as_str()
                        .expect("validated requirement")
                        .to_owned(),
                    reason,
                });
            }
        }
        if !omissions.is_empty() {
            omissions.sort();
            self.set_placement_selection(ImportedSelection {
                target,
                profile,
                omissions,
            })?;
        }
        Ok(())
    }

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
