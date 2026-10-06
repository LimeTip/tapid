//! Pure, deterministic resolution of normalized registry metadata.
#![deny(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};
use tapid_core::{PackageName, PackageVersion, PeerContext, RegistryOrigin};
use tapid_registry_client::{RegistryPackageId, RegistrySnapshot};

#[cfg(test)]
thread_local! {
    static REQUIREMENT_BASE_PARSE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_requirement_base_parse_count() {
    REQUIREMENT_BASE_PARSE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn requirement_base_parse_count() -> usize {
    REQUIREMENT_BASE_PARSE_COUNT.with(std::cell::Cell::get)
}

/// A validated npm version range, optionally bound to an alias target.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Requirement {
    /// Canonical trimmed source requirement used for deterministic diagnostics.
    pub raw: String,
    alias: Option<PackageName>,
    clauses: Vec<RequirementClause>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RequirementClause {
    AnyStable,
    Comparators(Vec<RequirementComparator>),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RequirementComparator {
    op: RequirementOperator,
    base: RequirementBase,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RequirementOperator {
    Exact,
    Caret,
    Tilde,
    Greater,
    GreaterEqual,
    Less,
    LessEqual,
}

/// One registry-qualified dependency constraint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    /// Registry from which candidates must be selected.
    pub registry: RegistryOrigin,
    /// Package name constrained by this dependency.
    pub name: PackageName,
    /// Supported version requirement for the package.
    pub requirement: Requirement,
}
impl Dependency {
    /// Creates a registry-qualified dependency constraint.
    pub fn new(registry: RegistryOrigin, name: PackageName, requirement: Requirement) -> Self {
        Self {
            registry,
            name,
            requirement,
        }
    }
}

impl FromStr for Requirement {
    type Err = ResolveError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw = s.trim();
        if let Some(spec) = raw.strip_prefix("npm:") {
            let (name, range) = match spec.rfind('@').filter(|index| *index > 0) {
                Some(index) => (&spec[..index], &spec[index + 1..]),
                None => (spec, "*"),
            };
            let name = name
                .parse::<PackageName>()
                .map_err(|_| ResolveError::UnsupportedRange(raw.into()))?;
            let range = range.trim();
            if range.is_empty() || range.starts_with("npm:") {
                return Err(ResolveError::UnsupportedRange(raw.into()));
            }
            let mut requirement = range
                .parse::<Requirement>()
                .map_err(|_| ResolveError::UnsupportedRange(raw.into()))?;
            requirement.raw = raw.into();
            requirement.alias = Some(name);
            return Ok(requirement);
        }
        if raw.is_empty() {
            return Ok(Self {
                raw: raw.into(),
                alias: None,
                clauses: vec![RequirementClause::AnyStable],
            });
        }
        let mut clauses = Vec::new();
        for clause in raw.split("||") {
            let clause = clause.trim();
            if clause.is_empty() {
                return Err(ResolveError::UnsupportedRange(raw.into()));
            }
            if clause == "*" || clause.eq_ignore_ascii_case("x") {
                clauses.push(RequirementClause::AnyStable);
                continue;
            }
            let mut comparators = Vec::new();
            if let Some(comparators_for_wildcard) = parse_x_range(clause) {
                if comparators_for_wildcard.is_empty() {
                    clauses.push(RequirementClause::AnyStable);
                } else {
                    clauses.push(RequirementClause::Comparators(comparators_for_wildcard));
                }
                continue;
            }
            let tokens = clause.split_whitespace().collect::<Vec<_>>();
            if let Some((lower, upper)) =
                tokens.as_slice().split_first().and_then(|(first, rest)| {
                    if rest.first() == Some(&"-") && rest.len() == 2 {
                        Some((*first, rest[1]))
                    } else {
                        None
                    }
                })
            {
                let Some(lower) = parse_requirement_base(RequirementOperator::GreaterEqual, lower)
                else {
                    return Err(ResolveError::UnsupportedRange(raw.into()));
                };
                let Some(upper) = parse_requirement_base(RequirementOperator::LessEqual, upper)
                else {
                    return Err(ResolveError::UnsupportedRange(raw.into()));
                };
                comparators.push(RequirementComparator {
                    op: RequirementOperator::GreaterEqual,
                    base: lower,
                });
                comparators.push(RequirementComparator {
                    op: RequirementOperator::LessEqual,
                    base: upper,
                });
                clauses.push(RequirementClause::Comparators(comparators));
                continue;
            }
            let mut index = 0;
            while index < tokens.len() {
                let token = tokens[index];
                let separated_wildcard_comparators = separated_operator(token)
                    .and_then(|_| tokens.get(index + 1).copied())
                    .and_then(|value| parse_x_range(&format!("{token}{value}")));
                if let Some(wildcard_comparators) = separated_wildcard_comparators {
                    comparators.extend(wildcard_comparators);
                    index += 2;
                    continue;
                }
                if let Some(wildcard_comparators) = parse_x_range(token) {
                    comparators.extend(wildcard_comparators);
                    index += 1;
                    continue;
                }
                let (op, value) = if let Some(op) = separated_operator(token) {
                    index += 1;
                    let Some(value) = tokens.get(index).copied() else {
                        return Err(ResolveError::UnsupportedRange(raw.into()));
                    };
                    (op, value)
                } else {
                    requirement_token(token)
                };
                let Some(base) = parse_requirement_base(op, value) else {
                    return Err(ResolveError::UnsupportedRange(raw.into()));
                };
                comparators.push(RequirementComparator { op, base });
                index += 1;
            }
            clauses.push(RequirementClause::Comparators(comparators));
        }
        Ok(Self {
            raw: raw.into(),
            alias: None,
            clauses,
        })
    }
}

impl Requirement {
    /// Actual registry name for an npm alias, or the declared dependency name.
    pub fn package_name<'a>(&'a self, declared: &'a PackageName) -> &'a PackageName {
        self.alias.as_ref().unwrap_or(declared)
    }

    pub fn is_alias(&self) -> bool {
        self.alias.is_some()
    }
    /// Returns whether an exact version satisfies this validated requirement.
    pub fn matches(&self, version: &PackageVersion) -> bool {
        matches_requirement(version, self)
    }
}

fn parse_x_range(clause: &str) -> Option<Vec<RequirementComparator>> {
    let (op, value) = requirement_token(clause);
    let value = value.strip_prefix('v').unwrap_or(value);
    let value = if let Some((version, build)) = value.split_once('+') {
        semver::BuildMetadata::new(build).ok()?;
        version
    } else {
        value
    };
    let parts = value.split('.').collect::<Vec<_>>();
    let wildcard_at = parts
        .iter()
        .position(|part| matches!(part.to_ascii_lowercase().as_str(), "x" | "*"));
    let wildcard_at = wildcard_at?;
    if parts[wildcard_at..]
        .iter()
        .any(|part| !matches!(part.to_ascii_lowercase().as_str(), "x" | "*"))
    {
        return None;
    }
    if parts[..wildcard_at].iter().any(|part| {
        part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
    }) {
        return None;
    }
    let supported_op = matches!(
        op,
        RequirementOperator::Exact
            | RequirementOperator::Caret
            | RequirementOperator::Tilde
            | RequirementOperator::Greater
            | RequirementOperator::GreaterEqual
            | RequirementOperator::Less
            | RequirementOperator::LessEqual
    );
    if !supported_op {
        return None;
    }
    if wildcard_at == 0 {
        return Some(match op {
            RequirementOperator::Greater | RequirementOperator::Less => {
                vec![RequirementComparator {
                    op: RequirementOperator::Less,
                    base: RequirementBase {
                        version: PackageVersion::stable(0, 0, 0),
                        precision: RequirementPrecision::Full,
                    },
                }]
            }
            _ => Vec::new(),
        });
    }

    let prefix = parts[..wildcard_at].join(".");
    let lower = parse_requirement_base(RequirementOperator::GreaterEqual, &prefix)?;
    let lower_comparator = RequirementComparator {
        op: RequirementOperator::GreaterEqual,
        base: lower.clone(),
    };
    let partial_upper = RequirementBase {
        version: partial_upper_bound(&lower.version, lower.precision)?,
        precision: RequirementPrecision::Full,
    };
    let less_than = |base| RequirementComparator {
        op: RequirementOperator::Less,
        base,
    };

    Some(match op {
        RequirementOperator::Greater => vec![RequirementComparator {
            op: RequirementOperator::GreaterEqual,
            base: partial_upper,
        }],
        RequirementOperator::GreaterEqual => vec![lower_comparator],
        RequirementOperator::Less => vec![less_than(lower)],
        RequirementOperator::LessEqual => vec![less_than(partial_upper)],
        RequirementOperator::Caret => vec![
            lower_comparator,
            less_than(RequirementBase {
                version: caret_upper_bound(&lower)?,
                precision: RequirementPrecision::Full,
            }),
        ],
        RequirementOperator::Tilde | RequirementOperator::Exact => {
            vec![lower_comparator, less_than(partial_upper)]
        }
    })
}

fn separated_operator(token: &str) -> Option<RequirementOperator> {
    match token {
        "~" => Some(RequirementOperator::Tilde),
        ">" => Some(RequirementOperator::Greater),
        ">=" => Some(RequirementOperator::GreaterEqual),
        "<" => Some(RequirementOperator::Less),
        "<=" => Some(RequirementOperator::LessEqual),
        "=" => Some(RequirementOperator::Exact),
        _ => None,
    }
}

fn requirement_token(token: &str) -> (RequirementOperator, &str) {
    for (prefix, op) in [
        (">=", RequirementOperator::GreaterEqual),
        ("<=", RequirementOperator::LessEqual),
        (">", RequirementOperator::Greater),
        ("<", RequirementOperator::Less),
        ("^", RequirementOperator::Caret),
        ("~", RequirementOperator::Tilde),
        ("=", RequirementOperator::Exact),
    ] {
        if let Some(value) = token.strip_prefix(prefix) {
            return (op, value);
        }
    }
    (RequirementOperator::Exact, token)
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RequirementBase {
    version: PackageVersion,
    precision: RequirementPrecision,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RequirementPrecision {
    Full,
    Major,
    Minor,
}

fn parse_requirement_base(op: RequirementOperator, value: &str) -> Option<RequirementBase> {
    #[cfg(test)]
    REQUIREMENT_BASE_PARSE_COUNT.with(|count| count.set(count.get() + 1));

    let value = value.strip_prefix('v').unwrap_or(value);
    if let Ok(mut version) = semver::Version::parse(value) {
        version.build = semver::BuildMetadata::EMPTY;
        return Some(RequirementBase {
            version: version.to_string().parse().ok()?,
            precision: RequirementPrecision::Full,
        });
    }
    let components = value.split('.').collect::<Vec<_>>();
    if !matches!(components.len(), 1 | 2)
        || components.iter().any(|component| {
            component.is_empty()
                || !component.bytes().all(|byte| byte.is_ascii_digit())
                || (component.len() > 1 && component.starts_with('0'))
        })
        || !(matches!(
            op,
            RequirementOperator::Exact
                | RequirementOperator::Tilde
                | RequirementOperator::Greater
                | RequirementOperator::GreaterEqual
                | RequirementOperator::Less
                | RequirementOperator::LessEqual
        ) || op == RequirementOperator::Caret && matches!(components.len(), 1 | 2))
    {
        return None;
    }
    let major = components[0].parse().ok()?;
    let minor = components
        .get(1)
        .map_or(Some(0), |value| value.parse().ok())?;
    Some(RequirementBase {
        version: PackageVersion::stable(major, minor, 0),
        precision: if components.len() == 1 {
            RequirementPrecision::Major
        } else {
            RequirementPrecision::Minor
        },
    })
}

/// Normalized metadata supplied by a registry adapter. The resolver never fetches it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageVersionMetadata {
    /// Registry package name for this version record.
    pub name: PackageName,
    /// Exact canonical version represented by this record.
    pub version: PackageVersion,
    /// Dependency requirements declared by this exact version.
    pub dependencies: BTreeMap<PackageName, Requirement>,
    /// Peer requirements declared by this exact version. These are never merged
    /// into `dependencies` and are retained for context validation.
    pub peer_dependencies: BTreeMap<PackageName, Requirement>,
    /// Peer requirements explicitly marked optional by the registry.
    pub optional_peer_dependencies: BTreeSet<PackageName>,
}

/// Normalized deterministic package records belonging to one registry origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryMetadata {
    /// Registry origin shared by every package record.
    pub registry: RegistryOrigin,
    /// Records sorted by package name and descending version after normalization.
    pub packages: Vec<PackageVersionMetadata>,
}
impl RegistryMetadata {
    /// Sorts records deterministically and rejects duplicate exact identities.
    pub fn normalize(
        registry: RegistryOrigin,
        mut packages: Vec<PackageVersionMetadata>,
    ) -> Result<Self, ResolveError> {
        packages.sort_by(|a, b| a.name.cmp(&b.name).then(b.version.cmp(&a.version)));
        for pair in packages.windows(2) {
            if pair[0].name == pair[1].name && pair[0].version == pair[1].version {
                return Err(ResolveError::DuplicateMetadata {
                    package: format!("{}:{}@{}", registry, pair[0].name, pair[0].version),
                });
            }
        }
        Ok(Self { registry, packages })
    }
}

/// Network-mode constraints applied to pure resolution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolutionOptions {
    /// Requires all candidates to be supplied by the caller without fetching.
    pub offline: bool,
    /// Rejects fresh resolution because frozen mode requires lockfile replay.
    pub frozen: bool,
}

/// Exact package identities, root selections, and parent-to-child edges for a graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolution {
    /// Every exact package identity selected in deterministic order.
    pub selected: Vec<RegistryPackageId>,
    /// Exact identities selected for direct manifest dependencies.
    pub roots: Vec<RegistryPackageId>,
    /// Local root names bound to exact actual package identities.
    pub root_bindings: BTreeMap<(RegistryOrigin, PackageName), RegistryPackageId>,
    /// Exact dependency edges used by lockfile and linker construction.
    pub dependencies: Vec<ResolvedDependency>,
    /// Peer providers bound to each selected package identity.
    pub peer_contexts: BTreeMap<RegistryPackageId, PeerContext>,
}

