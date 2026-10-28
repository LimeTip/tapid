use serde::Serialize;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use crate::{Lockfile, LockfileError, LockfilePackageKey, LockfilePackageSource};

/// A recorded dependency edge or a candidate peer-provider explanation edge.
#[derive(Clone, Debug)]
pub struct LockedGraphEdge {
    pub name: String,
    pub target: String,
    /// `dependency` or `peer`; native locks do not record dependency section kinds.
    pub kind: &'static str,
}

/// All recorded native nodes, with source-qualified identities and separate
/// entry points. This index does not select reachable nodes or establish audit
/// coverage. Peer edges retain all matching contexts and must not be treated as
/// proof of a unique provider for security applicability.
#[derive(Debug)]
pub struct LockedGraph {
    nodes: BTreeMap<String, LockfilePackageKey>,
    edges: BTreeMap<String, Vec<LockedGraphEdge>>,
    roots: Vec<(String, String)>,
}

/// Reachable native keys or imported placements and every incoming spelling.
/// Native candidate peer providers remain reachable, but non-unique applicability
/// is explicit. Imported peers already target validated exact placements.
#[derive(Debug)]
pub struct ReachableLockedGraph {
    pub occurrences: BTreeMap<String, BTreeSet<String>>,
    pub roots_recorded: bool,
    /// `(consumer key, peer name)` with zero or multiple recorded candidates.
    pub unresolved_peers: BTreeSet<(String, String)>,
}

impl LockedGraph {
    /// Traverse the recorded graph once per identity, without explanation limits
    /// or host filtering. Aliases encountered after a cycle are retained too.
    pub fn reachable(&self) -> ReachableLockedGraph {
        let mut result = ReachableLockedGraph {
            occurrences: BTreeMap::new(),
            roots_recorded: !self.roots.is_empty(),
            unresolved_peers: BTreeSet::new(),
        };
        let mut pending = self.roots.clone();
        while let Some((name, key)) = pending.pop() {
            let names = result.occurrences.entry(key.clone()).or_default();
            let first = names.is_empty();
            names.insert(name);
            if !first {
                continue;
            }
            let edges = &self.edges[&key];
            // Keys have already passed the owning reader's context validation.
            for peer in self.nodes[&key]
                .peer_context()
                .expect("validated graph context")
                .entries()
                .keys()
            {
                if edges
                    .iter()
                    .filter(|edge| edge.kind == "peer" && edge.name == peer.as_str())
                    .count()
                    != 1
                {
                    result
                        .unresolved_peers
                        .insert((key.clone(), peer.to_string()));
                }
            }
            pending.extend(
                edges
                    .iter()
                    .map(|edge| (edge.name.clone(), edge.target.clone())),
            );
        }
        result
    }

    pub fn nodes(&self) -> &BTreeMap<String, LockfilePackageKey> {
        &self.nodes
    }

    pub fn edges(&self) -> &BTreeMap<String, Vec<LockedGraphEdge>> {
        &self.edges
    }

    /// Sorted `(incoming name, exact key)` entry points. Unbound roots retain
    /// their actual names; distinct roots and multiple aliases are not collapsed.
    /// Legacy rootless locks return no roots; callers must report incomplete
    /// coverage rather than infer public identity.
    pub fn roots(&self) -> &[(String, String)] {
        &self.roots
    }
}

