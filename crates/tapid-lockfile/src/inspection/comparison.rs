use std::collections::BTreeMap;

use super::{
    GraphAvailability, GraphCoverage, GraphInstance, GraphRelationship, GraphScope, GraphSnapshot,
    SnapshotError,
};

/// Observed differences. Counts describe overlapping views, never upgrade counts.
#[derive(Debug, serde::Serialize)]
pub struct GraphComparison {
    /// False means graph delta fields are unavailable, never zero-change evidence.
    pub selected_graph_supported: bool,
    pub unsupported_reason: Option<&'static str>,
    pub scope: GraphScope,
    pub before_digest: String,
    pub after_digest: String,
    pub before_availability: GraphAvailability,
    pub after_availability: GraphAvailability,
    pub before_coverage: GraphCoverage,
    pub after_coverage: GraphCoverage,
    pub added_instances: Vec<GraphInstance>,
    pub removed_instances: Vec<GraphInstance>,
    pub changed_selections: usize,
    pub relationship_changes: Vec<GraphRelationshipChange>,
    pub evidence_changes: Vec<GraphEvidenceChange>,
}

/// A named site retarget, or an added/removed relationship.
#[derive(Debug, serde::Serialize)]
pub struct GraphRelationshipChange {
    pub before: Option<GraphRelationship>,
    pub after: Option<GraphRelationship>,
}

/// An independently observed evidence change, not a selected package upgrade.
#[derive(Debug, serde::Serialize)]
pub struct GraphEvidenceChange {
    pub dimension: String,
    pub subject: Option<String>,
    /// Persisted evidence only; source strings require protection when rendered.
    pub before: BTreeMap<String, String>,
    pub after: BTreeMap<String, String>,
    /// Store-local attestations are compared but never exposed.
    pub attestation_changed: bool,
}