/// Exact parent-to-child target selected for one dependency edge.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ResolvedDependency {
    /// Exact parent identity that declares this edge.
    pub parent: RegistryPackageId,
    /// Dependency name as declared by the parent.
    pub dependency: PackageName,
    /// Exact child identity selected for this parent edge.
    pub child: RegistryPackageId,
}

/// Structured deterministic failures from requirement parsing and graph selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    InvalidRequirement(String),
    UnsupportedRange(String),
    UnsupportedMode(&'static str),
    RegistryRouting(String),
    DuplicateMetadata {
        package: String,
    },
    MissingMetadata {
        packages: Vec<(String, String)>,
    },
    MissingCandidate {
        registry: String,
        name: String,
        requirement: String,
        available: Vec<String>,
    },
    Conflict {
        registry: String,
        name: String,
        requirements: Vec<String>,
        available: Vec<String>,
    },
    PeerDependency {
        package: String,
        peer: String,
        requirement: String,
        provider: Option<String>,
    },
}
impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PeerDependency {
                package,
                peer,
                requirement,
                provider: None,
            } => write!(
                f,
                "peer dependency unresolved: {package} requires {peer}@{requirement}, but no direct root provider was selected"
            ),
            Self::PeerDependency {
                package,
                peer,
                requirement,
                provider: Some(provider),
            } => write!(
                f,
                "peer dependency incompatible: {package} requires {peer}@{requirement}, but the direct root provider is {provider}"
            ),
            _ => write!(f, "{self:?}"),
        }
    }
}
impl std::error::Error for ResolveError {}

