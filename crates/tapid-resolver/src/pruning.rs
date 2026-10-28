use super::*;

/// Whether a selected declaration is dispensable or constrains a peer provider.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DeclarationKind {
    Required,
    Optional,
    Peer,
    OptionalPeer,
}

/// A declaration and its exact selected target. Root names retain alias bindings.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ResolvedDeclaration {
    /// Absent for a manifest entry point.
    pub parent: Option<RegistryPackageId>,
    pub name: PackageName,
    pub requirement: Requirement,
    pub target: RegistryPackageId,
    pub kind: DeclarationKind,
}

/// Pruning fails closed on inconsistent facts or an unsatisfied retained contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PruneError {
    InvalidGraph,
    RequiredDeclaration(Box<ResolvedDeclaration>),
    PeerDeclaration(Box<ResolvedDeclaration>),
}

/// Retained graph and causal optional cuts, without independently blaming descendants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrunedResolution {
    pub resolution: Resolution,
    pub cuts: Vec<ResolvedDeclaration>,
    pub removed: Vec<RegistryPackageId>,
}

impl Resolution {
    /// Removes classified unavailable instances without version reselection.
    ///
    /// The caller owns failure classification and optional declaration normalization.
    /// This pure operation grants no authority to omit an installation failure.
    /// Required dependencies propagate unavailability backwards; peers are checked
    /// after reachability, rather than silently repaired by dropping their consumers.
    pub fn prune_unavailable(
        &self,
        failed: &BTreeSet<RegistryPackageId>,
    ) -> Result<PrunedResolution, PruneError> {
        self.validate_declarations()?;
        let selected: BTreeSet<_> = self.selected.iter().cloned().collect();
        if !failed.is_subset(&selected) {
            return Err(PruneError::InvalidGraph);
        }
        let declarations: BTreeSet<_> = self.declarations.iter().cloned().collect();
        let mut required_parents: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for edge in &declarations {
            if edge.kind == DeclarationKind::Required
                && let Some(parent) = &edge.parent
            {
                required_parents
                    .entry(&edge.target)
                    .or_default()
                    .push(parent);
            }
        }
        let mut unavailable = failed.clone();
        let mut pending: Vec<_> = failed.iter().cloned().collect();
        while let Some(child) = pending.pop() {
            for parent in required_parents.get(&child).into_iter().flatten() {
                if unavailable.insert((*parent).clone()) {
                    pending.push((*parent).clone());
                }
            }
        }
        for edge in &declarations {
            if edge.parent.is_none()
                && edge.kind == DeclarationKind::Required
                && unavailable.contains(&edge.target)
            {
                return Err(PruneError::RequiredDeclaration(Box::new(edge.clone())));
            }
        }
        let mut outgoing: BTreeMap<_, Vec<_>> = BTreeMap::new();
        let mut pending = Vec::new();
        for edge in &declarations {
            if matches!(
                edge.kind,
                DeclarationKind::Required | DeclarationKind::Optional
            ) && !unavailable.contains(&edge.target)
            {
                if let Some(parent) = &edge.parent {
                    outgoing.entry(parent).or_default().push(&edge.target);
                } else {
                    pending.push(edge.target.clone());
                }
            }
        }
        let retained = reachable(pending, &outgoing);
        let parent_retained = |edge: &ResolvedDeclaration| {
            edge.parent.as_ref().is_none_or(|id| retained.contains(id))
        };
        for edge in &declarations {
            if edge.kind == DeclarationKind::Peer
                && parent_retained(edge)
                && !retained.contains(&edge.target)
            {
                return Err(PruneError::PeerDeclaration(Box::new(edge.clone())));
            }
        }
        let cuts = declarations
            .iter()
            .filter(|edge| {
                edge.kind == DeclarationKind::Optional
                    && parent_retained(edge)
                    && unavailable.contains(&edge.target)
            })
            .cloned()
            .collect();
        let mut resolution = self.clone();
        resolution.selected = retained.iter().cloned().collect();
        resolution.roots.retain(|id| retained.contains(id));
        resolution.roots.sort();
        resolution
            .root_bindings
            .retain(|_, id| retained.contains(id));
        resolution
            .dependencies
            .retain(|edge| retained.contains(&edge.parent) && retained.contains(&edge.child));
        resolution.dependencies.sort();
        resolution.declarations = declarations
            .into_iter()
            .filter(|edge| parent_retained(edge) && retained.contains(&edge.target))
            .collect();
        resolution.peer_contexts = retained
            .iter()
            .map(|id| (id.clone(), PeerContext::default()))
            .collect();
        for edge in &resolution.declarations {
            if matches!(
                edge.kind,
                DeclarationKind::Peer | DeclarationKind::OptionalPeer
            ) {
                let context = resolution
                    .peer_contexts
                    .get_mut(edge.parent.as_ref().expect("validated peer parent"))
                    .expect("retained peer parent");
                *context = context
                    .clone()
                    .with(edge.name.clone(), edge.target.version.clone());
            }
        }
        Ok(PrunedResolution {
            removed: selected.difference(&retained).cloned().collect(),
            resolution,
            cuts,
        })
    }