impl GraphSnapshot {
    /// Compare exact native identities and recorded selection sites. Replaced
    /// parents do not establish correspondence between their descendants.
    pub fn compare(&self, after: &Self) -> GraphComparison {
        let supported = (self.schema == 8) == (after.schema == 8);
        let mut evidence_changes = evidence_changes(self, after);
        let before = self.inspect();
        let after = after.inspect();
        if !supported {
            return GraphComparison {
                selected_graph_supported: false,
                unsupported_reason: Some("cross_format_selection_equivalence"),
                scope: GraphScope::All,
                before_digest: before.snapshot_digest,
                after_digest: after.snapshot_digest,
                before_availability: before.availability,
                after_availability: after.availability,
                before_coverage: before.coverage,
                after_coverage: after.coverage,
                added_instances: Vec::new(),
                removed_instances: Vec::new(),
                changed_selections: 0,
                relationship_changes: Vec::new(),
                evidence_changes,
            };
        }
        let placements: BTreeMap<_, _> = before
            .instances
            .iter()
            .chain(after.instances.iter())
            .filter_map(|p| {
                p.placement
                    .as_ref()
                    .map(|path| (p.reference.clone(), path.clone()))
            })
            .collect();
        let old: BTreeMap<_, _> = before.instances.iter().map(|p| (&p.identity, p)).collect();
        let new: BTreeMap<_, _> = after.instances.iter().map(|p| (&p.identity, p)).collect();
        let added_instances = new
            .iter()
            .filter(|(key, _)| !old.contains_key(*key))
            .map(|(_, p)| (*p).clone())
            .collect();
        let removed_instances = old
            .iter()
            .filter(|(key, _)| !new.contains_key(*key))
            .map(|(_, p)| (*p).clone())
            .collect();
        // Root membership has no local selection site; retain exact set deltas.
        // Other sites correspond only through an exact parent and local name.
        let site = |edge: &GraphRelationship| {
            (
                edge.kind.clone(),
                edge.parent
                    .as_ref()
                    .map(|p| placements.get(p).unwrap_or(p).clone()),
                edge.local_name.clone(),
                if edge.local_name.is_none() {
                    Some(edge.target.clone())
                } else {
                    None
                },
            )
        };
        let comparable = |edge: &&GraphRelationship| match edge.kind.as_str() {
            "root_binding" => {
                before.availability.named_root_bindings && after.availability.named_root_bindings
            }
            "root" => before.availability.explicit_roots && after.availability.explicit_roots,
            _ => true,
        };
        let old_edges: BTreeMap<_, _> = before
            .relationships
            .iter()
            .filter(comparable)
            .map(|e| (site(e), e))
            .collect();
        let new_edges: BTreeMap<_, _> = after
            .relationships
            .iter()
            .filter(comparable)
            .map(|e| (site(e), e))
            .collect();
        let mut sites: Vec<_> = old_edges.keys().chain(new_edges.keys()).collect();
        sites.sort();
        sites.dedup();
        let mut relationship_changes = Vec::new();
        let mut changed_selections = 0;
        for site in sites {
            let before = old_edges.get(site).copied();
            let after = new_edges.get(site).copied();
            if before == after {
                continue;
            }
            if site.2.is_some() {
                changed_selections += 1;
            }
            relationship_changes.push(GraphRelationshipChange {
                before: before.cloned(),
                after: after.cloned(),
            });
        }
        let identities: BTreeMap<_, _> = before
            .instances
            .iter()
            .chain(after.instances.iter())
            .map(|p| (p.reference.as_str(), p.identity.as_str()))
            .collect();
        relationship_changes.sort_by(|a, b| {
            super::relationship_order(
                a.before
                    .as_ref()
                    .or(a.after.as_ref())
                    .expect("nonempty change"),
                &identities,
            )
            .cmp(&super::relationship_order(
                b.before
                    .as_ref()
                    .or(b.after.as_ref())
                    .expect("nonempty change"),
                &identities,
            ))
        });
        evidence_changes.sort_by(|a, b| {
            let key = |change: &GraphEvidenceChange| {
                (
                    match change.dimension.as_str() {
                        "declarations" => 0,
                        "source_artifact" => 1,
                        "verification_receipts" => 2,
                        _ => 3,
                    },
                    change
                        .subject
                        .as_deref()
                        .and_then(|p| identities.get(p))
                        .copied()
                        .unwrap_or(""),
                )
            };
            key(a).cmp(&key(b))
        });
        GraphComparison {
            selected_graph_supported: true,
            unsupported_reason: None,
            scope: GraphScope::All,
            before_digest: before.snapshot_digest,
            after_digest: after.snapshot_digest,
            before_availability: before.availability,
            after_availability: after.availability,
            before_coverage: before.coverage,
            after_coverage: after.coverage,
            added_instances,
            removed_instances,
            changed_selections,
            relationship_changes,
            evidence_changes,
        }
    }

    /// Compare complete inputs, then include differences touching either side of
    /// the explicit scope. Retargets retain both endpoints even when only one
    /// endpoint is selected. Root digest evidence remains explicitly global.
    pub fn compare_scope(
        &self,
        after: &Self,
        scope: &GraphScope,
    ) -> Result<GraphComparison, SnapshotError> {
        let before_scope = self.inspect_scope(scope)?;
        let after_scope = after.inspect_scope(scope)?;
        let mut result = self.compare(after);
        let selected: std::collections::BTreeSet<_> = before_scope
            .instances
            .iter()
            .chain(after_scope.instances.iter())
            .map(|p| p.reference.as_str())
            .collect();
        result
            .added_instances
            .retain(|p| selected.contains(p.reference.as_str()));
        result
            .removed_instances
            .retain(|p| selected.contains(p.reference.as_str()));
        let touches = |edge: &GraphRelationship| {
            selected.contains(edge.target.as_str())
                || edge
                    .parent
                    .as_ref()
                    .is_some_and(|p| selected.contains(p.as_str()))
        };
        result.relationship_changes.retain(|c| {
            c.before.as_ref().is_some_and(touches) || c.after.as_ref().is_some_and(touches)
        });
        result.changed_selections = result
            .relationship_changes
            .iter()
            .filter(|c| {
                c.before
                    .as_ref()
                    .or(c.after.as_ref())
                    .is_some_and(|e| e.local_name.is_some())
            })
            .count();
        result.evidence_changes.retain(|c| {
            c.subject
                .as_ref()
                .is_none_or(|p| selected.contains(p.as_str()))
        });
        result.before_coverage = before_scope.coverage;
        result.after_coverage = after_scope.coverage;
        result.scope = before_scope.scope;
        Ok(result)
    }
}