/// Resolve a transitive graph from normalized metadata. No network or filesystem access occurs.
pub fn resolve_graph(
    ds: &[Dependency],
    metadata: &[RegistryMetadata],
    options: ResolutionOptions,
) -> Result<Resolution, ResolveError> {
    resolve_graph_with_routing(ds, metadata, options, |parent, _| Ok(parent.clone()))
}

/// Resolves a graph while selecting each transitive package's registry from its
/// parent registry and dependency name. Root identities remain caller-selected.
pub fn resolve_graph_with_routing<F>(
    ds: &[Dependency],
    metadata: &[RegistryMetadata],
    options: ResolutionOptions,
    mut registry_for_dependency: F,
) -> Result<Resolution, ResolveError>
where
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<RegistryOrigin, String>,
{
    if options.frozen {
        return Err(ResolveError::UnsupportedMode(
            "frozen resolution requires a lockfile replay input",
        ));
    }
    if options.offline && metadata.is_empty() {
        return Err(ResolveError::UnsupportedMode(
            "offline resolution requires a cached snapshot",
        ));
    }
    let candidate_index = candidate_index(metadata);
    let mut root_constraints: BTreeMap<
        (RegistryOrigin, PackageName, PackageName),
        BTreeSet<Requirement>,
    > = BTreeMap::new();
    for dependency in ds {
        root_constraints
            .entry((
                dependency.registry.clone(),
                dependency.name.clone(),
                dependency
                    .requirement
                    .package_name(&dependency.name)
                    .clone(),
            ))
            .or_default()
            .insert(dependency.requirement.clone());
    }

    let mut selected = BTreeSet::new();
    let mut selected_packages = BTreeMap::new();
    let mut roots = Vec::new();
    let mut root_bindings = BTreeMap::new();
    let mut queue = Vec::new();
    let mut missing_metadata = BTreeSet::new();
    for ((registry, local_name, name), requirements) in root_constraints {
        let package = match select_package(&registry, &name, &requirements, &candidate_index) {
            Ok(package) => package,
            Err(ResolveError::MissingCandidate { .. })
                if !candidate_index.contains_key(&(registry.clone(), name.clone())) =>
            {
                missing_metadata.insert((registry.to_string(), name.to_string()));
                continue;
            }
            Err(error) => return Err(error),
        };
        let id = RegistryPackageId::new(registry.clone(), name, package.version.clone());
        selected.insert(id.clone());
        selected_packages.insert(id.clone(), package);
        roots.push(id.clone());
        if let Some(previous) = root_bindings.insert((registry, local_name.clone()), id.clone())
            && previous != id
        {
            return Err(ResolveError::RegistryRouting(format!(
                "conflicting root binding for {local_name}"
            )));
        }
        queue.push(id);
    }

    let mut dependencies = BTreeSet::new();
    let mut expanded = BTreeSet::new();
    while let Some(parent) = queue.pop() {
        if !expanded.insert(parent.clone()) {
            continue;
        }
        let package_dependencies = selected_packages
            .get(&parent)
            .expect("selected package metadata")
            .dependencies
            .clone();
        for (dependency, requirement) in package_dependencies {
            let actual_name = requirement.package_name(&dependency).clone();
            let registry = registry_for_dependency(&parent.registry, &actual_name)
                .map_err(ResolveError::RegistryRouting)?;
            let requirements = BTreeSet::from([requirement]);
            let child_package =
                match select_package(&registry, &actual_name, &requirements, &candidate_index) {
                    Ok(package) => package,
                    Err(ResolveError::MissingCandidate { .. })
                        if !candidate_index
                            .contains_key(&(registry.clone(), actual_name.clone())) =>
                    {
                        missing_metadata.insert((registry.to_string(), actual_name.to_string()));
                        continue;
                    }
                    Err(error) => return Err(error),
                };
            let child =
                RegistryPackageId::new(registry, actual_name, child_package.version.clone());
            dependencies.insert(ResolvedDependency {
                parent: parent.clone(),
                dependency: dependency.clone(),
                child: child.clone(),
            });
            if selected.insert(child.clone()) {
                selected_packages.insert(child.clone(), child_package);
                queue.push(child);
            }
        }
    }

    if !missing_metadata.is_empty() {
        return Err(ResolveError::MissingMetadata {
            packages: missing_metadata.into_iter().collect(),
        });
    }

    roots.sort();
    roots.dedup();
    let mut root_providers: BTreeMap<PackageName, Vec<&RegistryPackageId>> = BTreeMap::new();
    for ((_, local_name), root) in &root_bindings {
        root_providers
            .entry(local_name.clone())
            .or_default()
            .push(root);
    }
    let mut peer_contexts = BTreeMap::new();
    for (id, package) in &selected_packages {
        let mut context = PeerContext::default();
        for (peer, requirement) in &package.peer_dependencies {
            let candidates = root_providers.get(peer);
            let mut provider = None;
            for candidate in candidates.into_iter().flatten() {
                let actual_name = if requirement.is_alias() {
                    requirement.package_name(peer)
                } else {
                    &candidate.name
                };
                let peer_registry = registry_for_dependency(&id.registry, actual_name)
                    .map_err(ResolveError::RegistryRouting)?;
                if candidate.registry != peer_registry {
                    continue;
                }
                if provider.replace(*candidate).is_some() {
                    return Err(ResolveError::RegistryRouting(format!(
                        "ambiguous root peer binding for {peer}"
                    )));
                }
            }
            if candidates.is_none() {
                // Preserve route validation even when no local provider exists.
                registry_for_dependency(&id.registry, requirement.package_name(peer))
                    .map_err(ResolveError::RegistryRouting)?;
            }
            if provider.is_none() && package.optional_peer_dependencies.contains(peer) {
                continue;
            }

            if !provider.is_some_and(|provider| {
                requirement.matches(&provider.version)
                    && (!requirement.is_alias() || requirement.package_name(peer) == &provider.name)
            }) {
                return Err(ResolveError::PeerDependency {
                    package: id.to_string(),
                    peer: peer.to_string(),
                    requirement: requirement.raw.clone(),
                    provider: provider.map(|provider| provider.version.to_string()),
                });
            }
            context = context.with(
                peer.clone(),
                provider
                    .expect("validated root peer provider")
                    .version
                    .clone(),
            );
        }
        peer_contexts.insert(id.clone(), context);
    }

    Ok(Resolution {
        selected: selected.into_iter().collect(),
        roots,
        root_bindings,
        dependencies: dependencies.into_iter().collect(),
        peer_contexts,
    })
}

