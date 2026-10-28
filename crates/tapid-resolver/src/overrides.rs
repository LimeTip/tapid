//! Bounded, range-only policy evaluated against local dependency bindings.

use super::*;

/// One compiled declaration. Identifiers are supplied by the manifest consumer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverrideRule {
    pub id: String,
    /// Local parent name and optional selected-version constraint; None is flat.
    pub parent: Option<(PackageName, Option<Requirement>)>,
    pub dependency: PackageName,
    pub replacement: Requirement,
}

/// Compiled range policy indexed by the local child name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OverridePolicy {
    rules: BTreeMap<PackageName, Vec<OverrideRule>>,
    registry: Option<PackageSource>,
    pub(super) bytes: usize,
}

/// Actual policy evidence for one reachable parent binding and dependency edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverrideMatch {
    pub parent: RegistryPackageId,
    pub binding: PackageName,
    pub dependency: PackageName,
    pub original: Requirement,
    pub effective: Requirement,
    pub rules: Vec<String>,
}

impl OverridePolicy {
    /// Evaluate current policy for a bounded batch of observed edges. This is
    /// policy evaluation, not evidence of how a persisted graph was resolved.
    pub fn inspect_edges<'a>(
        &self,
        edges: impl IntoIterator<
            Item = (
                &'a RegistryPackageId,
                &'a PackageName,
                &'a PackageName,
                &'a Requirement,
            ),
        >,
    ) -> Result<Vec<OverrideMatch>, ResolveError> {
        let mut work = 0;
        let mut bytes = self.bytes;
        let mut matches = Vec::new();
        for (parent, binding, dependency, original) in edges {
            work += 1;
            if work > 1_000_000 {
                return Err(ResolveError::MetadataLimitExceeded);
            }
            let (effective, rules) =
                self.apply(parent, binding, dependency, original, &mut work, &mut bytes)?;
            if !rules.is_empty() {
                bytes = bytes
                    .saturating_add(std::mem::size_of::<OverrideMatch>())
                    .saturating_add(parent.registry.as_str().len())
                    .saturating_add(parent.name.as_str().len())
                    .saturating_add(parent.version.to_string().len())
                    .saturating_add(binding.as_str().len())
                    .saturating_add(dependency.as_str().len());
                if bytes > 64 * 1024 * 1024 {
                    return Err(ResolveError::MetadataLimitExceeded);
                }
                matches.push(OverrideMatch {
                    parent: parent.clone(),
                    binding: binding.clone(),
                    dependency: dependency.clone(),
                    original: original.clone(),
                    effective,
                    rules,
                });
            }
        }
        Ok(matches)
    }

    /// Restrict policy to declaring parents from one exact registry origin.
    pub fn new_for_registry(
        rules: Vec<OverrideRule>,
        registry: PackageSource,
    ) -> Result<Self, ResolveError> {
        let mut policy = Self::new(rules)?;
        policy.bytes = policy.bytes.saturating_add(registry.as_str().len());
        if policy.bytes > 64 * 1024 * 1024 {
            return Err(ResolveError::MetadataLimitExceeded);
        }
        policy.registry = Some(registry);
        Ok(policy)
    }

    pub fn new(rules: Vec<OverrideRule>) -> Result<Self, ResolveError> {
        if rules.len() > 100_000 {
            return Err(ResolveError::MetadataLimitExceeded);
        }
        let mut policy = Self::default();
        let mut identifiers = BTreeSet::new();
        for rule in rules {
            let valid_range = |requirement: &Requirement| {
                !requirement.is_alias() && requirement.dist_tag().is_none()
            };
            if !valid_range(&rule.replacement)
                || rule
                    .parent
                    .as_ref()
                    .and_then(|(_, range)| range.as_ref())
                    .is_some_and(|range| !valid_range(range))
                || !identifiers.insert(rule.id.clone())
            {
                return Err(ResolveError::InvalidOverride { rule: rule.id });
            }
            policy.bytes = policy
                .bytes
                .saturating_add(rule.id.len())
                .saturating_add(rule.dependency.as_str().len())
                .saturating_add(rule.replacement.retained_bytes());
            if let Some((name, range)) = &rule.parent {
                policy.bytes = policy
                    .bytes
                    .saturating_add(name.as_str().len())
                    .saturating_add(range.as_ref().map_or(0, Requirement::retained_bytes));
            }
            if policy.bytes > 64 * 1024 * 1024 {
                return Err(ResolveError::MetadataLimitExceeded);
            }
            policy
                .rules
                .entry(rule.dependency.clone())
                .or_default()
                .push(rule);
        }
        for rules in policy.rules.values_mut() {
            rules.sort_by(|left, right| left.id.cmp(&right.id));
        }
        Ok(policy)
    }

    pub(super) fn apply(
        &self,
        parent: &RegistryPackageId,
        binding: &PackageName,
        dependency: &PackageName,
        original: &Requirement,
        work: &mut usize,
        bytes: &mut usize,
    ) -> Result<(Requirement, Vec<String>), ResolveError> {
        if parent.registry.registry().is_none()
            || self
                .registry
                .as_ref()
                .is_some_and(|registry| registry != &parent.registry)
        {
            return Ok((original.clone(), Vec::new()));
        }
        let mut matches = Vec::new();
        let mut precedence = 0;
        for rule in self.rules.get(dependency).into_iter().flatten() {
            *work = work.saturating_add(1);
            if *work > 1_000_000 {
                return Err(ResolveError::MetadataLimitExceeded);
            }
            let rank = match &rule.parent {
                None => 0,
                Some((name, range)) if name == binding => {
                    if let Some(range) = range {
                        if !range.matches(&parent.version) {
                            continue;
                        }
                        2
                    } else {
                        1
                    }
                }
                _ => continue,
            };
            if rank < precedence {
                continue;
            }
            if rank > precedence {
                matches.clear();
                precedence = rank;
            }
            matches.push(rule);
        }
        let Some(first) = matches.first() else {
            return Ok((original.clone(), Vec::new()));
        };
        // Charge evidence before copying identifiers or parsed replacement ranges.
        for rule in &matches {
            *bytes = bytes.saturating_add(rule.id.len());
        }
        *bytes = bytes
            .saturating_add(original.retained_bytes())
            .saturating_add(first.replacement.retained_bytes());
        if *bytes > 64 * 1024 * 1024 {
            return Err(ResolveError::MetadataLimitExceeded);
        }
        if matches
            .iter()
            .any(|rule| !same_range(&first.replacement, &rule.replacement))
        {
            return Err(ResolveError::OverrideConflict {
                parent: Box::new(parent.clone()),
                binding: binding.clone(),
                dependency: dependency.clone(),
                rules: matches.iter().map(|rule| rule.id.clone()).collect(),
            });
        }
        let mut effective = first.replacement.clone();
        effective.alias = original.alias.clone();
        if let Some(alias) = &effective.alias {
            effective.raw = format!("npm:{alias}@{}", first.replacement.raw);
        }
        Ok((
            effective,
            matches.iter().map(|rule| rule.id.clone()).collect(),
        ))
    }
}

pub(super) fn same_range(left: &Requirement, right: &Requirement) -> bool {
    left.alias == right.alias && left.tag == right.tag && left.clauses == right.clauses
}
