//! Persisted graph observations. No resolution, store access, or host selection.

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{Lockfile, LockfilePackageKey};

mod comparison;
mod imported;
mod preflight;
pub use comparison::{GraphComparison, GraphEvidenceChange, GraphRelationshipChange};

const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_INSTANCES: usize = 100_000;
const MAX_RELATIONSHIPS: usize = 500_000;

/// Stable failures contain no untrusted lockfile metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    InputLimit,
    AnalysisLimit,
    InvalidSnapshot,
    UnsupportedFormat,
    InvalidScope,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InputLimit => "snapshot exceeds the input byte limit",
            Self::AnalysisLimit => "snapshot exceeds the graph analysis limit",
            Self::InvalidSnapshot => "snapshot failed lockfile validation",
            Self::UnsupportedFormat => "snapshot format is unsupported",
            Self::InvalidScope => "snapshot scope exceeds the filter limits",
        })
    }
}

impl std::error::Error for SnapshotError {}

/// Validated persisted snapshot, independent of manifests and installed state.
pub struct GraphSnapshot {
    lock: SnapshotLock,
    schema: u32,
    named_root_bindings: bool,
    digest: String,
}

enum SnapshotLock {
    Native(Lockfile),
    Imported(crate::ImportedNpmLockfile),
}

impl GraphSnapshot {
    fn native(&self) -> &Lockfile {
        match &self.lock {
            SnapshotLock::Native(lock) => lock,
            SnapshotLock::Imported(_) => unreachable!("native adapter only"),
        }
    }
}

impl std::fmt::Debug for GraphSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphSnapshot")
            .field("schema", &self.schema)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

/// An exact persisted instance. Display metadata is untrusted and unsanitized.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GraphInstance {
    pub reference: String,
    pub identity: String,
    pub name: String,
    pub version: String,
    /// Stable imported placement; absent for native exact-key correspondence.
    pub placement: Option<String>,
}

/// Recorded relationship; native peer-provider placements are never inferred.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize)]
pub struct GraphRelationship {
    /// Recorded or adapted edge kind, retaining imported placement/peer/optional distinctions.
    pub kind: String,
    /// None denotes the project root; otherwise an exact instance reference.
    pub parent: Option<String>,
    /// None for legacy unnamed root membership.
    pub local_name: Option<String>,
    pub target: String,
}

/// Complete inventory of a validated snapshot, including disconnected records.
#[derive(Debug, serde::Serialize)]
pub struct GraphInspection {
    pub schema: u32,
    pub snapshot_digest: String,
    pub scope: GraphScope,
    pub availability: GraphAvailability,
    pub coverage: GraphCoverage,
    /// Ordered by full internal identity, never by display text or references.
    pub instances: Vec<GraphInstance>,
    pub relationships: Vec<GraphRelationship>,
}

/// Whether a snapshot represents each dimension. False means unavailable,
/// not unchanged. Scripts and host toolchains are never reconstructed.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GraphAvailability {
    pub explicit_roots: bool,
    pub named_root_bindings: bool,
    pub exact_scripts: bool,
    pub host_toolchain: bool,
    pub derived_outputs: bool,
    pub verification_receipts: bool,
    pub declaration_requirements: bool,
    pub peer_provider_targets: bool,
}

impl GraphAvailability {
    /// Stable per-dimension reasons; absence means represented, never verified safe.
    pub fn unavailable_reasons(&self) -> std::collections::BTreeMap<&'static str, &'static str> {
        [
            (self.explicit_roots, "explicit_roots", "not_recorded"),
            (
                self.named_root_bindings,
                "named_root_bindings",
                "not_recorded",
            ),
            (self.exact_scripts, "exact_scripts", "not_recorded"),
            (self.host_toolchain, "host_toolchain", "not_recorded"),
            (
                self.derived_outputs,
                "derived_outputs",
                "format_does_not_record",
            ),
            (
                self.verification_receipts,
                "verification_receipts",
                "format_does_not_record",
            ),
            (
                self.declaration_requirements,
                "declaration_requirements",
                "digest_evidence_only",
            ),
            (
                self.peer_provider_targets,
                "peer_provider_targets",
                "context_does_not_identify_provider_placement",
            ),
        ]
        .into_iter()
        .filter(|(available, _, _)| !available)
        .map(|(_, dimension, reason)| (dimension, reason))
        .collect()
    }
}