impl Lockfile {
    /// Project the persisted graph without resolving, filtering by host, or
    /// touching artifacts. Revalidate public-field/deserialized mutations using
    /// the owning reader before indexing exact keys.
    pub fn graph(&self) -> Result<LockedGraph, LockfileError> {
        let encoded = serde_json::to_string(self).map_err(LockfileError::Serialization)?;
        let lock = Self::from_json(&encoded)?;
        let mut nodes = BTreeMap::new();
        let mut edges = BTreeMap::new();
        for (key, dependencies) in lock
            .packages_typed()?
            .into_iter()
            .map(|(key, package)| (key, package.dependencies()))
            .chain(
                lock.workspace_packages_typed()?
                    .into_iter()
                    .map(|(key, package)| (key, package.dependencies())),
            )
        {
            edges.insert(
                key.to_string(),
                dependencies
                    .iter()
                    .map(|(name, target)| LockedGraphEdge {
                        name: name.clone(),
                        target: target.clone(),
                        kind: "dependency",
                    })
                    .collect::<Vec<_>>(),
            );
            nodes.insert(key.to_string(), key);
        }
        let mut providers = BTreeMap::<_, Vec<&LockfilePackageKey>>::new();
        for key in nodes.values() {
            providers
                .entry((&key.source, &key.name, &key.version))
                .or_default()
                .push(key);
        }
        for (encoded, source) in &nodes {
            for (name, version) in source.peer_context()?.entries() {
                for target in providers
                    .get(&(&source.source, name, version))
                    .into_iter()
                    .flatten()
                {
                    edges
                        .get_mut(encoded)
                        .expect("indexed graph node")
                        .push(LockedGraphEdge {
                            name: name.to_string(),
                            target: target.to_string(),
                            kind: "peer",
                        });
                }
            }
        }
        let mut roots = lock
            .root_bindings()
            .iter()
            .map(|(name, key)| (name.clone(), key.clone()))
            .collect::<Vec<_>>();
        let bound = roots
            .iter()
            .map(|(_, key)| key.as_str())
            .collect::<BTreeSet<_>>();
        let unbound = lock
            .roots()
            .iter()
            .filter(|key| !bound.contains(key.as_str()))
            .map(|key| (nodes[key].name.to_string(), key.clone()))
            .collect::<Vec<_>>();
        roots.extend(unbound);
        roots.sort();
        Ok(LockedGraph {
            nodes,
            edges,
            roots,
        })
    }
}

