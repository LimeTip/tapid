//! Typed metadata-to-selection handoff. No artifact or store access is permitted here.
use super::*;
use tapid_lockfile::{
    NativeDeclaration, NativeDeclarationKind, NativeDependency, NativeDependencyKind,
    NativeSelectedPackage, NativeSelection, NativeSourceEvidence,
};

impl graph::ResolvedGraph {
    // Kept internal until install/replay can consume pending evidence atomically.
    #[allow(dead_code)]
    pub(super) fn native_selection(
        &self,
        project: &Path,
        manifest: &PackageManifest,
        config: &crate::registry::RegistryConfig,
    ) -> Result<NativeSelection, OperationalError> {
        let invalid = |message: &str| OperationalError::new(ErrorKind::Resolution, message);
        if self
            .workspace_manifests
            .values()
            .any(|manifest| !manifest.peer_dependencies().is_empty())
        {
            return Err(invalid(
                "native selection producer does not yet support workspace peer evidence",
            ));
        }
        let mut keys = BTreeMap::new();
        let mut packages = BTreeMap::new();
        for id in &self.resolution.selected {
            let record = &self.records[&(
                id.registry.to_string(),
                id.name.to_string(),
                id.version.to_string(),
            )];
            if !record.complete_dependency_kinds {
                return Err(invalid(
                    "legacy metadata lacks complete dependency kind evidence",
                ));
            }
            if id.registry.registry().is_none() {
                return Err(invalid(
                    "pending native selections require registry sources",
                ));
            }
            let platform = record
                .platform
                .selected_context(std::env::consts::OS, std::env::consts::ARCH, current_libc())
                .map_err(|error| invalid(&error))?;
            let peer = self
                .resolution
                .peer_contexts
                .get(id)
                .cloned()
                .unwrap_or_default();
            let key = LockfilePackageKey::from_source(
                id.registry.clone(),
                id.name.clone(),
                id.version.clone(),
                &peer,
                &platform,
            )
            .to_string();
            packages.insert(
                key.clone(),
                NativeSelectedPackage {
                    source_evidence: NativeSourceEvidence::Pending {
                        artifact_url: record.artifact.clone(),
                        artifact_integrity: record
                            .integrity
                            .as_ref()
                            .ok_or_else(|| {
                                invalid("pending native selection requires declared integrity")
                            })?
                            .to_string(),
                    },
                    dependencies: BTreeMap::new(),
                    peer_providers: BTreeMap::new(),
                },
            );
            keys.insert(id.clone(), key);
        }
        for edge in &self.resolution.dependencies {
            let record = &self.records[&(
                edge.parent.registry.to_string(),
                edge.parent.name.to_string(),
                edge.parent.version.to_string(),
            )];
            let kind = if record
                .optional_dependencies
                .contains_key(edge.dependency.as_str())
            {
                NativeDependencyKind::Optional
            } else if record.dependencies.contains_key(edge.dependency.as_str()) {
                NativeDependencyKind::Required
            } else {
                return Err(invalid(
                    "selected dependency lacks declaration kind evidence",
                ));
            };
            packages
                .get_mut(&keys[&edge.parent])
                .expect("selected parent")
                .dependencies
                .insert(
                    edge.dependency.to_string(),
                    NativeDependency {
                        kind,
                        target: keys[&edge.child].clone(),
                    },
                );
        }
        for (id, context) in &self.resolution.peer_contexts {
            for (name, version) in context.entries() {
                // Native peers are supported only through accepted direct-root providers.
                // A version match alone is insufficient: preserve the routed exact binding.
                let registry = config.registry_for_dependency(&id.registry, name)?;
                let provider = self
                    .resolution
                    .root_bindings
                    .get(&(registry, name.clone()))
                    .filter(|provider| provider.name == *name && provider.version == *version)
                    .ok_or_else(|| invalid("missing exact direct-root peer provider"))?;
                packages
                    .get_mut(&keys[id])
                    .expect("selected consumer")
                    .peer_providers
                    .insert(name.to_string(), keys[provider].clone());
            }
        }
        let mut workspace_digests = BTreeMap::new();
        let mut workspace_targets = BTreeMap::new();
        for member in &self.workspace_locked {
            let key = member.key();
            let identity: LockfilePackageKey = key.parse()?;
            let source = identity
                .source
                .workspace()
                .ok_or_else(|| invalid("missing workspace source"))?
                .clone();
            workspace_digests.insert(
                source.path().to_owned(),
                member.manifest_digest().to_owned(),
            );
            workspace_targets.insert(identity.name.to_string(), key.clone());
            packages.insert(
                key,
                NativeSelectedPackage {
                    source_evidence: NativeSourceEvidence::Workspace { source },
                    dependencies: BTreeMap::new(),
                    peer_providers: BTreeMap::new(),
                },
            );
        }
        let mut declarations = Vec::new();
        for (workspace, manifest) in std::iter::once((None, manifest)).chain(
            self.workspace_manifests
                .iter()
                .map(|(path, manifest)| (Some(path.clone()), manifest)),
        ) {
            for (kind, name, range) in manifest.effective_install_declarations() {
                let (_, local) = dep_parts(name)?;
                let target = if let Some(key) = workspace_targets.get(name) {
                    key.clone()
                } else {
                    let requirement = range
                        .parse::<Requirement>()
                        .map_err(|error| invalid(&error.to_string()))?;
                    let registry = registry_for_requirement(config, name, &requirement)?;
                    let target = self
                        .resolution
                        .root_bindings
                        .get(&(registry, local.clone()))
                        .ok_or_else(|| invalid("missing accepted declaration binding"))?;
                    keys[target].clone()
                };
                let kind = match kind {
                    tapid_manifest::DependencyKind::Dependencies => NativeDeclarationKind::Required,
                    tapid_manifest::DependencyKind::DevDependencies => NativeDeclarationKind::Dev,
                    tapid_manifest::DependencyKind::OptionalDependencies => {
                        NativeDeclarationKind::Optional
                    }
                    tapid_manifest::DependencyKind::PeerDependencies => {
                        unreachable!("effective install declaration")
                    }
                };
                declarations.push(NativeDeclaration {
                    workspace: workspace.clone(),
                    name: local.to_string(),
                    kind,
                    target,
                });
            }
        }
        NativeSelection::new(
            "0".to_owned(),
            "0".to_owned(),
            root_digest(project)?,
            workspace_digests,
            declarations,
            packages,
        )
        .map_err(Into::into)
    }
}
