//! Allowlisted error evidence. Human messages and source chains are never wire data.
use serde::Serialize;
use std::{
    error::Error,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct DiagnosticPackage {
    pub(crate) source: String,
    pub(crate) name: String,
    pub(crate) version: Option<String>,
}

impl From<&tapid_core::PackageInstanceId> for DiagnosticPackage {
    fn from(id: &tapid_core::PackageInstanceId) -> Self {
        Self {
            source: id.registry.to_string(),
            name: id.name.to_string(),
            version: Some(id.version.to_string()),
        }
    }
}

impl From<&tapid_lockfile::LockfilePackageKey> for DiagnosticPackage {
    fn from(key: &tapid_lockfile::LockfilePackageKey) -> Self {
        use tapid_lockfile::LockfilePackageSource;
        let source = match &key.source {
            LockfilePackageSource::Registry(source) => source.to_string(),
            LockfilePackageSource::Copied(source) => source.to_string(),
            LockfilePackageSource::Workspace(source) => format!("workspace:{}", source.path()),
        };
        Self {
            source,
            name: key.name.to_string(),
            version: Some(key.version.to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct Declaration {
    pub(crate) origin: &'static str,
    pub(crate) pointer: String,
    pub(crate) workspace: Option<String>,
    #[serde(skip)]
    pub(crate) manifest: Option<PathBuf>,
}

impl Declaration {
    pub(crate) fn metadata(section: &str, name: &str) -> Self {
        Self {
            origin: "metadata",
            pointer: Self::pointer(section, name),
            workspace: None,
            manifest: None,
        }
    }

    pub(crate) fn manifest(path: &Path, pointer: String) -> Self {
        Self {
            origin: "manifest",
            pointer,
            workspace: None,
            manifest: Some(path.to_owned()),
        }
    }

    pub(crate) fn pointer(section: &str, name: &str) -> String {
        // Redact before escaping: escaped slashes would hide URL credentials
        // from the common output sanitizer.
        let escape = |value: &str| super::sanitize(value).replace('~', "~0").replace('/', "~1");
        format!("/{}/{}", escape(section), escape(name))
    }

    pub(crate) fn selects_resolver_root(&self) -> bool {
        self.origin == "manifest" && !self.pointer.starts_with("/peerDependencies/")
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct RequirementDiagnostic {
    pub(crate) local_name: Option<String>,
    pub(crate) requirement: String,
    pub(crate) package: Option<DiagnosticPackage>,
    pub(crate) declaration: Option<Declaration>,
}

impl RequirementDiagnostic {
    fn from_origin(origin: &tapid_resolver::RequirementOrigin, requirement: &str) -> Self {
        Self {
            local_name: Some(origin.name.to_string()),
            requirement: requirement.to_owned(),
            package: origin.package.as_deref().map(Into::into),
            declaration: origin
                .package
                .as_ref()
                .map(|_| Declaration::metadata("dependencies", origin.name.as_str())),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct PeerDiagnostic {
    pub(crate) name: String,
    pub(crate) requirement: String,
    pub(crate) status: &'static str,
    pub(crate) provider: Option<DiagnosticPackage>,
    pub(crate) declaration: Option<Declaration>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Comparison {
    pub(crate) kind: &'static str,
    pub(crate) algorithm: Option<&'static str>,
    pub(crate) expected: String,
    pub(crate) observed: String,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct DiagnosticContext {
    pub(crate) package: Option<DiagnosticPackage>,
    pub(crate) declaration: Option<Declaration>,
    pub(crate) peer: Option<PeerDiagnostic>,
    pub(crate) comparison: Option<Comparison>,
    pub(crate) requirements: Vec<RequirementDiagnostic>,
    pub(crate) available: Vec<String>,
}

impl DiagnosticContext {
    pub(crate) fn manifest_error(
        evidence: &tapid_manifest::ManifestDiagnostic,
        path: &Path,
    ) -> Self {
        let pointer = match (evidence.field, evidence.key.as_deref()) {
            (Some(field), Some(key)) => Declaration::pointer(field, key),
            (Some(field), None) => format!("/{field}"),
            (None, _) => String::new(),
        };
        let mut diagnostic =
            Self::comparison("manifest_value", None, evidence.expected, evidence.observed);
        diagnostic.declaration = Some(Declaration::manifest(path, pointer));
        diagnostic
    }

    pub(crate) fn manifest_requirements(
        manifest: &tapid_manifest::PackageManifest,
        path: &Path,
        workspace: Option<&str>,
    ) -> Vec<RequirementDiagnostic> {
        crate::application::lifecycle::direct_dependencies(manifest)
            .into_iter()
            .map(|(name, requirement, kind)| {
                let mut declaration = Declaration::manifest(
                    path,
                    Declaration::pointer(crate::application::lifecycle::kind_name(kind), &name),
                );
                declaration.workspace = workspace.map(str::to_owned);
                let local_name = crate::online::dep_parts(&name)
                    .map(|(_, name)| name.to_string())
                    .unwrap_or(name);
                RequirementDiagnostic {
                    local_name: Some(local_name),
                    requirement,
                    package: None,
                    declaration: Some(declaration),
                }
            })
            .collect()
    }

    pub(crate) fn comparison(
        kind: &'static str,
        algorithm: Option<&'static str>,
        expected: impl ToString,
        observed: impl ToString,
    ) -> Self {
        Self {
            comparison: Some(Comparison {
                kind,
                algorithm,
                expected: expected.to_string(),
                observed: observed.to_string(),
            }),
            ..Self::default()
        }
    }

    pub(crate) fn from_error(error: &(dyn Error + 'static)) -> Option<Self> {
        if let Some(tapid_manifest::WorkspaceError::Manifest {
            path, diagnostic, ..
        }) = error.downcast_ref()
        {
            return Some(Self::manifest_error(diagnostic, path));
        }
        use tapid_resolver::ResolveError;
        if let Some(error) = error.downcast_ref::<ResolveError>() {
            let mut context = Self::default();
            match error {
                ResolveError::PeerDependency {
                    package,
                    peer,
                    requirement,
                    provider,
                } => {
                    context.package = Some(package.as_ref().into());
                    context.declaration = Some(Declaration::metadata("peerDependencies", peer));
                    context.comparison = Self::comparison(
                        "peer_requirement",
                        None,
                        requirement,
                        provider
                            .as_ref()
                            .map(|id| id.version.to_string())
                            .unwrap_or_else(|| "missing".into()),
                    )
                    .comparison;
                    context.peer = Some(PeerDiagnostic {
                        name: peer.clone(),
                        requirement: requirement.clone(),
                        status: if provider.is_some() {
                            "incompatible"
                        } else {
                            "missing"
                        },
                        provider: provider.as_deref().map(Into::into),
                        declaration: None,
                    });
                }
                ResolveError::MissingCandidate {
                    registry,
                    name,
                    requirement,
                    available,
                    origin,
                } => {
                    context.package = Some(DiagnosticPackage {
                        source: registry.clone(),
                        name: name.clone(),
                        version: None,
                    });
                    context.available = available.clone();
                    context
                        .requirements
                        .push(RequirementDiagnostic::from_origin(origin, requirement));
                }
                ResolveError::Conflict {
                    registry,
                    name,
                    requirements,
                    available,
                    origin,
                } => {
                    context.package = Some(DiagnosticPackage {
                        source: registry.clone(),
                        name: name.clone(),
                        version: None,
                    });
                    context.available = available.clone();
                    context.requirements = requirements
                        .iter()
                        .map(|requirement| RequirementDiagnostic::from_origin(origin, requirement))
                        .collect();
                }
                _ => return None,
            }
            return Some(context);
        }
        if let Some(tapid_lockfile::LockfileError::RootManifestDigestMismatch {
            expected,
            actual,
        }) = error.downcast_ref()
        {
            return Some(Self::comparison(
                "root_manifest_digest",
                Some("sha256"),
                expected,
                actual,
            ));
        }
        if let Some(error) = error.downcast_ref::<tapid_store::IngestError>() {
            return match error {
                tapid_store::IngestError::DigestMismatch { expected, actual } => Some(
                    Self::comparison("artifact_digest", Some("sha256"), expected, actual),
                ),
                tapid_store::IngestError::TreeDigestMismatch { expected, actual } => Some(
                    Self::comparison("tree_digest", Some("sha256"), expected, actual),
                ),
                _ => None,
            };
        }
        None
    }

    /// Attach declarations already parsed by the operation, never reread after rollback.
    pub(crate) fn locate_requirements(&mut self, declarations: &[RequirementDiagnostic]) {
        let mut located = Vec::new();
        for requirement in &self.requirements {
            let matches = declarations
                .iter()
                .filter(|candidate| {
                    candidate.local_name == requirement.local_name
                        && candidate.requirement == requirement.requirement
                        && candidate
                            .declaration
                            .as_ref()
                            .is_some_and(Declaration::selects_resolver_root)
                })
                .collect::<Vec<_>>();
            if requirement.declaration.is_none() && !matches.is_empty() {
                located.extend(matches.into_iter().cloned());
            } else {
                located.push(requirement.clone());
            }
        }
        self.requirements = located;
        if let Some(peer) = &mut self.peer {
            peer.declaration = declarations
                .iter()
                .find(|candidate| {
                    candidate.local_name.as_deref() == Some(&peer.name)
                        && peer.provider.is_some()
                        && candidate
                            .declaration
                            .as_ref()
                            .is_some_and(Declaration::selects_resolver_root)
                })
                .and_then(|candidate| candidate.declaration.clone());
        }
    }
}
