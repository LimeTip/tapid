//! Persist already accepted metadata omissions; this does not authorize new cuts.
use super::*;
use tapid_lockfile::{
    AcceptedOmission, DeclarationSection, LockedSelection, OmissionOrigin, OmissionReason,
    RetainedDeclaration, SelectionProfile,
};
use tapid_resolver::{DeclarationKind, EntryPointSection};

pub(super) fn record_registry_selection(
    lock: &mut Lockfile,
    manifest: &PackageManifest,
    workspace_manifests: &BTreeMap<String, PackageManifest>,
    resolution: &Resolution,
    records: &BTreeMap<PackageRecordKey, PackageRecord>,
    config: &crate::registry::RegistryConfig,
    mut omissions: Vec<AcceptedOmission>,
) -> Result<(), OperationalError> {
    // Copied sources retain their separate declaration contract.
    if resolution
        .selected
        .iter()
        .any(|id| id.registry.registry().is_none())
    {
        return Ok(());
    }
    let keys = resolution
        .selected
        .iter()
        .map(|id| {
            let record = &records[&(
                id.registry.to_string(),
                id.name.to_string(),
                id.version.to_string(),
            )];
            let platform = record
                .platform
                .selected_context(std::env::consts::OS, std::env::consts::ARCH, current_libc())
                .map_err(|error| OperationalError::new(ErrorKind::Resolution, error))?;
            let key = LockfilePackageKey::from_source(
                id.registry.clone(),
                id.name.clone(),
                id.version.clone(),
                resolution
                    .peer_contexts
                    .get(id)
                    .unwrap_or(&tapid_core::PeerContext::default()),
                &platform,
            )
            .to_string();
            Ok((id.clone(), key))
        })
        .collect::<Result<BTreeMap<_, _>, OperationalError>>()?;
    for parent in &resolution.selected {
        let record = &records[&(
            parent.registry.to_string(),
            parent.name.to_string(),
            parent.version.to_string(),
        )];
        let key = &keys[parent];
        for (name, raw) in &record.optional_dependencies {
            if lock.packages()[key].dependencies().contains_key(name) {
                continue;
            }
            let name_value: PackageName = name
                .parse()
                .map_err(|error: tapid_core::DomainError| error.to_string())?;
            let requirement: Requirement = raw
                .parse()
                .map_err(|error: ResolveError| error.to_string())?;
            let actual = requirement.package_name(&name_value);
            let registry = config.registry_for_dependency(&parent.registry, actual)?;
            let candidates = records
                .values()
                .filter(|candidate| candidate.registry == registry && &candidate.name == actual)
                .collect::<Vec<_>>();
            let reason = if candidates.is_empty() {
                OmissionReason::MetadataAbsent
            } else if candidates.iter().any(|candidate| {
                requirement.matches_tagged_version(&candidate.version, &candidate.dist_tags)
            }) && candidates
                .iter()
                .filter(|candidate| {
                    requirement.matches_tagged_version(&candidate.version, &candidate.dist_tags)
                })
                .all(|candidate| !current_platform_matches(&candidate.platform))
            {
                OmissionReason::IncompatiblePlatform
            } else {
                OmissionReason::NoUsableCandidate
            };
            omissions.push(AcceptedOmission {
                original_requirement: None,
                origin: OmissionOrigin::Package { key: key.clone() },
                name: name.clone(),
                requirement: raw.clone(),
                registry: registry.to_string(),
                target: None,
                reason,
            });
        }
    }
    if omissions.is_empty() {
        return Ok(());
    }
    for omission in &mut omissions {
        if let OmissionOrigin::Workspace { key } = &omission.origin {
            let original = workspace_manifests[key]
                .optional_dependencies()
                .get(&omission.name)
                .ok_or("missing original workspace omission declaration")?;
            if original != &omission.requirement {
                omission.original_requirement = Some(original.clone());
            }
        }
    }
    let mut declarations = Vec::new();
    for declaration in &resolution.declarations {
        if matches!(
            declaration.kind,
            DeclarationKind::Peer | DeclarationKind::OptionalPeer
        ) {
            continue;
        }
        let (origin, section) = if let Some(parent) = &declaration.parent {
            (
                OmissionOrigin::Package {
                    key: keys[parent].clone(),
                },
                if declaration.kind == DeclarationKind::Optional {
                    DeclarationSection::OptionalDependencies
                } else {
                    DeclarationSection::Dependencies
                },
            )
        } else {
            let entry = declaration
                .entry_point
                .as_ref()
                .ok_or("missing manifest declaration origin")?;
            (
                entry
                    .workspace
                    .as_ref()
                    .map_or(OmissionOrigin::Root, |key| OmissionOrigin::Workspace {
                        key: key.clone(),
                    }),
                match entry.section {
                    EntryPointSection::Dependencies => DeclarationSection::Dependencies,
                    EntryPointSection::DevDependencies => DeclarationSection::DevDependencies,
                    EntryPointSection::OptionalDependencies => {
                        DeclarationSection::OptionalDependencies
                    }
                },
            )
        };
        let original_requirement = if let OmissionOrigin::Workspace { key } = &origin {
            let manifest = &workspace_manifests[key];
            let dependencies = match section {
                DeclarationSection::Dependencies => manifest.dependencies(),
                DeclarationSection::DevDependencies => manifest.dev_dependencies(),
                DeclarationSection::OptionalDependencies => manifest.optional_dependencies(),
            };
            let original = dependencies
                .get(declaration.name.as_str())
                .ok_or("missing original workspace declaration")?;
            (original != &declaration.requirement.raw).then(|| original.clone())
        } else {
            None
        };
        declarations.push(RetainedDeclaration {
            original_requirement,
            origin,
            section,
            name: declaration.name.to_string(),
            requirement: declaration.requirement.raw.clone(),
            target: keys[&declaration.target].clone(),
        });
    }
    // Local links never enter registry resolution. Preserve their original
    // declarations alongside registry edges; automatic member roots are membership,
    // not invented root dependency declarations.
    for (origin, manifest) in std::iter::once((OmissionOrigin::Root, manifest)).chain(
        workspace_manifests
            .iter()
            .map(|(key, manifest)| (OmissionOrigin::Workspace { key: key.clone() }, manifest)),
    ) {
        for (section, dependencies) in [
            (DeclarationSection::Dependencies, manifest.dependencies()),
            (
                DeclarationSection::DevDependencies,
                manifest.dev_dependencies(),
            ),
            (
                DeclarationSection::OptionalDependencies,
                manifest.optional_dependencies(),
            ),
        ] {
            for (name, requirement) in dependencies {
                let target = match &origin {
                    OmissionOrigin::Root => lock.workspace_packages().keys().find(|key| {
                        key.parse::<LockfilePackageKey>()
                            .is_ok_and(|key| key.name.as_str() == name)
                    }),
                    OmissionOrigin::Workspace { key } => lock.workspace_packages()[key]
                        .dependencies()
                        .get(name)
                        .filter(|target| lock.workspace_packages().contains_key(*target)),
                    OmissionOrigin::Package { .. } => unreachable!(),
                };
                if let Some(target) = target {
                    declarations.push(RetainedDeclaration {
                        original_requirement: None,
                        origin: origin.clone(),
                        section,
                        name: name.clone(),
                        requirement: requirement.clone(),
                        target: target.clone(),
                    });
                }
            }
        }
    }
    declarations.sort();
    declarations.dedup();
    omissions.sort();
    lock.set_selection(LockedSelection {
        target: selection_target(),
        profile: SelectionProfile::Development,
        declarations,
        omissions,
    })
    .map_err(OperationalError::from)
}
