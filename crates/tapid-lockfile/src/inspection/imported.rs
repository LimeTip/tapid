//! Adapter for the existing validated all-placement npm graph.
use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::{
    GraphAvailability, GraphCoverage, GraphEvidenceChange, GraphInspection, GraphInstance,
    GraphRelationship, GraphScope, GraphSnapshot, MAX_INSTANCES, MAX_RELATIONSHIPS, SnapshotError,
    SnapshotLock, sort_relationships,
};

fn reference(identity: &str) -> String {
    super::reference_for("imported", identity)
}
use crate::{ImportedNpmLockfile, ImportedNpmPackage};

fn identity(package: &ImportedNpmPackage, graph: &crate::ImportedNpmGraph) -> String {
    // A versioned tuple avoids delimiter ambiguity. Artifact receipts, integrity,
    // and download URLs are evidence, not selection identity.
    serde_json::to_string(&(
        "imported-v1",
        &package.path,
        package.name.to_string(),
        package.version.to_string(),
        package.registry.to_string(),
        (
            &package.peers,
            graph.peer_context(&package.path).to_string(),
        ),
        package.os.iter().collect::<std::collections::BTreeSet<_>>(),
        package
            .cpu
            .iter()
            .collect::<std::collections::BTreeSet<_>>(),
        package
            .libc
            .iter()
            .collect::<std::collections::BTreeSet<_>>(),
        package.optional,
        &package.optional_peers,
    ))
    .expect("string tuple")
}

fn workspace_identity(path: &str, fields: &serde_json::Value) -> String {
    serde_json::to_string(&(
        "imported-workspace-v1",
        path,
        &fields["name"],
        &fields["version"],
    ))
    .expect("workspace tuple")
}

impl GraphSnapshot {
    pub(super) fn from_imported(input: &str, bytes: &[u8]) -> Result<Self, SnapshotError> {
        let lock =
            ImportedNpmLockfile::from_json(input).map_err(|_| SnapshotError::InvalidSnapshot)?;
        let graph = lock.graph().map_err(|_| SnapshotError::InvalidSnapshot)?;
        let instances = graph.packages.len() + graph.workspaces.len();
        let relationships = instances
            + graph.roots.len()
            + graph.workspace_links.len()
            + graph
                .member_roots
                .values()
                .map(|r| r.dependencies.len())
                .sum::<usize>()
            + graph
                .packages
                .values()
                .map(|p| p.dependencies.len() + p.peers.len())
                .sum::<usize>();
        if instances > MAX_INSTANCES || relationships > MAX_RELATIONSHIPS {
            return Err(SnapshotError::AnalysisLimit);
        }
        let snapshot = Self {
            lock: SnapshotLock::Imported(lock),
            schema: 8,
            named_root_bindings: true,
            digest: format!("sha256-{:x}", Sha256::digest(bytes)),
        };
        Ok(snapshot)
    }

