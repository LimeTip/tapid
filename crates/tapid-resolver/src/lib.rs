//! Pure deterministic resolver.
#![deny(unsafe_code)]
use std::{fmt, str::FromStr};
use tapid_core::{PackageName, PackageVersion, RegistryOrigin};
use tapid_registry_client::{PackageMetadata, RegistryPackageId, RegistrySnapshot};
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
        if s.trim().is_empty()
            || s.split_whitespace().any(|t| {
                t.trim_start_matches(['^', '~', '>', '<', '='])
                    .parse::<PackageVersion>()
                    .is_err()
            })
        {
            Err(ResolveError::InvalidRequirement(s.into()))
        } else {
            Ok(Self {
                raw: s.trim().into(),
            })
        }
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
    UnsupportedMode(&'static str),
    MissingCandidate {
        registry: String,
        name: String,
        requirement: String,
        available: Vec<String>,
    },
}
impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ResolveError {}
pub fn resolve(
    ds: &[Dependency],
    ss: &[RegistrySnapshot],
    options: ResolutionOptions,
) -> Result<Resolution, ResolveError> {
    if options.offline {
        return Err(ResolveError::UnsupportedMode(
            "offline resolution requires a cached snapshot",
        ));
    }
    if options.frozen {
        return Err(ResolveError::UnsupportedMode(
            "frozen resolution requires a lockfile replay input",
        ));
    }
    let mut out = Vec::new();
    for d in ds {
        let mut c: Vec<&PackageMetadata> = ss
            .iter()
            .filter(|s| s.registry() == &d.registry)
            .flat_map(|s| s.candidates(&d.name))
            .filter(|p| ok(p.identity.version, &d.requirement.raw))
            .collect();
        c.sort_by(|left, right| {
            right
                .identity
                .version
                .cmp(&left.identity.version)
                .then_with(|| left.identity.cmp(&right.identity))
                .then_with(|| {
                    left.integrity
                        .as_ref()
                        .map(ToString::to_string)
                        .cmp(&right.integrity.as_ref().map(ToString::to_string))
                })
        });
        if let Some(p) = c.first() {
            out.push(p.identity.clone())
        } else {
            return Err(ResolveError::MissingCandidate {
                registry: d.registry.to_string(),
                name: d.name.to_string(),
                requirement: d.requirement.raw.clone(),
                available: {
                    let mut available: Vec<_> = ss
                        .iter()
                        .filter(|s| s.registry() == &d.registry)
                        .flat_map(|s| s.candidates(&d.name))
                        .map(|p| p.identity.version.to_string())
                        .collect();
                    available.sort();
                    available.dedup();
                    available
                },
            });
        }
    }
    Ok(Resolution { selected: out })
}
fn ok(v: PackageVersion, r: &str) -> bool {
    r.split_whitespace().all(|t| {
        let (op, x) = if let Some(x) = t.strip_prefix('^') {
            ('^', x)
        } else if let Some(x) = t.strip_prefix('~') {
            ('~', x)
        } else if let Some(x) = t.strip_prefix(">=") {
            ('G', x)
        } else if let Some(x) = t.strip_prefix("<=") {
            ('L', x)
        } else if let Some(x) = t.strip_prefix('>') {
            ('>', x)
        } else if let Some(x) = t.strip_prefix('<') {
            ('<', x)
        } else {
            ('=', t.trim_start_matches('='))
        };
        let Ok(b) = x.parse() else { return false };
        match op {
            '=' => v == b,
            '^' => v >= b && v.major == b.major,
            '~' => v >= b && v.major == b.major && v.minor == b.minor,
            'G' => v >= b,
            'L' => v <= b,
            '>' => v > b,
            '<' => v < b,
            _ => false,
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use tapid_registry_client::*;
    fn s(v: Vec<&str>) -> RegistrySnapshot {
        RegistrySnapshot::normalize(RawRegistrySnapshot {
            registry: "https://x".into(),
            packages: v
                .into_iter()
                .map(|x| RawPackageMetadata {
                    name: "foo".into(),
                    version: x.into(),
                    integrity: None,
                    artifact: None,
                })
                .collect(),
        })
        .unwrap()
    }
    fn d(r: &str) -> Dependency {
        Dependency::new(
            "https://x".parse().unwrap(),
            "foo".parse().unwrap(),
            r.parse().unwrap(),
        )
    }
    #[test]
    fn exact() {
        assert_eq!(
            resolve(&[d("1.0.0")], &[s(vec!["1.0.0"])], Default::default())
                .unwrap()
                .selected[0]
                .version
                .to_string(),
            "1.0.0"
        )
    }
    #[test]
    fn range() {
        assert_eq!(
            resolve(
                &[d("^1.0.0")],
                &[s(vec!["1.1.0", "1.9.0", "2.0.0"])],
                Default::default()
            )
            .unwrap()
            .selected[0]
                .version
                .to_string(),
            "1.9.0"
        )
    }
    #[test]
    fn missing() {
        assert!(matches!(
            resolve(&[d("2.0.0")], &[s(vec!["1.0.0"])], Default::default()),
            Err(ResolveError::MissingCandidate { .. })
        ))
    }
}
