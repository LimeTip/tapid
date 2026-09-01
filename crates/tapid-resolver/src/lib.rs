//! Pure, deterministic resolution of normalized registry metadata.
#![deny(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};
use tapid_core::{PackageName, PackageVersion, RegistryOrigin};
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

/// A validated dependency requirement in Tapid's supported npm range subset.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Requirement {
    /// Canonical trimmed source requirement used for deterministic diagnostics.
    pub raw: String,
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
        if raw.is_empty() {
            return Err(ResolveError::InvalidRequirement(s.into()));
        }
        let mut clauses = Vec::new();
        for clause in raw.split("||") {
            let clause = clause.trim();
            if clause.is_empty() {
                return Err(ResolveError::UnsupportedRange(raw.into()));
            }
            if clause == "*" {
                clauses.push(RequirementClause::AnyStable);
                continue;
            }
            let mut comparators = Vec::new();
            let tokens = clause.split_whitespace().collect::<Vec<_>>();
            let mut index = 0;
            while index < tokens.len() {
                let token = tokens[index];
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
            clauses,
        })
    }
}

impl Requirement {
    /// Returns whether an exact version satisfies this validated requirement.
    pub fn matches(&self, version: &PackageVersion) -> bool {
        matches_requirement(version, self)
    }
}

fn separated_operator(token: &str) -> Option<RequirementOperator> {
    match token {
        "~" => Some(RequirementOperator::Tilde),
        ">" => Some(RequirementOperator::Greater),
        ">=" => Some(RequirementOperator::GreaterEqual),
        "<" => Some(RequirementOperator::Less),
        "<=" => Some(RequirementOperator::LessEqual),
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

    value
        .parse::<PackageVersion>()
        .ok()
        .map(|version| RequirementBase {
            version,
            precision: RequirementPrecision::Full,
        })
        .or_else(|| {
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
                ) && !(op == RequirementOperator::Caret && components.len() == 1))
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
    /// Exact dependency edges used by lockfile and linker construction.
    pub dependencies: Vec<ResolvedDependency>,
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
}
impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ResolveError {}

/// Resolve a transitive graph from normalized metadata. No network or filesystem access occurs.
pub fn resolve_graph(
    ds: &[Dependency],
    metadata: &[RegistryMetadata],
    options: ResolutionOptions,
) -> Result<Resolution, ResolveError> {
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
    let mut root_constraints: BTreeMap<(RegistryOrigin, PackageName), BTreeSet<Requirement>> =
        BTreeMap::new();
    for dependency in ds {
        root_constraints
            .entry((dependency.registry.clone(), dependency.name.clone()))
            .or_default()
            .insert(dependency.requirement.clone());
    }

    let mut selected = BTreeSet::new();
    let mut selected_packages = BTreeMap::new();
    let mut roots = Vec::new();
    let mut queue = Vec::new();
    let mut missing_metadata = BTreeSet::new();
    for ((registry, name), requirements) in root_constraints {
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
        let id = RegistryPackageId::new(registry, name, package.version.clone());
        selected.insert(id.clone());
        selected_packages.insert(id.clone(), package);
        roots.push(id.clone());
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
            let requirements = BTreeSet::from([requirement]);
            let child_package = match select_package(
                &parent.registry,
                &dependency,
                &requirements,
                &candidate_index,
            ) {
                Ok(package) => package,
                Err(ResolveError::MissingCandidate { .. })
                    if !candidate_index
                        .contains_key(&(parent.registry.clone(), dependency.clone())) =>
                {
                    missing_metadata.insert((parent.registry.to_string(), dependency.to_string()));
                    continue;
                }
                Err(error) => return Err(error),
            };
            let child = RegistryPackageId::new(
                parent.registry.clone(),
                dependency.clone(),
                child_package.version.clone(),
            );
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

    Ok(Resolution {
        selected: selected.into_iter().collect(),
        roots,
        dependencies: dependencies.into_iter().collect(),
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
                        RequirementOperator::Caret => {
                            if comparator.base.precision == RequirementPrecision::Major {
                                return base
                                    .major()
                                    .checked_add(1)
                                    .map(|major| {
                                        let upper = PackageVersion::stable(major, 0, 0);
                                        version >= base && version < &upper
                                    })
                                    .unwrap_or(version >= base);
                            }
                            if base.major() > 0 {
                                return base
                                    .major()
                                    .checked_add(1)
                                    .map(|major| {
                                        let upper = PackageVersion::stable(major, 0, 0);
                                        version >= base && version < &upper
                                    })
                                    .unwrap_or(version >= base);
                            }
                            if base.minor() > 0 {
                                return base
                                    .minor()
                                    .checked_add(1)
                                    .map(|minor| {
                                        let upper = PackageVersion::stable(0, minor, 0);
                                        version >= base && version < &upper
                                    })
                                    .unwrap_or(
                                        version >= base
                                            && version.major() == 0
                                            && version.minor() == base.minor(),
                                    );
                            }
                            base.patch()
                                .checked_add(1)
                                .map(|patch| {
                                    let upper = PackageVersion::stable(0, 0, patch);
                                    version >= base && version < &upper
                                })
                                .unwrap_or(version == base)
                        }
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
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    resolve_graph(ds, &metadata, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn req(s: &str) -> Requirement {
        s.parse().unwrap()
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
    fn available_versions_use_semver_order_before_rendering() {
        let first = PackageVersionMetadata {
            name: "pkg".parse().unwrap(),
            version: "10.0.0".parse().unwrap(),
            dependencies: BTreeMap::new(),
        };
        let second = PackageVersionMetadata {
            name: "pkg".parse().unwrap(),
            version: "2.0.0".parse().unwrap(),
            dependencies: BTreeMap::new(),
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