    pub(super) fn inspect_imported(&self, lock: &ImportedNpmLockfile) -> GraphInspection {
        let graph = lock.graph().expect("validated imported graph");
        let mut instances: Vec<_> = graph
            .packages
            .values()
            .map(|p| {
                let identity = identity(p, &graph);
                GraphInstance {
                    reference: reference(&identity),
                    identity,
                    name: p.name.to_string(),
                    version: p.version.to_string(),
                    placement: Some(p.path.clone()),
                }
            })
            .collect();
        for (path, fields) in &graph.workspaces {
            let identity = workspace_identity(path, fields);
            instances.push(GraphInstance {
                reference: reference(&identity),
                identity,
                name: fields["name"].as_str().unwrap_or_default().into(),
                version: fields["version"].as_str().unwrap_or_default().into(),
                placement: Some(path.clone()),
            });
        }
        instances.sort_by(|a, b| a.identity.cmp(&b.identity));
        let refs: BTreeMap<_, _> = instances
            .iter()
            .map(|p| {
                (
                    p.placement.as_deref().expect("imported placement"),
                    p.reference.clone(),
                )
            })
            .collect();
        let mut relationships = Vec::new();
        let mut edge = |kind: &str, parent: Option<&str>, name: &str, target: &str| {
            relationships.push(GraphRelationship {
                kind: kind.into(),
                parent: parent.map(|p| refs[p].clone()),
                local_name: Some(name.into()),
                target: refs[target].clone(),
            });
        };
        for path in refs.keys() {
            edge("placement", None, path, path);
        }
        for (name, target) in &graph.roots {
            edge(
                if graph.optional_roots.contains(name) {
                    "optional_imported_entry"
                } else {
                    "imported_entry"
                },
                None,
                name,
                target,
            );
        }
        for (path, roots) in &graph.member_roots {
            for (name, target) in &roots.dependencies {
                edge(
                    if roots.optional_dependencies.contains(name) {
                        "optional_dependency"
                    } else {
                        "dependency"
                    },
                    Some(path),
                    name,
                    target,
                );
            }
        }
        for (path, target) in &graph.workspace_links {
            edge("workspace_link", None, path, target);
        }
        for (path, p) in &graph.packages {
            for (name, target) in &p.dependencies {
                edge(
                    if p.optional_dependencies.contains(name) {
                        "optional_dependency"
                    } else {
                        "dependency"
                    },
                    Some(path),
                    name,
                    target,
                );
            }
            for (name, target) in &p.peers {
                edge(
                    if p.optional_peers.contains(name) {
                        "optional_peer"
                    } else {
                        "peer"
                    },
                    Some(path),
                    name,
                    target,
                );
            }
        }
        sort_relationships(&mut relationships, &instances);
        GraphInspection {
            schema: 8,
            snapshot_digest: self.digest.clone(),
            scope: GraphScope::All,
            availability: GraphAvailability {
                explicit_roots: true,
                named_root_bindings: true,
                exact_scripts: false,
                host_toolchain: false,
                derived_outputs: false,
                verification_receipts: true,
                declaration_requirements: true,
                peer_provider_targets: true,
            },
            coverage: GraphCoverage {
                analysis_complete: true,
                total_instances: instances.len(),
                total_relationships: relationships.len(),
                excluded_instances: 0,
                excluded_relationships: 0,
            },
            instances,
            relationships,
        }
    }
}

pub(super) fn evidence(
    before: &ImportedNpmLockfile,
    after: &ImportedNpmLockfile,
) -> Vec<GraphEvidenceChange> {
    let mut changes = Vec::new();
    let mut record = |dimension: &str,
                      subject: Option<String>,
                      before: BTreeMap<String, String>,
                      after: BTreeMap<String, String>| {
        if before != after {
            changes.push(GraphEvidenceChange {
                dimension: dimension.into(),
                subject,
                before,
                after,
                attestation_changed: false,
            });
        }
    };
    let root = |lock: &ImportedNpmLockfile| {
        let mut fields = lock.declaration_fields("");
        fields.insert("manifest_digest".into(), lock.root_manifest_digest().into());
        fields
    };
    record("declarations", None, root(before), root(after));
    let old = before.graph().expect("validated graph");
    let new = after.graph().expect("validated graph");
    for (path, fields) in &old.workspaces {
        let Some(new_fields) = new.workspaces.get(path) else {
            continue;
        };
        if workspace_identity(path, fields) != workspace_identity(path, new_fields) {
            continue;
        }
        let declaration = |lock: &ImportedNpmLockfile| {
            let mut fields = lock.declaration_fields(path);
            if let Some(digest) = lock.workspace_manifest_digests().get(path) {
                fields.insert("manifest_digest".into(), digest.clone());
            }
            fields
        };
        record(
            "declarations",
            Some(reference(&workspace_identity(path, fields))),
            declaration(before),
            declaration(after),
        );
    }
    for (path, package) in &old.packages {
        let Some(other) = new
            .packages
            .get(path)
            .filter(|p| identity(p, &new) == identity(package, &old))
        else {
            continue;
        };
        let subject = Some(reference(&identity(package, &old)));
        record(
            "declarations",
            subject.clone(),
            before.declaration_fields(path),
            after.declaration_fields(path),
        );
        let artifact = |p: &ImportedNpmPackage| {
            [
                ("url".into(), p.resolved.clone()),
                ("integrity".into(), p.integrity.to_string()),
            ]
            .into()
        };
        record(
            "source_artifact",
            subject.clone(),
            artifact(package),
            artifact(other),
        );
        let receipt = |lock: &ImportedNpmLockfile| {
            let mut fields = BTreeMap::new();
            if let Some(tree) = lock.verified_tree(path) {
                fields.insert("tree_digest".into(), tree.into());
            }
            if let Some(receipt) = lock.verified_artifact(path) {
                fields.insert("verified_url".into(), receipt.resolved.clone());
                fields.insert("verified_integrity".into(), receipt.integrity.clone());
            }
            fields
        };
        record(
            "verification_receipts",
            subject,
            receipt(before),
            receipt(after),
        );
    }
    changes
}