/// Whole-input examination, separate from any later CLI detail pagination.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GraphCoverage {
    pub analysis_complete: bool,
    pub total_instances: usize,
    pub total_relationships: usize,
    pub excluded_instances: usize,
    pub excluded_relationships: usize,
}

/// Explicit selection, without dependency-closure expansion. Multiple values
/// form a union. Package names are actual names, independent of local aliases.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "kind", content = "values", rename_all = "snake_case")]
pub enum GraphScope {
    #[default]
    All,
    Packages(Vec<String>),
    Instances(Vec<String>),
}

impl GraphScope {
    fn normalized(&self) -> Result<Self, SnapshotError> {
        let (Self::Packages(values) | Self::Instances(values)) = self else {
            return Ok(Self::All);
        };
        if values.len() > 64 || values.iter().any(|v| v.len() > 4096) {
            return Err(SnapshotError::InvalidScope);
        }
        let mut values = values.clone();
        values.sort();
        values.dedup();
        Ok(match self {
            Self::Packages(_) => Self::Packages(values),
            _ => Self::Instances(values),
        })
    }

    fn includes(&self, instance: &GraphInstance) -> bool {
        match self {
            Self::All => true,
            Self::Packages(values) => values.binary_search(&instance.name).is_ok(),
            Self::Instances(values) => values.binary_search(&instance.reference).is_ok(),
        }
    }
}

