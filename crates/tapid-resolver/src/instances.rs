//! Placement identity is separate from reusable artifact metadata. This graph
//! must not be flattened into `Resolution` for lockfile or layout construction.
use super::*;

/// Deterministic placement reference, independent of artifact/provider versions.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct InstanceRef(String);
impl InstanceRef {
    /// Canonical length-delimited placement identity. This is not a file path.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One selected placement with separate ordinary and exact peer-provider edges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedInstance {
    pub artifact: RegistryPackageId,
    pub dependencies: BTreeMap<PackageName, InstanceRef>,
    pub peers: BTreeMap<PackageName, InstanceRef>,
}
/// Selected contextual graph. Metadata is caller-filtered; this interface does
/// not establish platform eligibility or workspace visibility on its own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceResolution {
    pub roots: BTreeMap<(PackageSource, PackageName), InstanceRef>,
    pub instances: BTreeMap<InstanceRef, ResolvedInstance>,
}

/// Exact context retained for an absent or invalid nearest provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerBindingFailure {
    pub consumer: InstanceRef,
    pub package: RegistryPackageId,
    pub peer: PackageName,
    pub requirement: String,
    pub expected_source: PackageSource,
    pub provider: Option<RegistryPackageId>,
}

impl fmt::Display for PeerBindingFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use std::fmt::Write;
        const SUFFIX: &str = " [truncated]";
        struct Message {
            text: String,
            truncated: bool,
        }
        impl fmt::Write for Message {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                let remaining = (8192 - SUFFIX.len()).saturating_sub(self.text.len());
                let mut end = text.len().min(remaining);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                self.text.push_str(&text[..end]);
                self.truncated |= end != text.len();
                Ok(())
            }
        }
        let mut message = Message {
            text: String::new(),
            truncated: false,
        };
        write!(
            message,
            "peer dependency unresolved in {} ({}) requires {}@{} from {}; nearest enclosing provider: ",
            self.consumer.as_str(),
            self.package,
            self.peer,
            self.requirement,
            self.expected_source
        )?;
        match &self.provider {
            Some(provider) => write!(message, "{provider}")?,
            None => message.write_str("absent")?,
        }
        message.write_str("; correct the nearest enclosing dependency declaration or declare a compatible provider in an enclosing scope")?;
        if message.truncated {
            message.text.push_str(SUFFIX);
        }
        f.write_str(&message.text)
    }
}
/// Selects ordinary candidates using the existing version/routing policy, then
/// binds peers by local name in the nearest enclosing scope. Never installs or
/// hoists a peer. The legacy artifact-keyed resolver remains a separate contract
/// until consumers can persist and materialize exact references.
pub fn resolve_instances_with_preferences<F>(
    dependencies: &[Dependency],
    metadata: &[RegistryMetadata],
    options: ResolutionOptions,
    mut registry_for_dependency: F,
    preferred: &ResolutionPreferences,
) -> Result<InstanceResolution, ResolveError>
where
    F: FnMut(&PackageSource, &PackageName) -> Result<PackageSource, String>,
{
    let selected = select_graph(
        dependencies,
        metadata,
        options,
        &mut registry_for_dependency,
        preferred,
    )?;
    expand(selected, &mut registry_for_dependency, Limits::default())
}
#[derive(Clone, Copy)]
struct Limits {
    instances: usize,
    edges: usize,
    depth: usize,
    bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            instances: 100_000,
            edges: 1_000_000,
            depth: 256,
            bytes: 64 * 1024 * 1024,
        }
    }
}
struct Scope {
    outer: Option<usize>,
    bindings: BTreeMap<PackageName, Vec<InstanceRef>>,
}
struct Expansion {
    packages: BTreeMap<RegistryPackageId, PackageVersionMetadata>,
    dependencies: BTreeMap<RegistryPackageId, BTreeMap<PackageName, RegistryPackageId>>,
    recurrences: Vec<(InstanceRef, usize)>,
    graph: InstanceResolution,
    scopes: Vec<Scope>,
    enclosing: BTreeMap<InstanceRef, usize>,
    limits: Limits,
    edges: usize,
    bytes: usize,
}
impl Expansion {
    fn charge(&mut self, bytes: usize, edges: usize) -> Result<(), ResolveError> {
        self.bytes = self.bytes.saturating_add(bytes);
        self.edges = self.edges.saturating_add(edges);
        if self.bytes > self.limits.bytes || self.edges > self.limits.edges {
            return Err(ResolveError::UnsupportedMode(
                "contextual graph exceeds text or edge limit",
            ));
        }
        Ok(())
    }
    fn allocate(
        &mut self,
        reference: InstanceRef,
        artifact: RegistryPackageId,
        enclosing: usize,
    ) -> Result<(), ResolveError> {
        if self.graph.instances.len() >= self.limits.instances {
            return Err(ResolveError::UnsupportedMode(
                "contextual graph exceeds instance limit",
            ));
        }
        self.charge(
            reference
                .0
                .len()
                .saturating_mul(2)
                .saturating_add(artifact.to_string().len()),
            0,
        )?;
        self.enclosing.insert(reference.clone(), enclosing);
        self.graph.instances.insert(
            reference,
            ResolvedInstance {
                artifact,
                dependencies: BTreeMap::new(),
                peers: BTreeMap::new(),
            },
        );
        Ok(())
    }
    fn lookup(&self, enclosing: usize, name: &PackageName) -> Option<&Vec<InstanceRef>> {
        let mut scope = Some(enclosing);
        while let Some(index) = scope {
            if let Some(bindings) = self.scopes[index].bindings.get(name) {
                return Some(bindings);
            }
            scope = self.scopes[index].outer;
        }
        None
    }