type CandidateIndex<'a> = BTreeMap<(RegistryOrigin, PackageName), Vec<&'a PackageVersionMetadata>>;

fn candidate_index(metadata: &[RegistryMetadata]) -> CandidateIndex<'_> {
    let mut index = CandidateIndex::new();
    for registry in metadata {
        for package in &registry.packages {
            index
                .entry((registry.registry.clone(), package.name.clone()))
                .or_default()
                .push(package);
        }
    }
    index
}

fn select_package(
    registry: &RegistryOrigin,
    name: &PackageName,
    requirements: &BTreeSet<Requirement>,
    candidates: &CandidateIndex<'_>,
) -> Result<PackageVersionMetadata, ResolveError> {
    let matching = candidates
        .get(&(registry.clone(), name.clone()))
        .map(Vec::as_slice)
        .unwrap_or_default();
    let package = matching
        .iter()
        .copied()
        .filter(|package| {
            requirements
                .iter()
                .all(|requirement| matches_requirement(&package.version, requirement))
        })
        .max_by(|a, b| a.version.cmp(&b.version))
        .cloned();
    package.ok_or_else(|| {
        let requirements: Vec<_> = requirements
            .iter()
            .map(|requirement| requirement.raw.clone())
            .collect();
        let available = available(matching);
        if requirements.len() > 1 {
            ResolveError::Conflict {
                registry: registry.to_string(),
                name: name.to_string(),
                requirements,
                available,
            }
        } else {
            ResolveError::MissingCandidate {
                registry: registry.to_string(),
                name: name.to_string(),
                requirement: requirements.into_iter().next().unwrap_or_default(),
                available,
            }
        }
    })
}