    fn validate_declarations(&self) -> Result<(), PruneError> {
        if self.selected.len() > 100_000 || self.declarations.len() > 1_100_000 {
            return Err(PruneError::InvalidGraph);
        }
        let mut bindings = BTreeMap::new();
        let providers: BTreeSet<_> = self
            .root_bindings
            .iter()
            .map(|((_, name), id)| (name, id))
            .collect();
        let selected: BTreeSet<_> = self.selected.iter().cloned().collect();
        let mut roots = BTreeMap::new();
        let mut dependencies = BTreeSet::new();
        let mut peers: BTreeMap<_, PeerContext> = selected
            .iter()
            .map(|id| (id.clone(), PeerContext::default()))
            .collect();
        for edge in &self.declarations {
            let peer = matches!(
                edge.kind,
                DeclarationKind::Peer | DeclarationKind::OptionalPeer
            );
            let key = (
                edge.parent.clone(),
                edge.name.clone(),
                peer,
                if edge.parent.is_none() {
                    Some(edge.target.registry.clone())
                } else {
                    None
                },
            );
            let value = (edge.target.clone(), edge.kind);
            if bindings
                .insert(key, value.clone())
                .is_some_and(|previous| previous != value)
                || (edge.requirement.is_alias()
                    && edge.requirement.package_name(&edge.name) != &edge.target.name)
            {
                return Err(PruneError::InvalidGraph);
            }

            if !selected.contains(&edge.target)
                || edge
                    .parent
                    .as_ref()
                    .is_some_and(|id| !selected.contains(id))
            {
                return Err(PruneError::InvalidGraph);
            }
            match (&edge.parent, edge.kind) {
                (None, DeclarationKind::Required | DeclarationKind::Optional) => {
                    let key = (edge.target.registry.clone(), edge.name.clone());
                    if roots
                        .insert(key, edge.target.clone())
                        .is_some_and(|id| id != edge.target)
                    {
                        return Err(PruneError::InvalidGraph);
                    }
                }
                (Some(parent), DeclarationKind::Required | DeclarationKind::Optional) => {
                    dependencies.insert(ResolvedDependency {
                        parent: parent.clone(),
                        dependency: edge.name.clone(),
                        child: edge.target.clone(),
                    });
                }
                (Some(parent), DeclarationKind::Peer | DeclarationKind::OptionalPeer) => {
                    if !providers.contains(&(&edge.name, &edge.target)) {
                        return Err(PruneError::InvalidGraph);
                    }
                    let context = peers.get_mut(parent).ok_or(PruneError::InvalidGraph)?;
                    *context = context
                        .clone()
                        .with(edge.name.clone(), edge.target.version.clone());
                }
                _ => return Err(PruneError::InvalidGraph),
            }
        }
        let mut outgoing: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for edge in &self.dependencies {
            outgoing.entry(&edge.parent).or_default().push(&edge.child);
        }
        if reachable(self.roots.iter().cloned(), &outgoing) != selected {
            return Err(PruneError::InvalidGraph);
        }
        if roots != self.root_bindings
            || roots.values().cloned().collect::<BTreeSet<_>>()
                != self.roots.iter().cloned().collect()
            || dependencies != self.dependencies.iter().cloned().collect()
            || peers != self.peer_contexts
        {
            return Err(PruneError::InvalidGraph);
        }
        Ok(())
    }
}

fn reachable(
    roots: impl IntoIterator<Item = RegistryPackageId>,
    outgoing: &BTreeMap<&RegistryPackageId, Vec<&RegistryPackageId>>,
) -> BTreeSet<RegistryPackageId> {
    let mut pending: Vec<_> = roots.into_iter().collect();
    let mut retained = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if retained.insert(id.clone()) {
            pending.extend(
                outgoing
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .map(|id| (*id).clone()),
            );
        }
    }
    retained
}