fn evidence_changes(before: &GraphSnapshot, after: &GraphSnapshot) -> Vec<GraphEvidenceChange> {
    if let (super::SnapshotLock::Imported(left), super::SnapshotLock::Imported(right)) =
        (&before.lock, &after.lock)
    {
        return super::imported::evidence(left, right);
    }
    if (before.schema == 8) != (after.schema == 8) {
        let digest = |snapshot: &GraphSnapshot| match &snapshot.lock {
            super::SnapshotLock::Native(lock) => lock.root_manifest_digest().to_owned(),
            super::SnapshotLock::Imported(lock) => lock.root_manifest_digest().to_owned(),
        };
        let old = digest(before);
        let new = digest(after);
        return if old == new {
            Vec::new()
        } else {
            vec![GraphEvidenceChange {
                dimension: "declarations".into(),
                subject: None,
                before: [("manifest_digest".into(), old)].into(),
                after: [("manifest_digest".into(), new)].into(),
                attestation_changed: false,
            }]
        };
    }
    let mut changes = Vec::new();
    let mut record = |dimension: &str,
                      subject: Option<String>,
                      before: BTreeMap<String, String>,
                      after: BTreeMap<String, String>,
                      attestation_changed: bool| {
        if before != after || attestation_changed {
            changes.push(GraphEvidenceChange {
                dimension: dimension.into(),
                subject,
                before,
                after,
                attestation_changed,
            });
        }
    };
    record(
        "declarations",
        None,
        [(
            "manifest_digest".into(),
            before.native().root_manifest_digest().into(),
        )]
        .into(),
        [(
            "manifest_digest".into(),
            after.native().root_manifest_digest().into(),
        )]
        .into(),
        false,
    );
    for (key, package) in before.native().workspace_packages() {
        if let Some(new) = after.native().workspace_packages().get(key) {
            record(
                "declarations",
                Some(super::reference(key)),
                [("manifest_digest".into(), package.manifest_digest().into())].into(),
                [("manifest_digest".into(), new.manifest_digest().into())].into(),
                false,
            );
        }
    }
    for (key, package) in before.native().packages() {
        if let Some(new) = after.native().packages().get(key) {
            record(
                "source_artifact",
                Some(super::reference(key)),
                artifact(package),
                artifact(new),
                false,
            );
        }
    }
    for (key, package) in before.native().packages() {
        if let Some(new) = after.native().packages().get(key) {
            let attestation_changed = package
                .derived_hooks()
                .iter()
                .map(|h| (h.hook(), h.attestation()))
                .ne(new
                    .derived_hooks()
                    .iter()
                    .map(|h| (h.hook(), h.attestation())));
            record(
                "derived_outputs",
                Some(super::reference(key)),
                derived(package),
                derived(new),
                attestation_changed,
            );
        }
    }
    changes
}

fn artifact(package: &crate::LockedPackage) -> BTreeMap<String, String> {
    let mut evidence: BTreeMap<String, String> = [
        ("integrity".into(), package.artifact_integrity().into()),
        ("tree_digest".into(), package.tree_digest().into()),
        ("unpacked_digest".into(), package.unpacked_digest().into()),
    ]
    .into();
    if let Some(value) = package.artifact_url() {
        evidence.insert("url".into(), value.into());
    }
    if let Some(value) = package.registry_integrity_declared() {
        evidence.insert("registry_integrity_declared".into(), value.to_string());
    }
    evidence
}

fn derived(package: &crate::LockedPackage) -> BTreeMap<String, String> {
    let mut evidence = BTreeMap::new();
    for (index, hook) in package.derived_hooks().iter().enumerate() {
        evidence.insert(format!("{index}.hook"), hook.hook().into());
        evidence.insert(format!("{index}.recipe_key"), hook.key().into());
        evidence.insert(format!("{index}.tree_digest"), hook.tree_digest().into());
    }
    evidence
}