    // Reuse a recurring subtree only when every reachable peer sees exactly the
    // same references. Compare references, never provider versions: providers can
    // themselves have distinct contexts. This deliberately rejects environments
    // that differ even for a name shadowed inside the subtree.
    fn validate_recurrences(&self) -> Result<(), ResolveError> {
        let mut work = 0usize;
        let mut step = || {
            work = work.saturating_add(1);
            if work > self.limits.edges {
                Err(ResolveError::UnsupportedMode(
                    "contextual cycle analysis exceeds work limit",
                ))
            } else {
                Ok(())
            }
        };
        for (reference, scope) in &self.recurrences {
            let previous = self.enclosing[reference];
            let mut seen = BTreeSet::new();
            let mut queue = vec![&self.graph.instances[reference].artifact];
            while let Some(artifact) = queue.pop() {
                if !seen.insert(artifact) {
                    continue;
                }
                step()?;
                for peer in self.packages[artifact].peer_dependencies.keys() {
                    step()?;
                    if self.lookup(previous, peer) != self.lookup(*scope, peer) {
                        return Err(ResolveError::UnsupportedMode(
                            "dependency cycle changes exact peer-provider environment; remove the cycle or align its enclosing provider declarations",
                        ));
                    }
                }
                for child in self
                    .dependencies
                    .get(artifact)
                    .into_iter()
                    .flat_map(|edges| edges.values())
                {
                    step()?;
                    queue.push(child);
                }
            }
        }
        Ok(())
    }
    fn children(
        &mut self,
        parent: &InstanceRef,
        active: &mut Vec<InstanceRef>,
    ) -> Result<(), ResolveError> {
        if active.len() >= self.limits.depth {
            return Err(ResolveError::UnsupportedMode(
                "contextual graph exceeds scope depth limit",
            ));
        }
        active.push(parent.clone());
        let artifact = self.graph.instances[parent].artifact.clone();
        let children = self
            .dependencies
            .get(&artifact)
            .cloned()
            .unwrap_or_default();
        let scope = self.scopes.len();
        self.scopes.push(Scope {
            outer: Some(self.enclosing[parent]),
            bindings: BTreeMap::new(),
        });
        let mut pending = Vec::new();
        // Allocate every ordinary sibling before any descendant binds peers.
        for (local, child) in children {
            let recurrence = active
                .iter()
                .rposition(|reference| self.graph.instances[reference].artifact == child);
            let reference = if let Some(index) = recurrence {
                self.recurrences.push((active[index].clone(), scope));
                active[index].clone()
            } else {
                let reference = InstanceRef(format!(
                    "{}{len}:{local}",
                    parent.0,
                    len = local.as_str().len()
                ));
                self.allocate(reference.clone(), child, scope)?;
                pending.push(reference.clone());
                reference
            };
            self.charge(
                local
                    .as_str()
                    .len()
                    .saturating_add(reference.0.len())
                    .saturating_mul(2),
                1,
            )?;
            self.scopes[scope]
                .bindings
                .insert(local.clone(), vec![reference.clone()]);
            self.graph
                .instances
                .get_mut(parent)
                .expect("allocated parent")
                .dependencies
                .insert(local, reference);
        }
        for reference in pending {
            self.children(&reference, active)?;
        }
        active.pop();
        Ok(())
    }
}
fn expand<F>(
    selected: ArtifactSelections,
    route: &mut F,
    limits: Limits,
) -> Result<InstanceResolution, ResolveError>
where
    F: FnMut(&PackageSource, &PackageName) -> Result<PackageSource, String>,
{
    let mut dependencies: BTreeMap<_, BTreeMap<_, _>> = BTreeMap::new();
    for edge in selected.dependencies {
        dependencies
            .entry(edge.parent)
            .or_default()
            .insert(edge.dependency, edge.child);
    }
    let mut expansion = Expansion {
        packages: selected.selected_packages,
        dependencies,
        recurrences: Vec::new(),
        graph: InstanceResolution {
            roots: BTreeMap::new(),
            instances: BTreeMap::new(),
        },
        scopes: vec![Scope {
            outer: None,
            bindings: BTreeMap::new(),
        }],
        enclosing: BTreeMap::new(),
        limits,
        edges: 0,
        bytes: 0,
    };
    for ((source, local), artifact) in selected.root_bindings {
        let reference = InstanceRef(format!(
            "{len}:{source}{name_len}:{local}",
            len = source.as_str().len(),
            name_len = local.as_str().len()
        ));
        expansion.allocate(reference.clone(), artifact, 0)?;
        expansion.charge(
            source
                .as_str()
                .len()
                .saturating_add(local.as_str().len())
                .saturating_add(reference.0.len())
                .saturating_mul(2),
            1,
        )?;
        expansion.scopes[0]
            .bindings
            .entry(local.clone())
            .or_default()
            .push(reference.clone());
        expansion.graph.roots.insert((source, local), reference);
    }
    for root in expansion.graph.roots.values().cloned().collect::<Vec<_>>() {
        expansion.children(&root, &mut Vec::new())?;
    }
    expansion.validate_recurrences()?;
    for reference in expansion
        .graph
        .instances
        .keys()
        .cloned()
        .collect::<Vec<_>>()
    {
        let artifact = expansion.graph.instances[&reference].artifact.clone();
        let package = expansion.packages[&artifact].clone();
        for (peer, requirement) in &package.peer_dependencies {
            let visible = expansion.lookup(expansion.enclosing[&reference], peer);
            let mut provider = None;
            let mut expected_source = None;
            for candidate in visible.into_iter().flatten() {
                let candidate_artifact = &expansion.graph.instances[candidate].artifact;
                let actual_name = if requirement.is_alias() {
                    requirement.package_name(peer)
                } else {
                    &candidate_artifact.name
                };
                let source = route(&artifact.registry, actual_name)
                    .map_err(ResolveError::RegistryRouting)?;
                expected_source = Some(source.clone());
                if candidate_artifact.registry == source
                    && provider.replace(candidate.clone()).is_some()
                {
                    return Err(ResolveError::RegistryRouting(format!(
                        "ambiguous nearest peer binding for {peer}"
                    )));
                }
            }
            if visible.is_none() {
                expected_source = Some(
                    route(&artifact.registry, requirement.package_name(peer))
                        .map_err(ResolveError::RegistryRouting)?,
                );
                if package.optional_peer_dependencies.contains(peer) {
                    continue;
                }
            }
            let valid = provider.as_ref().is_some_and(|provider| {
                let id = &expansion.graph.instances[provider].artifact;
                (!requirement.is_alias() || requirement.package_name(peer) == &id.name)
                    && requirement
                        .matches_tagged_version(&id.version, &expansion.packages[id].dist_tags)
            });
            if !valid {
                return Err(ResolveError::ContextualPeerDependency(Box::new(
                    PeerBindingFailure {
                        consumer: reference.clone(),
                        package: artifact.clone(),
                        peer: peer.clone(),
                        requirement: requirement.raw.clone(),
                        expected_source: provider
                            .as_ref()
                            .map(|reference| {
                                expansion.graph.instances[reference]
                                    .artifact
                                    .registry
                                    .clone()
                            })
                            .or(expected_source)
                            .expect("visible or absent binding routed"),
                        provider: provider
                            .as_ref()
                            .or_else(|| visible.and_then(|bindings| bindings.first()))
                            .map(|reference| expansion.graph.instances[reference].artifact.clone()),
                    },
                )));
            }
            let provider = provider.expect("validated provider");
            expansion.charge(peer.as_str().len().saturating_add(provider.0.len()), 1)?;
            expansion
                .graph
                .instances
                .get_mut(&reference)
                .expect("allocated instance")
                .peers
                .insert(peer.clone(), provider);
        }
    }
    Ok(expansion.graph)
}

#[cfg(test)]
mod tests;
