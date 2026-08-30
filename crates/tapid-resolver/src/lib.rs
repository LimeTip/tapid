//! Pure, deterministic resolution of normalized registry metadata.
#![deny(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};
use tapid_core::{PackageName, PackageVersion, RegistryOrigin};
use tapid_registry_client::{RegistryPackageId, RegistrySnapshot};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Requirement {
    pub raw: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    pub registry: RegistryOrigin,
    pub name: PackageName,
    pub requirement: Requirement,
}
impl Dependency {
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
        if raw != "*" {
            for token in raw.split_whitespace() {
                let (op, value) = requirement_token(token);
                if token.starts_with(['>', '<']) || parse_requirement_base(op, value).is_none() {
                    return Err(ResolveError::UnsupportedRange(raw.into()));
                }
            }
        }
        Ok(Self { raw: raw.into() })
    }
}

fn requirement_token(token: &str) -> (char, &str) {
    if let Some(value) = token.strip_prefix('^') {
        ('^', value)
    } else if let Some(value) = token.strip_prefix('~') {
        ('~', value)
    } else {
        ('=', token.trim_start_matches('='))
    }
}

#[derive(Clone, Copy)]
struct RequirementBase {
    version: PackageVersion,
    major_only: bool,
}

fn parse_requirement_base(op: char, value: &str) -> Option<RequirementBase> {
    value
        .parse::<PackageVersion>()
        .ok()
        .map(|version| RequirementBase {
            version,
            major_only: false,
        })
        .or_else(|| {
            if op != '^'
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
                || (value.len() > 1 && value.starts_with('0'))
            {
                return None;
            }
            value.parse().ok().map(|major| RequirementBase {
                version: PackageVersion {
                    major,
                    minor: 0,
                    patch: 0,
                },
                major_only: true,
            })
        })
}

/// Normalized metadata supplied by a registry adapter. The resolver never fetches it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageVersionMetadata {
    pub name: PackageName,
    pub version: PackageVersion,
    pub dependencies: BTreeMap<PackageName, Requirement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryMetadata {
    pub registry: RegistryOrigin,
    pub packages: Vec<PackageVersionMetadata>,
}
impl RegistryMetadata {
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
    fn candidates(&self, name: &PackageName) -> impl Iterator<Item = &PackageVersionMetadata> {
        self.packages.iter().filter(move |p| &p.name == name)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolutionOptions {
    pub offline: bool,
    pub frozen: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolution {
    pub selected: Vec<RegistryPackageId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    InvalidRequirement(String),
    UnsupportedRange(String),
    UnsupportedMode(&'static str),
    DuplicateMetadata {
        package: String,
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
    let mut constraints: BTreeMap<(RegistryOrigin, PackageName), BTreeSet<String>> =
        BTreeMap::new();
    let mut selected = BTreeMap::new();
    let mut queue = ds.to_vec();
    while let Some(dep) = queue.pop() {
        let key = (dep.registry.clone(), dep.name.clone());
        constraints
            .entry(key.clone())
            .or_default()
            .insert(dep.requirement.raw.clone());
        let reqs: Vec<_> = constraints[&key].iter().cloned().collect();
        let candidates: Vec<_> = metadata
            .iter()
            .filter(|m| m.registry == dep.registry)
            .flat_map(|m| m.candidates(&dep.name))
            .filter(|p| reqs.iter().all(|r| matches_requirement(p.version, r)))
            .collect();
        let available = available(metadata, &dep.registry, &dep.name);
        let Some(package) = candidates
            .into_iter()
            .max_by(|a, b| a.version.cmp(&b.version))
        else {
            return Err(if reqs.len() > 1 {
                ResolveError::Conflict {
                    registry: dep.registry.to_string(),
                    name: dep.name.to_string(),
                    requirements: reqs,
                    available,
                }
            } else {
                ResolveError::MissingCandidate {
                    registry: dep.registry.to_string(),
                    name: dep.name.to_string(),
                    requirement: dep.requirement.raw,
                    available,
                }
            });
        };
        let id = RegistryPackageId::new(dep.registry.clone(), dep.name.clone(), package.version);
        let changed = selected.get(&key) != Some(&id);
        selected.insert(key, id);
        if changed {
            queue.extend(package.dependencies.iter().map(|(name, requirement)| {
                Dependency::new(dep.registry.clone(), name.clone(), requirement.clone())
            }));
        }
    }
    Ok(Resolution {
        selected: selected.into_values().collect(),
    })
}

fn available(
    metadata: &[RegistryMetadata],
    registry: &RegistryOrigin,
    name: &PackageName,
) -> Vec<String> {
    let mut out: Vec<_> = metadata
        .iter()
        .filter(|m| &m.registry == registry)
        .flat_map(|m| m.candidates(name))
        .map(|p| p.version.to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}
fn matches_requirement(version: PackageVersion, raw: &str) -> bool {
    if raw.trim() == "*" {
        return true;
    }
    raw.split_whitespace().all(|token| {
        let (op, value) = requirement_token(token);
        let Some(parsed_base) = parse_requirement_base(op, value) else {
            return false;
        };
        let base = parsed_base.version;
        match op {
            '=' => version == base,
            '^' => {
                if parsed_base.major_only {
                    return base
                        .major
                        .checked_add(1)
                        .map(|major| {
                            let upper = PackageVersion {
                                major,
                                minor: 0,
                                patch: 0,
                            };
                            version >= base && version < upper
                        })
                        .unwrap_or(version >= base);
                }
                if base.major > 0 {
                    return base
                        .major
                        .checked_add(1)
                        .map(|major| {
                            let upper = PackageVersion {
                                major,
                                minor: 0,
                                patch: 0,
                            };
                            version >= base && version < upper
                        })
                        .unwrap_or(version >= base);
                }
                if base.minor > 0 {
                    return base
                        .minor
                        .checked_add(1)
                        .map(|minor| {
                            let upper = PackageVersion {
                                major: 0,
                                minor,
                                patch: 0,
                            };
                            version >= base && version < upper
                        })
                        .unwrap_or(
                            version >= base && version.major == 0 && version.minor == base.minor,
                        );
                }
                base.patch
                    .checked_add(1)
                    .map(|patch| {
                        let upper = PackageVersion {
                            major: 0,
                            minor: 0,
                            patch,
                        };
                        version >= base && version < upper
                    })
                    .unwrap_or(version == base)
            }
            '~' => version >= base && version.major == base.major && version.minor == base.minor,
            _ => false,
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
                    version: p.identity.version,
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
            "<1.0.0".parse::<Requirement>(),
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
