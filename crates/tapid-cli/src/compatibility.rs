//! Compatibility facts are observations, separate from installation success.
use serde::Serialize;
use tapid_manifest::{NodeEngineDeclaration, PackageManifest};

#[derive(Debug, Default, Serialize)]
pub(crate) struct Report {
    pub(crate) facts: Vec<Fact>,
    pub(crate) evidence_complete: bool,
    pub(crate) strict_satisfied: bool,
    pub(crate) runtime: Runtime,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Runtime {
    state: RuntimeState,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeState {
    #[default]
    NotRequested,
}

#[derive(Debug, Serialize)]
pub(crate) struct Fact {
    pub(crate) location: String,
    pub(crate) identity: String,
    pub(crate) provenance: &'static str,
    pub(crate) classification: &'static str,
    pub(crate) status: &'static str,
    pub(crate) reason: &'static str,
    pub(crate) requirement: Option<String>,
}

impl Report {
    pub(crate) fn project(manifest: &PackageManifest) -> Self {
        let mut report = Self {
            evidence_complete: true,
            strict_satisfied: true,
            ..Self::default()
        };
        report.manifest(manifest, "package.json");
        report
    }

    pub(crate) fn manifest(&mut self, manifest: &PackageManifest, path: &str) {
        let (classification, status, reason) = node_evaluation(manifest.node_engine());
        let requirement = match manifest.node_engine() {
            NodeEngineDeclaration::Range(range) => Some(range),
            _ => None,
        };
        self.push(
            path,
            "engines/node",
            classification,
            status,
            reason,
            requirement,
        );
        let (classification, status, reason) = match manifest.package_manager() {
            None => ("absent", "not_declared", "no_declaration"),
            Some(value) => match value
                .as_str()
                .and_then(|s| s.strip_prefix("tapid@"))
                .and_then(|v| v.parse::<tapid_core::PackageVersion>().ok())
            {
                Some(version) if version.to_string() == env!("CARGO_PKG_VERSION") => {
                    ("exact_tapid", "satisfied", "manager_version_matches")
                }
                Some(_) => ("exact_tapid", "incompatible", "manager_version_mismatch"),
                None => ("unsupported", "unsupported", "unsupported_package_manager"),
            },
        };
        self.push(
            path,
            "packageManager",
            classification,
            status,
            reason,
            manifest
                .package_manager()
                .and_then(serde_json::Value::as_str),
        );
        let (classification, status, reason) = if manifest.dev_engines().is_some() {
            ("unsupported", "unsupported", "unsupported_dev_engines")
        } else {
            ("absent", "not_declared", "no_declaration")
        };
        self.push(path, "devEngines", classification, status, reason, None);
    }

    fn push(
        &mut self,
        path: &str,
        field: &str,
        classification: &'static str,
        status: &'static str,
        reason: &'static str,
        requirement: Option<&str>,
    ) {
        self.strict_satisfied &= matches!(status, "satisfied" | "not_declared");
        self.facts.push(Fact {
            location: format!("{path}#/{field}"),
            identity: path.into(),
            provenance: "project_manifest",
            requirement: requirement.map(str::to_owned),
            classification,
            status,
            reason,
        });
    }

    pub(crate) fn selected_package(
        &mut self,
        key: &str,
        requirement: &tapid_registry_client::NodeEngineRequirement,
        provenance: &'static str,
    ) {
        use tapid_registry_client::NodeEngineRequirement as R;
        let declaration = match requirement {
            R::Unknown => {
                self.unknown_package(key);
                return;
            }
            R::NotDeclared => NodeEngineDeclaration::NotDeclared,
            R::Range(range) => NodeEngineDeclaration::Range(range),
            R::MalformedEngines => NodeEngineDeclaration::MalformedEngines,
            R::MalformedNode => NodeEngineDeclaration::MalformedNode,
        };
        let (classification, status, reason) = node_evaluation(declaration);
        self.strict_satisfied &= matches!(status, "satisfied" | "not_declared");
        self.facts.push(Fact {
            location: "engines/node".into(),
            identity: key.into(),
            provenance,
            requirement: match requirement {
                R::Range(range) => Some(range.clone()),
                _ => None,
            },
            classification,
            status,
            reason,
        });
    }

    pub(crate) fn locked_package(&mut self, key: &str, package: &tapid_lockfile::LockedPackage) {
        self.selected_package(
            key,
            &registry_node_engine(package.node_engine()),
            "native_lock",
        );
    }

    /// Call only with verified original trees, before lifecycle hooks can derive outputs.
    /// Recovery enriches this operation's report, never the serialized lock.
    pub(crate) fn recover_original_manifests(
        &mut self,
        lock: &tapid_lockfile::Lockfile,
        trees: &std::collections::BTreeMap<String, std::path::PathBuf>,
    ) {
        for fact in &mut self.facts {
            if fact.status != "unknown_evidence" {
                continue;
            }
            let recovered = (|| {
                let package = lock.packages().get(&fact.identity)?;
                let tree = trees.get(&fact.identity)?;
                let root = crate::filesystem::tree::package_content_root(tree).ok()?;
                let bytes = crate::filesystem::read::read_bounded_file(
                    &root.join("package.json"),
                    1024 * 1024,
                )
                .ok()?;
                // Manifest owns project/source declaration classification; registry
                // normalization is not needed when the original file is available.
                let text = std::str::from_utf8(&bytes).ok()?;
                let manifest = PackageManifest::parse(text).ok()?;
                if manifest.name().as_str() != package.name()
                    || manifest.version().to_string() != package.version()
                {
                    return None;
                }
                let evaluation = node_evaluation(manifest.node_engine());
                let range = match manifest.node_engine() {
                    NodeEngineDeclaration::Range(range) => Some(range.to_owned()),
                    _ => None,
                };
                Some((evaluation, range))
            })();
            if let Some(((classification, status, reason), requirement)) = recovered {
                fact.classification = classification;
                fact.status = status;
                fact.reason = reason;
                fact.requirement = requirement;
                fact.provenance = "verified_source_manifest";
            }
        }
        self.evidence_complete = self
            .facts
            .iter()
            .all(|fact| fact.status != "unknown_evidence");
        self.strict_satisfied = self
            .facts
            .iter()
            .all(|fact| matches!(fact.status, "satisfied" | "not_declared"));
    }

    /// Historical locks have no declaration evidence. Omission cannot mean absence.
    pub(crate) fn unknown_package(&mut self, key: &str) {
        self.evidence_complete = false;
        self.strict_satisfied = false;
        self.facts.push(Fact {
            location: "engines/node".into(),
            identity: key.into(),
            provenance: "unavailable",
            requirement: None,
            classification: "unknown",
            status: "unknown_evidence",
            reason: "selected_package_evidence_unavailable",
        });
    }
}

fn node_evaluation(
    declaration: NodeEngineDeclaration<'_>,
) -> (&'static str, &'static str, &'static str) {
    match declaration {
        NodeEngineDeclaration::NotDeclared => ("absent", "not_declared", "no_declaration"),
        NodeEngineDeclaration::MalformedEngines => {
            ("malformed", "unsupported", "engines_not_object")
        }
        NodeEngineDeclaration::MalformedNode => ("malformed", "unsupported", "node_not_string"),
        NodeEngineDeclaration::Range(range) => {
            if tapid_resolver::Requirement::parse_range(range).is_ok() {
                ("range", "unchecked_runtime", "identification_not_requested")
            } else {
                ("unevaluable_range", "unsupported", "unsupported_node_range")
            }
        }
    }
}

pub(crate) fn lock_node_engine(
    requirement: &tapid_registry_client::NodeEngineRequirement,
) -> tapid_lockfile::LockedNodeEngine {
    use tapid_lockfile::LockedNodeEngine as L;
    use tapid_registry_client::NodeEngineRequirement as R;
    match requirement {
        R::Unknown => L::Unknown,
        R::NotDeclared => L::NotDeclared,
        R::Range(range) => L::Range(range.clone()),
        R::MalformedEngines => L::MalformedEngines,
        R::MalformedNode => L::MalformedNode,
    }
}

pub(crate) fn registry_node_engine(
    requirement: &tapid_lockfile::LockedNodeEngine,
) -> tapid_registry_client::NodeEngineRequirement {
    use tapid_lockfile::LockedNodeEngine as L;
    use tapid_registry_client::NodeEngineRequirement as R;
    match requirement {
        L::Unknown => R::Unknown,
        L::NotDeclared => R::NotDeclared,
        L::Range(range) => R::Range(range.clone()),
        L::MalformedEngines => R::MalformedEngines,
        L::MalformedNode => R::MalformedNode,
    }
}