const MAX_PATHS: usize = 50;
const MAX_DEPTH: usize = 128;
const MAX_VISITS: usize = 10_000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockedGraphPath {
    pub steps: Vec<LockedGraphStep>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockedGraphStep {
    pub key: String,
    pub via: Option<String>,
    pub edge_kind: Option<String>,
    pub source_kind: &'static str,
}

#[derive(Clone)]
pub struct LockedGraphRoot {
    pub key: String,
    pub name: String,
    pub kind: String,
}

#[derive(Default)]
struct SearchState {
    paths: Vec<LockedGraphPath>,
    truncated: bool,
    visits: usize,
}

impl LockedGraph {
    /// Cycle-safe explanation paths, bounded at 50 paths, 128 levels and
    /// 10,000 visits. Candidate peer edges do not establish security identity.
    pub fn paths_to(
        &self,
        roots: &[LockedGraphRoot],
        requested: &tapid_core::PackageName,
    ) -> LockedGraphPaths {
        paths_to(self, roots, PathTarget::Name(requested.as_str()))
    }

    /// Explain one exact source/context-qualified key. Other occurrences with
    /// the same name do not consume the displayed-path limit.
    pub fn paths_to_key(&self, roots: &[LockedGraphRoot], key: &str) -> LockedGraphPaths {
        paths_to(self, roots, PathTarget::Key(key))
    }
}

impl crate::ImportedNpmGraph {
    /// Bounded explanation paths over recorded placements, including peers and
    /// optional/platform branches. Keys in roots and results are npm paths;
    /// identities and constraints remain available in the imported graph.
    pub fn paths_to(
        &self,
        roots: &[LockedGraphRoot],
        requested: &tapid_core::PackageName,
    ) -> LockedGraphPaths {
        paths_to(
            &crate::npm_import::ImportedPathGraph::new(self),
            roots,
            PathTarget::Name(requested.as_str()),
        )
    }

    /// Explain one exact imported placement without collapsing equal names or
    /// peer contexts. Uses the same bounds as native graph traversal.
    pub fn paths_to_key(&self, roots: &[LockedGraphRoot], key: &str) -> LockedGraphPaths {
        paths_to(
            &crate::npm_import::ImportedPathGraph::new(self),
            roots,
            PathTarget::Key(key),
        )
    }
}

pub(crate) trait PathGraph {
    fn node(&self, key: &str) -> Option<(&str, &'static str)>;
    fn outgoing(&self, key: &str) -> Cow<'_, [LockedGraphEdge]>;
}

impl PathGraph for LockedGraph {
    fn node(&self, key: &str) -> Option<(&str, &'static str)> {
        self.nodes
            .get(key)
            .map(|node| (node.name.as_str(), source_kind(node)))
    }

    fn outgoing(&self, key: &str) -> Cow<'_, [LockedGraphEdge]> {
        Cow::Borrowed(self.edges.get(key).map(Vec::as_slice).unwrap_or_default())
    }
}

#[derive(Clone, Copy)]
enum PathTarget<'a> {
    Name(&'a str),
    Key(&'a str),
}

fn paths_to(
    graph: &impl PathGraph,
    roots: &[LockedGraphRoot],
    requested: PathTarget<'_>,
) -> LockedGraphPaths {
    let mut state = SearchState::default();
    for root in roots {
        let Some((_, source_kind)) = graph.node(&root.key) else {
            continue;
        };
        let mut path = vec![LockedGraphStep {
            key: root.key.clone(),
            via: Some(root.name.clone()),
            edge_kind: Some(root.kind.clone()),
            source_kind,
        }];
        search_paths(
            &root.key,
            requested,
            graph,
            &mut BTreeSet::from([root.key.clone()]),
            &mut path,
            &mut state,
            0,
        );
        if state.truncated {
            break;
        }
    }
    LockedGraphPaths {
        paths: state.paths,
        truncated: state.truncated,
    }
}

/// Display completeness is independent of graph/evidence completeness.
#[derive(Debug)]
pub struct LockedGraphPaths {
    pub paths: Vec<LockedGraphPath>,
    pub truncated: bool,
}

fn source_kind(key: &LockfilePackageKey) -> &'static str {
    match &key.source {
        LockfilePackageSource::Workspace(_) => "workspace",
        LockfilePackageSource::Copied(_) => "copied",
        _ => "registry",
    }
}

#[allow(clippy::too_many_arguments)]
fn search_paths(
    current: &str,
    requested: PathTarget<'_>,
    graph: &impl PathGraph,
    visited: &mut BTreeSet<String>,
    path: &mut Vec<LockedGraphStep>,
    state: &mut SearchState,
    depth: usize,
) {
    if state.visits >= MAX_VISITS {
        state.truncated = true;
        return;
    }
    state.visits += 1;
    let Some((name, _)) = graph.node(current) else {
        return;
    };
    if match requested {
        PathTarget::Name(requested) => name == requested,
        PathTarget::Key(requested) => current == requested,
    } {
        if state.paths.len() == MAX_PATHS {
            state.truncated = true;
        } else {
            state.paths.push(LockedGraphPath {
                steps: path.clone(),
            });
        }
    }
    if depth >= MAX_DEPTH {
        if !graph.outgoing(current).is_empty() {
            state.truncated = true;
        }
        return;
    }
    for edge in graph.outgoing(current).iter() {
        if state.truncated {
            return;
        }
        if !visited.insert(edge.target.clone()) {
            continue;
        }
        let Some((_, source_kind)) = graph.node(&edge.target) else {
            visited.remove(&edge.target);
            continue;
        };
        path.push(LockedGraphStep {
            key: edge.target.clone(),
            via: Some(edge.name.clone()),
            edge_kind: Some(edge.kind.to_owned()),
            source_kind,
        });
        search_paths(
            &edge.target,
            requested,
            graph,
            visited,
            path,
            state,
            depth + 1,
        );
        path.pop();
        visited.remove(&edge.target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package_key(name: &str, version: &str) -> String {
        crate::LockedPackage::new_with_provenance(
            "https://registry.npmjs.org",
            name,
            version,
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "a".repeat(64)),
            crate::RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap()
        .key()
    }

    #[test]
    fn traversal_is_cycle_safe_and_reports_duplicate_scoped_versions() {
        let root = package_key("root", "1.0.0");
        let middle = package_key("middle", "1.0.0");
        let target_one = package_key("@scope/target", "1.0.0");
        let target_two = package_key("@scope/target", "2.0.0");
        let graph = BTreeMap::from([
            (
                root.clone(),
                vec![
                    LockedGraphEdge {
                        name: "middle".into(),
                        target: middle.clone(),
                        kind: "dependency",
                    },
                    LockedGraphEdge {
                        name: "target-v2".into(),
                        target: target_two.clone(),
                        kind: "dependency",
                    },
                ],
            ),
            (
                middle.clone(),
                vec![
                    LockedGraphEdge {
                        name: "root".into(),
                        target: root.clone(),
                        kind: "dependency",
                    },
                    LockedGraphEdge {
                        name: "target-v1".into(),
                        target: target_one.clone(),
                        kind: "dependency",
                    },
                ],
            ),
            (
                target_one.clone(),
                vec![LockedGraphEdge {
                    name: "target-v2".into(),
                    target: target_two.clone(),
                    kind: "dependency",
                }],
            ),
            (target_two.clone(), Vec::new()),
        ]);
        let graph = LockedGraph {
            nodes: graph
                .keys()
                .map(|key| (key.clone(), key.parse().unwrap()))
                .collect(),
            edges: graph,
            roots: Vec::new(),
        };
        let requested: tapid_core::PackageName = "@scope/target".parse().unwrap();
        let mut state = SearchState::default();
        let mut path = vec![LockedGraphStep {
            key: root.clone(),
            via: Some("root".into()),
            edge_kind: Some("dependencies".into()),
            source_kind: "registry",
        }];
        search_paths(
            &root,
            PathTarget::Name(requested.as_str()),
            &graph,
            &mut BTreeSet::from([root.clone()]),
            &mut path,
            &mut state,
            0,
        );
        let final_keys = state
            .paths
            .iter()
            .map(|path| path.steps.last().unwrap().key.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            final_keys,
            BTreeSet::from([target_one.as_str(), target_two.as_str()])
        );
        assert_eq!(state.paths.len(), 3);
        assert!(state.paths.iter().any(|path| {
            path.steps.last().is_some_and(|step| step.key == target_two) && path.steps.len() == 4
        }));
        assert!(!state.truncated);
    }

    #[test]
    fn traversal_caps_path_results_and_marks_the_report_truncated() {
        let root = package_key("root", "1.0.0");
        let mut graph: BTreeMap<String, Vec<LockedGraphEdge>> = BTreeMap::new();
        let mut edges = Vec::new();
        for version in 0..=MAX_PATHS {
            let target = package_key("target", &format!("1.0.{version}"));
            edges.push(LockedGraphEdge {
                name: format!("target-{version}"),
                target: target.clone(),
                kind: "dependency",
            });
            graph.insert(target, Vec::new());
        }
        graph.insert(root.clone(), edges);
        let graph = LockedGraph {
            nodes: graph
                .keys()
                .map(|key| (key.clone(), key.parse().unwrap()))
                .collect(),
            edges: graph,
            roots: Vec::new(),
        };
        let requested: tapid_core::PackageName = "target".parse().unwrap();
        let mut state = SearchState::default();
        search_paths(
            &root,
            PathTarget::Name(requested.as_str()),
            &graph,
            &mut BTreeSet::from([root.clone()]),
            &mut vec![LockedGraphStep {
                key: root.clone(),
                via: Some("root".into()),
                edge_kind: Some("dependencies".into()),
                source_kind: "registry",
            }],
            &mut state,
            0,
        );
        assert_eq!(state.paths.len(), MAX_PATHS);
        assert!(state.truncated);
    }
}