impl GraphSnapshot {
    /// Parse explicit snapshot bytes without recovering or accessing project state.
    /// Rejects inputs over 32 MiB and graphs over 100,000 instances / 500,000
    /// recorded relationships. The typed parser retains its recursion limit.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SnapshotError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(SnapshotError::InputLimit);
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Format {
            lockfile_version: Option<u32>,
            root_bindings: Option<serde::de::IgnoredAny>,
        }
        let format: Format =
            serde_json::from_slice(bytes).map_err(|_| SnapshotError::InvalidSnapshot)?;
        let schema = format
            .lockfile_version
            .ok_or(SnapshotError::UnsupportedFormat)?;
        if !matches!(schema, 4..=10) {
            return Err(SnapshotError::UnsupportedFormat);
        }
        preflight::check(bytes)?;
        let input = std::str::from_utf8(bytes).map_err(|_| SnapshotError::InvalidSnapshot)?;
        if schema == 8 {
            return Self::from_imported(input, bytes);
        }
        let lock = Lockfile::from_json(input).map_err(|_| SnapshotError::InvalidSnapshot)?;
        let instances = lock.packages().len() + lock.workspace_packages().len();
        let relationships = lock.roots().len()
            + lock.root_bindings().len()
            + lock
                .packages()
                .values()
                .map(|p| p.dependencies().len())
                .sum::<usize>()
            + lock
                .workspace_packages()
                .values()
                .map(|p| p.dependencies().len())
                .sum::<usize>();
        if instances > MAX_INSTANCES || relationships > MAX_RELATIONSHIPS {
            return Err(SnapshotError::AnalysisLimit);
        }
        Ok(Self {
            lock: SnapshotLock::Native(lock),
            schema,
            named_root_bindings: format.root_bindings.is_some(),
            digest: format!("sha256-{:x}", Sha256::digest(bytes)),
        })
    }

    /// Inventory all persisted instances. This does not apply host-platform filters.
    pub fn inspect(&self) -> GraphInspection {
        if let SnapshotLock::Imported(lock) = &self.lock {
            return self.inspect_imported(lock);
        }
        let mut instances = Vec::new();
        let mut relationships = Vec::new();
        for (identity, dependencies) in self
            .native()
            .packages()
            .iter()
            .map(|(key, package)| (key, package.dependencies()))
            .chain(
                self.native()
                    .workspace_packages()
                    .iter()
                    .map(|(key, package)| (key, package.dependencies())),
            )
        {
            let key: LockfilePackageKey = identity.parse().expect("validated package identity");
            let reference = reference(identity);
            instances.push(GraphInstance {
                reference: reference.clone(),
                identity: identity.clone(),
                name: key.name.to_string(),
                version: key.version.to_string(),
                placement: None,
            });
            for (name, target) in dependencies {
                relationships.push(GraphRelationship {
                    kind: "dependency".into(),
                    parent: Some(reference.clone()),
                    local_name: Some(name.clone()),
                    target: self::reference(target),
                });
            }
        }
        instances.sort_by(|a, b| a.identity.cmp(&b.identity));
        for target in self.native().roots() {
            relationships.push(GraphRelationship {
                kind: "root".into(),
                parent: None,
                local_name: None,
                target: reference(target),
            });
        }
        for (name, target) in self.native().root_bindings() {
            relationships.push(GraphRelationship {
                kind: "root_binding".into(),
                parent: None,
                local_name: Some(name.clone()),
                target: reference(target),
            });
        }
        sort_relationships(&mut relationships, &instances);
        GraphInspection {
            schema: self.schema,
            snapshot_digest: self.digest.clone(),
            scope: GraphScope::All,
            availability: GraphAvailability {
                explicit_roots: self.schema >= 5 || !self.native().roots().is_empty(),
                named_root_bindings: self.named_root_bindings,
                exact_scripts: false,
                host_toolchain: false,
                derived_outputs: self.schema >= 9,
                verification_receipts: false,
                declaration_requirements: false,
                peer_provider_targets: false,
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

    /// Examine the complete validated input, then select instances and incident
    /// relationships. Coverage never establishes equality outside this scope.
    pub fn inspect_scope(&self, scope: &GraphScope) -> Result<GraphInspection, SnapshotError> {
        let scope = scope.normalized()?;
        let mut result = self.inspect();
        result.instances.retain(|instance| scope.includes(instance));
        let selected: std::collections::BTreeSet<_> = result
            .instances
            .iter()
            .map(|p| p.reference.as_str())
            .collect();
        result.relationships.retain(|edge| {
            selected.contains(edge.target.as_str())
                || edge
                    .parent
                    .as_ref()
                    .is_some_and(|p| selected.contains(p.as_str()))
        });
        result.coverage.excluded_instances =
            result.coverage.total_instances - result.instances.len();
        result.coverage.excluded_relationships =
            result.coverage.total_relationships - result.relationships.len();
        result.scope = scope;
        Ok(result)
    }
}

fn sort_relationships(relationships: &mut [GraphRelationship], instances: &[GraphInstance]) {
    let identities: std::collections::BTreeMap<_, _> = instances
        .iter()
        .map(|p| (p.reference.as_str(), p.identity.as_str()))
        .collect();
    relationships.sort_by(|a, b| {
        relationship_order(a, &identities).cmp(&relationship_order(b, &identities))
    });
}

fn relationship_order<'a>(
    edge: &'a GraphRelationship,
    identities: &std::collections::BTreeMap<&str, &'a str>,
) -> (u8, Option<&'a str>, Option<&'a str>, &'a str) {
    let category = match edge.kind.as_str() {
        "root" => 0,
        "root_binding" => 1,
        _ => 2,
    };
    (
        category,
        edge.parent.as_ref().map(|p| identities[p.as_str()]),
        edge.local_name.as_deref(),
        identities[edge.target.as_str()],
    )
}

fn reference(identity: &str) -> String {
    reference_for("native", identity)
}

fn reference_for(format: &str, identity: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"tapid-lock-graph-instance-v1\0");
    digest.update(format.as_bytes());
    digest.update(b"\0");
    digest.update(identity.as_bytes());
    format!("instance-v1-{:x}", digest.finalize())
}
