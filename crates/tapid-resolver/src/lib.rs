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
        || (!matches!(
            op,
            RequirementOperator::Exact
                | RequirementOperator::Tilde
                | RequirementOperator::Greater
                | RequirementOperator::GreaterEqual
                | RequirementOperator::Less
                | RequirementOperator::LessEqual
        ) && !(op == RequirementOperator::Caret && matches!(components.len(), 1 | 2)))
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
    let root_providers = root_bindings
        .iter()
        .map(|((registry, local_name), root)| ((registry.clone(), local_name.clone()), root))
        .collect::<BTreeMap<_, _>>();
    let mut peer_contexts = BTreeMap::new();
    for (id, package) in &selected_packages {
        let mut context = PeerContext::default();
        for (peer, requirement) in &package.peer_dependencies {
            let peer_registry =
                registry_for_dependency(&id.registry, requirement.package_name(peer))
                    .map_err(ResolveError::RegistryRouting)?;
            let provider = root_providers.get(&(peer_registry, peer.clone()));
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
mod tests {
    use super::*;
    use proptest::prelude::*;
    fn req(s: &str) -> Requirement {
        s.parse().unwrap()
    }

    #[test]
    fn npm_aliases_validate_actual_names_and_ranges() {
        let local: PackageName = "local".parse().unwrap();
        for (spec, target) in [
            ("npm:h3@2.0.1-rc.20", "h3"),
            ("npm:@scope/pkg@^1", "@scope/pkg"),
            ("npm:@scope/pkg", "@scope/pkg"),
        ] {
            let requirement = req(spec);
            assert_eq!(requirement.raw, spec);
            assert_eq!(requirement.package_name(&local).as_str(), target);
            assert!(requirement.is_alias());
        }
        assert!(req("npm:h3@2.0.1-rc.20").matches(&"2.0.1-rc.20".parse().unwrap()));
        assert!(!req("npm:h3@2.0.1-rc.20").matches(&"2.0.1".parse().unwrap()));
        for spec in [
            "npm:",
            "npm:pkg@",
            "npm:pkg@latest",
            "npm:../pkg@1",
            "npm:@scope@1",
            "npm:pkg@npm:other@1",
            "npm:https://example.test/pkg@1",
            "npm:pkg@workspace:*",
        ] {
            assert!(spec.parse::<Requirement>().is_err(), "{spec}");
        }
    }

    #[test]
    fn aliases_select_distinct_root_versions_and_route_transitives_by_actual_name() {
        let public = "https://registry.npmjs.org";
        let private = "https://packages.example";
        let metadata = vec![
            registry(
                public,
                vec![
                    package("h3", "1.0.0", &[]),
                    package("h3", "2.0.0", &[]),
                    package("parent", "1.0.0", &[("local", "npm:@actual/pkg@^1")]),
                ],
            ),
            registry(private, vec![package("@actual/pkg", "1.2.0", &[])]),
        ];
        let resolution = resolve_graph_with_routing(
            &[
                dep(public, "first", "npm:h3@1"),
                dep(public, "second", "npm:h3@2"),
                dep(public, "parent", "1"),
            ],
            &metadata,
            Default::default(),
            |parent, name| {
                Ok(if name.as_str() == "@actual/pkg" {
                    private.parse().unwrap()
                } else {
                    parent.clone()
                })
            },
        )
        .unwrap();
        assert_eq!(resolution.selected.len(), 4);
        let bindings = resolution
            .root_bindings
            .iter()
            .map(|((_, name), id)| (name.as_str(), id.version.to_string()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(bindings["first"], "1.0.0");
        assert_eq!(bindings["second"], "2.0.0");
        let edge = &resolution.dependencies[0];
        assert_eq!(edge.dependency.as_str(), "local");
        assert_eq!(edge.child.name.as_str(), "@actual/pkg");
        assert_eq!(edge.child.registry.as_str(), private);
    }

    #[test]
    fn validated_requirement_reuses_its_parsed_form_when_matching_candidates() {
        reset_requirement_base_parse_count();
        let requirement = req("^1.2.3");
        let parses_after_validation = requirement_base_parse_count();
        assert!(parses_after_validation > 0);

        for version in ["1.2.3", "1.9.0", "2.0.0"] {
            assert_eq!(
                requirement.matches(&version.parse().unwrap()),
                version != "2.0.0"
            );
        }

        assert_eq!(
            requirement_base_parse_count(),
            parses_after_validation,
            "candidate matching must not repeatedly parse the validated requirement"
        );
    }

    proptest! {
        #[test]
        fn generated_exact_requirements_trim_and_match_their_version(
            major in 0u64..1000, minor in 0u64..1000, patch in 0u64..1000,
        ) {
            let version_text = format!("{major}.{minor}.{patch}");
            let version: PackageVersion = version_text.parse().unwrap();
            let requirement: Requirement = format!("  ={version_text}  ").parse().unwrap();
            prop_assert_eq!(&requirement.raw, &format!("={version_text}"));
            prop_assert!(requirement.matches(&version));
        }
    }

    fn dep(registry: &str, name: &str, range: &str) -> Dependency {
        Dependency::new(registry.parse().unwrap(), name.parse().unwrap(), req(range))
    }
    fn package(name: &str, version: &str, dependencies: &[(&str, &str)]) -> PackageVersionMetadata {
        PackageVersionMetadata {
            name: name.parse().unwrap(),
            version: version.parse().unwrap(),
            dependencies: dependencies
                .iter()
                .map(|(n, r)| (n.parse().unwrap(), req(r)))
                .collect(),
            peer_dependencies: BTreeMap::new(),
            optional_peer_dependencies: BTreeSet::new(),
        }
    }
    fn registry(url: &str, packages: Vec<PackageVersionMetadata>) -> RegistryMetadata {
        RegistryMetadata::normalize(url.parse().unwrap(), packages).unwrap()
    }

    #[test]
    fn exact_prerelease_selects_only_the_matching_candidate() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", "2.0.0-rc.23", &[]),
                package("foo", "2.0.0-rc.24", &[]),
                package("foo", "2.0.0", &[]),
            ],
        );
        let r = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "2.0.0-rc.24")],
            &[m],
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            r.selected[0].to_string(),
            "https://registry.npmjs.org:foo@2.0.0-rc.24"
        );
    }

    #[test]
    fn prerelease_caret_selects_matching_prereleases_and_stable_release() {
        let prerelease_only = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", "2.0.0-next.4", &[]),
                package("foo", "2.0.0-next.6", &[]),
                package("foo", "2.1.0-next.1", &[]),
            ],
        );
        let selected = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "^2.0.0-next.5")],
            &[prerelease_only],
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            selected.selected[0].to_string(),
            "https://registry.npmjs.org:foo@2.0.0-next.6"
        );

        let with_stable = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", "2.0.0-next.6", &[]),
                package("foo", "2.0.0", &[]),
            ],
        );
        let selected = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "^2.0.0-next.5")],
            &[with_stable],
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            selected.selected[0].to_string(),
            "https://registry.npmjs.org:foo@2.0.0"
        );
    }

    #[test]
    fn stable_ranges_do_not_select_prerelease_candidates() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![package("foo", "2.0.0-rc.24", &[])],
        );
        let error = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "*")],
            &[m],
            Default::default(),
        )
        .unwrap_err();
        assert!(matches!(error, ResolveError::MissingCandidate { .. }));
    }

    #[test]
    fn npm_or_ranges_select_the_highest_matching_alternative() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", "2.4.1", &[]),
                package("foo", "2.9.0", &[]),
                package("foo", "3.1.0", &[]),
                package("foo", "4.0.0", &[]),
            ],
        );
        let selected = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "^2.4.1 || ^3.0.0")],
            &[m],
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            selected.selected[0].to_string(),
            "https://registry.npmjs.org:foo@3.1.0"
        );
    }

    #[test]
    fn malformed_or_ranges_are_rejected() {
        for requirement in ["|| ^1.0.0", "^1.0.0 ||", "^1.0.0 || || ^2.0.0"] {
            assert!(matches!(
                requirement.parse::<Requirement>(),
                Err(ResolveError::UnsupportedRange(_))
            ));
        }
    }

    #[test]
    fn npm_wildcard_and_hyphen_ranges_match_bounds() {
        let any: Requirement = "x".parse().unwrap();
        assert!(any.matches(&"1.2.3".parse().unwrap()));
        assert!(!any.matches(&"1.2.3-beta.1".parse().unwrap()));

        let hyphen: Requirement = "1.2.0 - 2.0.0".parse().unwrap();
        assert!(!hyphen.matches(&"1.1.9".parse().unwrap()));
        assert!(hyphen.matches(&"1.2.0".parse().unwrap()));
        assert!(hyphen.matches(&"2.0.0".parse().unwrap()));
        assert!(!hyphen.matches(&"2.0.1".parse().unwrap()));
    }

    #[test]
    fn npm_x_ranges_match_major_minor_and_all_wildcards() {
        for (range, yes, no) in [
            ("1.x", "1.9.0", "2.0.0"),
            ("1.2.*", "1.2.9", "1.3.0"),
            ("*.*", "9.0.0", "1.0.0-beta.1"),
        ] {
            let requirement: Requirement = range.parse().unwrap();
            assert!(requirement.matches(&yes.parse().unwrap()), "{range}");
            assert!(!requirement.matches(&no.parse().unwrap()), "{range}");
        }
    }

    #[test]
    fn exact_and_caret_are_deterministic() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", "1.1.0", &[]),
                package("foo", "1.9.0", &[]),
                package("foo", "2.0.0", &[]),
            ],
        );
        let r = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "^1.0.0")],
            &[m],
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            r.selected[0].to_string(),
            "https://registry.npmjs.org:foo@1.9.0"
        );
    }

    #[test]
    fn npm_comparison_intersections_support_spaced_and_compact_operators() {
        for text in [">= 2.1.2 < 3", ">=2.1.2 <3"] {
            let requirement: Requirement = text.parse().unwrap();
            assert!(!requirement.matches(&"2.1.1".parse().unwrap()), "{text}");
            assert!(requirement.matches(&"2.1.2".parse().unwrap()), "{text}");
            assert!(requirement.matches(&"2.9.9".parse().unwrap()), "{text}");
            assert!(!requirement.matches(&"3.0.0".parse().unwrap()), "{text}");
            assert!(
                !requirement.matches(&"2.2.0-beta.1".parse().unwrap()),
                "{text}"
            );
        }
    }

    #[test]
    fn partial_comparison_bounds_follow_npm_x_range_semantics() {
        for (text, matching, rejected) in [
            (">2", "3.0.0", "2.9.9"),
            ("<=2", "2.9.9", "3.0.0"),
            (">2.1", "2.2.0", "2.1.99"),
            ("<=2.1", "2.1.99", "2.2.0"),
        ] {
            let requirement: Requirement = text.parse().unwrap();
            assert!(requirement.matches(&matching.parse().unwrap()), "{text}");
            assert!(!requirement.matches(&rejected.parse().unwrap()), "{text}");
        }

        for text in ["<3 3.0.0-beta.1", "<=2 3.0.0-beta.1"] {
            let requirement: Requirement = text.parse().unwrap();
            assert!(
                !requirement.matches(&"3.0.0-beta.1".parse().unwrap()),
                "{text} must preserve npm's -0 partial upper bound"
            );
        }
    }

    #[test]
    fn partial_range_intersection_allows_explicit_matching_prerelease() {
        let below_major_floor: Requirement = "2 2.0.0-beta.1".parse().unwrap();
        assert!(!below_major_floor.matches(&"2.0.0-beta.1".parse().unwrap()));

        let major: Requirement = "2 2.1.0-beta.1".parse().unwrap();
        assert!(major.matches(&"2.1.0-beta.1".parse().unwrap()));
        assert!(!major.matches(&"2.1.0-beta.2".parse().unwrap()));

        let below_minor_floor: Requirement = "2.1 2.1.0-beta.1".parse().unwrap();
        assert!(!below_minor_floor.matches(&"2.1.0-beta.1".parse().unwrap()));

        let minor: Requirement = "2.1 2.1.1-beta.1".parse().unwrap();
        assert!(minor.matches(&"2.1.1-beta.1".parse().unwrap()));
        assert!(!minor.matches(&"2.1.2-beta.1".parse().unwrap()));
    }

    #[test]
    fn bare_major_range_selects_highest_matching_major() {
        let requirement = req("2");
        assert!(!requirement.matches(&"2.0.0-beta.1".parse().unwrap()));
        assert!(requirement.matches(&"2.0.0".parse().unwrap()));
        assert!(requirement.matches(&"2.9.9".parse().unwrap()));
        assert!(!requirement.matches(&"3.0.0".parse().unwrap()));
    }

    #[test]
    fn bare_minor_range_selects_highest_matching_minor() {
        let requirement = req("2.1");
        assert!(!requirement.matches(&"2.1.0-beta.1".parse().unwrap()));
        assert!(requirement.matches(&"2.1.0".parse().unwrap()));
        assert!(requirement.matches(&"2.1.9".parse().unwrap()));
        assert!(!requirement.matches(&"2.2.0".parse().unwrap()));
    }

    #[test]
    fn partial_tilde_ranges_follow_npm_semantics() {
        for text in ["~2", "~ 2"] {
            let requirement: Requirement = text.parse().unwrap();
            assert!(
                !requirement.matches(&"2.0.0-beta.1".parse().unwrap()),
                "{text}"
            );
            assert!(requirement.matches(&"2.0.0".parse().unwrap()), "{text}");
            assert!(requirement.matches(&"2.9.9".parse().unwrap()), "{text}");
            assert!(!requirement.matches(&"3.0.0".parse().unwrap()), "{text}");
        }

        for text in ["~2.1", "~ 2.1"] {
            let requirement: Requirement = text.parse().unwrap();
            assert!(
                !requirement.matches(&"2.1.0-beta.1".parse().unwrap()),
                "{text}"
            );
            assert!(requirement.matches(&"2.1.0".parse().unwrap()), "{text}");
            assert!(requirement.matches(&"2.1.99".parse().unwrap()), "{text}");
            assert!(!requirement.matches(&"2.2.0".parse().unwrap()), "{text}");
        }

        let explicit_prerelease: Requirement = "~2 2.1.0-beta.1".parse().unwrap();
        assert!(explicit_prerelease.matches(&"2.1.0-beta.1".parse().unwrap()));
        assert!(!explicit_prerelease.matches(&"2.1.0-beta.2".parse().unwrap()));
    }

    #[test]
    fn partial_ranges_reject_noncanonical_components() {
        for requirement in ["02", "2.01", "2.", ".2", "2.1.0.0"] {
            assert!(matches!(
                requirement.parse::<Requirement>(),
                Err(ResolveError::UnsupportedRange(_))
            ));
        }
    }

    #[test]
    fn major_only_caret_range_selects_highest_matching_major() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", "3.0.0", &[]),
                package("foo", "3.9.0", &[]),
                package("foo", "4.0.0", &[]),
            ],
        );

        let r = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "^3")],
            &[m],
            Default::default(),
        )
        .unwrap();

        assert_eq!(
            r.selected[0].to_string(),
            "https://registry.npmjs.org:foo@3.9.0"
        );
    }

    #[test]
    fn zero_major_only_caret_range_uses_next_major_as_upper_bound() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", "0.0.1", &[]),
                package("foo", "0.9.0", &[]),
                package("foo", "1.0.0", &[]),
            ],
        );

        let r = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "^0")],
            &[m],
            Default::default(),
        )
        .unwrap();

        assert_eq!(
            r.selected[0].to_string(),
            "https://registry.npmjs.org:foo@0.9.0"
        );
    }

    #[test]
    fn resolves_peer_requirements_from_root_providers_without_installing_peer_as_root() {
        let mut plugin = package("plugin", "1.0.0", &[]);
        plugin.peer_dependencies = BTreeMap::from([("react".parse().unwrap(), req("^18.0.0"))]);
        let metadata = registry(
            "https://registry.npmjs.org",
            vec![plugin, package("react", "18.2.0", &[])],
        );
        let result = resolve_graph(
            &[
                dep("https://registry.npmjs.org", "plugin", "1.0.0"),
                dep("https://registry.npmjs.org", "react", "^18.0.0"),
            ],
            &[metadata],
            Default::default(),
        )
        .unwrap();
        assert_eq!(result.roots.len(), 2);
        assert_eq!(result.selected.len(), 2);
        let plugin_id = RegistryPackageId::new(
            "https://registry.npmjs.org".parse().unwrap(),
            "plugin".parse().unwrap(),
            "1.0.0".parse().unwrap(),
        );
        let expected_context = tapid_core::PeerContext::default()
            .with("react".parse().unwrap(), "18.2.0".parse().unwrap());
        assert_eq!(
            result.peer_contexts.get(&plugin_id),
            Some(&expected_context)
        );

        let mut missing = package("plugin", "1.0.0", &[]);
        missing.peer_dependencies = BTreeMap::from([("react".parse().unwrap(), req("^18.0.0"))]);
        let metadata = registry("https://registry.npmjs.org", vec![missing]);
        let error = resolve_graph(
            &[dep("https://registry.npmjs.org", "plugin", "1.0.0")],
            &[metadata],
            Default::default(),
        )
        .unwrap_err();
        assert!(matches!(error, ResolveError::PeerDependency { .. }));
        assert!(error.to_string().contains("peer dependency unresolved"));
    }

    #[test]
    fn optional_peer_dependency_allows_missing_direct_provider() {
        let mut plugin = package("plugin", "1.0.0", &[]);
        let peer: PackageName = "host".parse().unwrap();
        plugin.peer_dependencies = BTreeMap::from([(peer.clone(), req("^2.0.0"))]);
        plugin.optional_peer_dependencies = BTreeSet::from([peer]);
        let metadata = registry("https://registry.npmjs.org", vec![plugin]);
        let result = resolve_graph(
            &[dep("https://registry.npmjs.org", "plugin", "1.0.0")],
            &[metadata],
            Default::default(),
        )
        .unwrap();
        let plugin_id = RegistryPackageId::new(
            "https://registry.npmjs.org".parse().unwrap(),
            "plugin".parse().unwrap(),
            "1.0.0".parse().unwrap(),
        );
        assert_eq!(
            result.peer_contexts.get(&plugin_id),
            Some(&PeerContext::default())
        );
    }

    #[test]
    fn optional_peer_dependency_binds_a_compatible_direct_provider() {
        let mut plugin = package("plugin", "1.0.0", &[]);
        let peer: PackageName = "host".parse().unwrap();
        plugin.peer_dependencies = BTreeMap::from([(peer.clone(), req("^2.0.0"))]);
        plugin.optional_peer_dependencies = BTreeSet::from([peer]);
        let metadata = registry(
            "https://registry.npmjs.org",
            vec![plugin, package("host", "2.4.0", &[])],
        );
        let result = resolve_graph(
            &[
                dep("https://registry.npmjs.org", "plugin", "1.0.0"),
                dep("https://registry.npmjs.org", "host", "^2.0.0"),
            ],
            &[metadata],
            Default::default(),
        )
        .unwrap();
        let plugin_id = RegistryPackageId::new(
            "https://registry.npmjs.org".parse().unwrap(),
            "plugin".parse().unwrap(),
            "1.0.0".parse().unwrap(),
        );
        let expected_context =
            PeerContext::default().with("host".parse().unwrap(), "2.4.0".parse().unwrap());
        assert_eq!(
            result.peer_contexts.get(&plugin_id),
            Some(&expected_context)
        );
    }

    #[test]
    fn optional_peer_dependency_rejects_an_incompatible_direct_provider() {
        let mut plugin = package("plugin", "1.0.0", &[]);
        let peer: PackageName = "host".parse().unwrap();
        plugin.peer_dependencies = BTreeMap::from([(peer.clone(), req("^2.0.0"))]);
        plugin.optional_peer_dependencies = BTreeSet::from([peer]);
        let metadata = registry(
            "https://registry.npmjs.org",
            vec![plugin, package("host", "3.0.0", &[])],
        );
        let error = resolve_graph(
            &[
                dep("https://registry.npmjs.org", "plugin", "1.0.0"),
                dep("https://registry.npmjs.org", "host", "3.0.0"),
            ],
            &[metadata],
            Default::default(),
        )
        .unwrap_err();
        assert!(matches!(error, ResolveError::PeerDependency { .. }));
    }

    #[test]
    fn preserves_peer_requirements_separately_from_ordinary_dependencies() {
        let peer: PackageVersionMetadata = PackageVersionMetadata {
            name: "plugin".parse().unwrap(),
            version: "1.0.0".parse().unwrap(),
            dependencies: BTreeMap::from([("runtime".parse().unwrap(), req("^1.0.0"))]),
            peer_dependencies: BTreeMap::from([("react".parse().unwrap(), req("^18.0.0"))]),
            optional_peer_dependencies: BTreeSet::new(),
        };

        assert!(peer.dependencies.contains_key(&"runtime".parse().unwrap()));
        assert!(!peer.dependencies.contains_key(&"react".parse().unwrap()));
        assert_eq!(
            peer.peer_dependencies[&"react".parse().unwrap()].raw,
            "^18.0.0"
        );
    }

    #[test]
    fn available_versions_use_semver_order_before_rendering() {
        let first = PackageVersionMetadata {
            name: "pkg".parse().unwrap(),
            version: "10.0.0".parse().unwrap(),
            dependencies: BTreeMap::new(),
            peer_dependencies: BTreeMap::new(),
            optional_peer_dependencies: BTreeSet::new(),
        };
        let second = PackageVersionMetadata {
            name: "pkg".parse().unwrap(),
            version: "2.0.0".parse().unwrap(),
            dependencies: BTreeMap::new(),
            peer_dependencies: BTreeMap::new(),
            optional_peer_dependencies: BTreeSet::new(),
        };

        assert_eq!(available(&[&first, &second]), vec!["2.0.0", "10.0.0"]);
    }

    #[test]
    fn missing_metadata_is_reported_as_a_sorted_frontier() {
        let registry: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let app: PackageName = "app".parse().unwrap();
        let metadata = RegistryMetadata::normalize(
            registry.clone(),
            vec![PackageVersionMetadata {
                name: app.clone(),
                version: "1.0.0".parse().unwrap(),
                dependencies: BTreeMap::from([
                    ("z-child".parse().unwrap(), req("1.0.0")),
                    ("a-child".parse().unwrap(), req("1.0.0")),
                ]),
                peer_dependencies: BTreeMap::new(),
                optional_peer_dependencies: BTreeSet::new(),
            }],
        )
        .unwrap();

        let error = resolve_graph(
            &[Dependency::new(registry, app, req("1.0.0"))],
            &[metadata],
            ResolutionOptions::default(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            ResolveError::MissingMetadata {
                packages: vec![
                    (
                        "https://registry.npmjs.org".to_owned(),
                        "a-child".to_owned()
                    ),
                    (
                        "https://registry.npmjs.org".to_owned(),
                        "z-child".to_owned()
                    ),
                ],
            }
        );
    }

    #[test]
    fn major_only_caret_range_rejects_leading_zeroes() {
        assert!(matches!(
            "^03".parse::<Requirement>(),
            Err(ResolveError::UnsupportedRange(_))
        ));
    }

    #[test]
    fn npm_and_jsr_registries_remain_distinct() {
        let npm = registry(
            "https://registry.npmjs.org",
            vec![package("foo", "1.0.0", &[])],
        );
        let jsr = registry("https://jsr.io", vec![package("foo", "1.0.0", &[])]);
        let result = resolve_graph(
            &[
                dep("https://registry.npmjs.org", "foo", "1.0.0"),
                dep("https://jsr.io", "foo", "1.0.0"),
            ],
            &[npm, jsr],
            Default::default(),
        )
        .unwrap();
        assert_eq!(result.selected[0].registry.to_string(), "https://jsr.io");
        assert_eq!(
            result.selected[1].registry.to_string(),
            "https://registry.npmjs.org"
        );
    }

    #[test]
    fn shuffled_metadata_normalizes_and_tilde_is_supported() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", "1.2.1", &[]),
                package("foo", "1.2.9", &[]),
                package("foo", "1.3.0", &[]),
            ],
        );
        let result = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "~1.2.0")],
            &[m],
            Default::default(),
        )
        .unwrap();
        assert_eq!(result.selected[0].version.to_string(), "1.2.9");
    }

    #[test]
    fn transitive_dependencies_and_cycles_are_finite_and_sorted() {
        let m = registry(
            "https://jsr.io",
            vec![
                package("a", "1.0.0", &[("b", "1.0.0")]),
                package("b", "1.0.0", &[("a", "1.0.0")]),
            ],
        );
        let result = resolve_graph(
            &[dep("https://jsr.io", "a", "1.0.0")],
            &[m],
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            result
                .selected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["https://jsr.io:a@1.0.0", "https://jsr.io:b@1.0.0"]
        );
    }

    #[test]
    fn different_parents_can_select_different_versions_of_one_dependency() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![
                package("a", "1.0.0", &[("debug", "^3.0.0")]),
                package("b", "1.0.0", &[("debug", "^4.0.0")]),
                package("debug", "3.2.7", &[]),
                package("debug", "4.3.7", &[]),
            ],
        );

        let result = resolve_graph(
            &[
                dep("https://registry.npmjs.org", "a", "*"),
                dep("https://registry.npmjs.org", "b", "*"),
            ],
            &[m],
            Default::default(),
        )
        .unwrap();

        assert_eq!(
            result
                .selected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "https://registry.npmjs.org:a@1.0.0",
                "https://registry.npmjs.org:b@1.0.0",
                "https://registry.npmjs.org:debug@3.2.7",
                "https://registry.npmjs.org:debug@4.3.7",
            ]
        );
    }

    #[test]
    fn incompatible_constraints_are_structured_and_deterministic() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![package("foo", "1.0.0", &[]), package("foo", "2.0.0", &[])],
        );
        let result = resolve_graph(
            &[
                dep("https://registry.npmjs.org", "foo", "^1.0.0"),
                dep("https://registry.npmjs.org", "foo", "^2.0.0"),
            ],
            &[m],
            Default::default(),
        );
        assert!(
            matches!(result, Err(ResolveError::Conflict { requirements, .. }) if requirements == vec!["^1.0.0", "^2.0.0"])
        );
    }

    #[test]
    fn npm_zero_major_caret_bounds_are_respected() {
        let m = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", "0.2.3", &[]),
                package("foo", "0.2.9", &[]),
                package("foo", "0.3.0", &[]),
            ],
        );
        let result = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", "^0.2.3")],
            &[m],
            Default::default(),
        )
        .unwrap();
        assert_eq!(result.selected[0].version.to_string(), "0.2.9");
    }

    #[test]
    fn caret_ranges_at_integer_bounds_fail_closed_without_panicking() {
        let max = u64::MAX;
        let m = registry(
            "https://registry.npmjs.org",
            vec![package("foo", &format!("{max}.0.0"), &[])],
        );
        let result = resolve_graph(
            &[dep(
                "https://registry.npmjs.org",
                "foo",
                &format!("^{max}.0.0"),
            )],
            &[m],
            Default::default(),
        )
        .unwrap();
        assert_eq!(result.selected[0].version.to_string(), format!("{max}.0.0"));
    }

    #[test]
    fn zero_major_caret_ranges_at_integer_bounds_fail_closed_without_panicking() {
        let max = u64::MAX;
        for (version, requirement) in [
            (format!("0.{max}.0"), format!("^0.{max}.0")),
            (format!("0.0.{max}"), format!("^0.0.{max}")),
        ] {
            let m = registry(
                "https://registry.npmjs.org",
                vec![
                    package("foo", &version, &[]),
                    package(
                        "foo",
                        if requirement.starts_with("^0.")
                            && requirement.contains(&format!(".{max}."))
                        {
                            "1.0.0"
                        } else {
                            "0.1.0"
                        },
                        &[],
                    ),
                ],
            );
            let result = resolve_graph(
                &[dep("https://registry.npmjs.org", "foo", &requirement)],
                &[m],
                Default::default(),
            )
            .unwrap();
            assert_eq!(result.selected[0].version.to_string(), version);
        }
    }

    #[test]
    fn unsupported_ranges_and_modes_fail_closed() {
        assert!(matches!(
            "!=1.0.0".parse::<Requirement>(),
            Err(ResolveError::UnsupportedRange(_))
        ));
        assert!(matches!(
            resolve_graph(
                &[],
                &[],
                ResolutionOptions {
                    offline: true,
                    frozen: false
                }
            ),
            Err(ResolveError::UnsupportedMode(_))
        ));
        assert!(matches!(
            resolve_graph(
                &[],
                &[],
                ResolutionOptions {
                    offline: false,
                    frozen: true
                }
            ),
            Err(ResolveError::UnsupportedMode(_))
        ));
    }
}