fn available(candidates: &[&PackageVersionMetadata]) -> Vec<String> {
    let mut versions = candidates
        .iter()
        .map(|package| &package.version)
        .collect::<Vec<_>>();
    versions.sort();
    versions.dedup();
    versions.into_iter().map(ToString::to_string).collect()
}

fn partial_upper_bound(
    base: &PackageVersion,
    precision: RequirementPrecision,
) -> Option<PackageVersion> {
    match precision {
        RequirementPrecision::Full => None,
        RequirementPrecision::Major => base
            .major()
            .checked_add(1)
            .map(|major| PackageVersion::stable(major, 0, 0)),
        RequirementPrecision::Minor => base
            .minor()
            .checked_add(1)
            .map(|minor| PackageVersion::stable(base.major(), minor, 0))
            .or_else(|| {
                base.major()
                    .checked_add(1)
                    .map(|major| PackageVersion::stable(major, 0, 0))
            }),
    }
}

fn caret_upper_bound(base: &RequirementBase) -> Option<PackageVersion> {
    if base.precision == RequirementPrecision::Major || base.version.major() > 0 {
        base.version
            .major()
            .checked_add(1)
            .map(|major| PackageVersion::stable(major, 0, 0))
    } else if base.precision == RequirementPrecision::Minor || base.version.minor() > 0 {
        base.version
            .minor()
            .checked_add(1)
            .map(|minor| PackageVersion::stable(0, minor, 0))
    } else {
        base.version
            .patch()
            .checked_add(1)
            .map(|patch| PackageVersion::stable(0, 0, patch))
    }
}

fn matches_requirement(version: &PackageVersion, requirement: &Requirement) -> bool {
    requirement.clauses.iter().any(|clause| match clause {
        RequirementClause::AnyStable => version.prerelease().is_none(),
        RequirementClause::Comparators(comparators) => {
            let prerelease_is_eligible = version.prerelease().is_none()
                || comparators.iter().any(|comparator| {
                    let base = &comparator.base.version;
                    base.prerelease().is_some()
                        && version.major() == base.major()
                        && version.minor() == base.minor()
                        && version.patch() == base.patch()
                });
            prerelease_is_eligible
                && comparators.iter().all(|comparator| {
                    let base = &comparator.base.version;
                    match comparator.op {
                        RequirementOperator::Exact => match comparator.base.precision {
                            RequirementPrecision::Full => version == base,
                            RequirementPrecision::Major => {
                                version >= base && version.major() == base.major()
                            }
                            RequirementPrecision::Minor => {
                                version >= base
                                    && version.major() == base.major()
                                    && version.minor() == base.minor()
                            }
                        },
                        RequirementOperator::Caret => caret_upper_bound(&comparator.base)
                            .map(|upper| version >= base && version < &upper)
                            .unwrap_or_else(|| {
                                if comparator.base.precision == RequirementPrecision::Major
                                    || base.major() > 0
                                {
                                    version >= base && version.major() == base.major()
                                } else if comparator.base.precision == RequirementPrecision::Minor
                                    || base.minor() > 0
                                {
                                    version >= base
                                        && version.major() == base.major()
                                        && version.minor() == base.minor()
                                } else {
                                    version == base
                                }
                            }),
                        RequirementOperator::Tilde => {
                            version >= base
                                && version.major() == base.major()
                                && (comparator.base.precision == RequirementPrecision::Major
                                    || version.minor() == base.minor())
                        }
                        RequirementOperator::Greater => {
                            if comparator.base.precision == RequirementPrecision::Full {
                                version > base
                            } else {
                                partial_upper_bound(base, comparator.base.precision)
                                    .is_some_and(|upper| version >= &upper)
                            }
                        }
                        RequirementOperator::GreaterEqual => version >= base,
                        RequirementOperator::Less => match comparator.base.precision {
                            RequirementPrecision::Full => version < base,
                            RequirementPrecision::Major => version.major() < base.major(),
                            RequirementPrecision::Minor => {
                                (version.major(), version.minor()) < (base.major(), base.minor())
                            }
                        },
                        RequirementOperator::LessEqual => match comparator.base.precision {
                            RequirementPrecision::Full => version <= base,
                            RequirementPrecision::Major => version.major() <= base.major(),
                            RequirementPrecision::Minor => {
                                (version.major(), version.minor()) <= (base.major(), base.minor())
                            }
                        },
                    }
                })
        }
    })
}

/// Compatibility entry point for metadata snapshots without dependency maps.
pub fn resolve(
    ds: &[Dependency],
    snapshots: &[RegistrySnapshot],
    options: ResolutionOptions,
) -> Result<Resolution, ResolveError> {
    let metadata = snapshots
        .iter()
        .map(|snapshot| RegistryMetadata {
            registry: snapshot.registry().clone(),
            packages: snapshot
                .packages()
                .values()
                .flatten()
                .map(|p| PackageVersionMetadata {
                    name: p.identity.name.clone(),
                    version: p.identity.version.clone(),
                    dependencies: BTreeMap::new(),
                    peer_dependencies: BTreeMap::new(),
                    optional_peer_dependencies: BTreeSet::new(),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    resolve_graph(ds, &metadata, options)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
