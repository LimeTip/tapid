//! Pure interpretation of manifest source declarations and requirements.
use crate::Requirement;
use std::collections::BTreeMap;
use tapid_core::{PackageName, PackageVersion, RegistryOrigin};
use tapid_manifest::PackageManifest;
use tapid_registry_client::GitDependency;
const NPM: &str = "https://registry.npmjs.org";
const JSR: &str = "https://jsr.io";

/// A syntactically supported copied source; content has not been read or verified.
#[derive(Clone, Debug)]
pub enum CopiedDeclaration {
    File(String),
    Git(GitDependency),
}

/// Classifies copied-source syntax, rejecting unsupported protocols and file paths.
pub fn copied_declaration(raw: &str) -> Result<Option<CopiedDeclaration>, String> {
    if let Some(path) = raw.strip_prefix("file:") {
        let path = path.strip_prefix("./").unwrap_or(path);
        if path.is_empty()
            || path.starts_with('/')
            || path.contains(['\\', ':', '|', '#'])
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || ![".tgz", ".tar.gz", ".tar"]
                .iter()
                .any(|extension| path.ends_with(extension))
        {
            return Err("file dependency must name a project-relative .tgz, .tar.gz, or .tar regular file; directory links and traversal are unsupported".into());
        }
        return Ok(Some(CopiedDeclaration::File(path.into())));
    }
    if raw.starts_with("git+") {
        return GitDependency::parse(raw)
            .map(CopiedDeclaration::Git)
            .map(Some);
    }
    if raw.contains("://")
        || raw.starts_with("github:")
        || raw.starts_with("git:")
        || raw.starts_with("ssh:")
    {
        return Err("unsupported dependency protocol; only git+https repositories and file tarballs are supported".into());
    }
    Ok(None)
}

/// Expands supported workspace protocols against an explicit local version.
pub fn workspace_requirement(
    dependency: &str,
    version: &PackageVersion,
) -> Result<Requirement, String> {
    let range = if let Some(protocol) = dependency.strip_prefix("workspace:") {
        match protocol {
            "*" => "*".to_owned(),
            "^" => format!("^{version}"),
            "~" => format!("~{version}"),
            _ => {
                return Err(format!(
                    "unsupported workspace protocol '{dependency}'; supported compatibility forms are workspace:*, workspace:^, and workspace:~"
                ));
            }
        }
    } else {
        dependency.to_owned()
    };
    range
        .parse::<Requirement>()
        .map_err(|error| format!("invalid workspace dependency range '{dependency}': {error}"))
}

/// Splits a manifest binding into its protocol-default registry and local package name.
/// Configured registry selection is supplied separately by the caller.
pub fn dependency_parts(name: &str) -> Result<(RegistryOrigin, PackageName), String> {
    let (origin, raw) = if let Some(v) = name.strip_prefix("jsr:") {
        (JSR, v)
    } else if let Some(v) = name.strip_prefix("npm:") {
        (NPM, v)
    } else {
        (NPM, name)
    };
    Ok((
        origin
            .parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?,
        raw.parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?,
    ))
}

/// Validates the installation subset of manifest overrides without changing parsing support.
pub fn manifest_overrides(
    manifest: &PackageManifest,
) -> Result<BTreeMap<PackageName, Requirement>, String> {
    let mut overrides = BTreeMap::new();
    for (name, range) in manifest.overrides() {
        if name.starts_with("jsr:") {
            return Err(format!(
                "unsupported override target '{name}': npm registry package names only are supported"
            ));
        }
        let selector_name = name.strip_prefix("npm:").unwrap_or(name);
        let has_version_selector = if let Some(scoped) = selector_name.strip_prefix('@') {
            scoped.contains('@')
        } else {
            selector_name.contains('@')
        };
        if has_version_selector {
            return Err(format!(
                "unsupported override selector '{name}': version-qualified selectors are not supported"
            ));
        }
        let (registry, package) = dependency_parts(name)?;
        if registry.to_string() != NPM {
            return Err(format!(
                "unsupported override target '{name}': npm registry package names only are supported"
            ));
        }
        let requirement = range
            .parse::<Requirement>()
            .map_err(|error| format!("invalid override '{name}' range '{range}': {error}"))?;
        if requirement.is_alias() {
            return Err(format!(
                "unsupported alias override '{name}': override values must be version ranges"
            ));
        }
        match overrides.insert(package.clone(), requirement.clone()) {
            Some(previous) if previous != requirement => {
                return Err(format!("conflicting override declarations for '{package}'"));
            }
            _ => {}
        }
    }
    Ok(overrides)
}

/// Registry preflight projection used by exact replay, excluding copied and workspace sources.
/// This preserves declared requirements and protocol-default origins, not configured routes.
pub fn manifest_roots(manifest: &PackageManifest) -> Result<Vec<crate::Dependency>, String> {
    let overrides = manifest_overrides(manifest)?;
    let mut roots = Vec::new();
    for declaration in manifest
        .dependency_declarations()
        .filter(|declaration| declaration.kind != tapid_manifest::DependencyKind::PeerDependencies)
    {
        let name = declaration.name;
        let range = declaration.specification;
        let kind = declaration.kind.field_name();
        if copied_declaration(range)
            .map_err(|reason| format!("dependency '{name}': {reason}"))?
            .is_some()
        {
            continue;
        }
        if range.starts_with("workspace:") {
            continue;
        }
        let (registry, package) = dependency_parts(name)?;
        let requirement = range.parse::<Requirement>().map_err(|error| {
            format!("invalid {kind} dependency '{name}' range '{range}': {error}")
        })?;
        if registry.as_str() == JSR && requirement.is_alias() {
            return Err(format!(
                "npm alias '{name}@{range}' cannot use a JSR dependency name"
            ));
        }
        if registry.as_str() == JSR && requirement.dist_tag().is_some() {
            return Err(format!(
                "unsupported JSR dist-tag dependency '{name}@{range}': npm dist-tags only are supported"
            ));
        }
        roots.push(crate::Dependency::new(
            registry.into(),
            package,
            requirement,
        ));
    }
    for dependency in &roots {
        if dependency.registry.to_string() == NPM
            && overrides
                .get(&dependency.name)
                .is_some_and(|override_requirement| {
                    override_requirement.raw != dependency.requirement.raw
                })
        {
            return Err(format!(
                "unsupported direct dependency override for '{}': npm requires the override range to match the declared dependency range",
                dependency.name
            ));
        }
    }
    Ok(roots)
}

/// The source selected for a declaration from explicit workspace facts or routing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclarationTarget {
    Workspace,
    Registry {
        registry: tapid_core::PackageSource,
        package: PackageName,
    },
}

/// A declaration retains its section and spelling independently of its target.
#[derive(Clone, Debug)]
pub struct InterpretedDeclaration<'a> {
    pub declaration: tapid_manifest::DependencyDeclaration<'a>,
    pub local_name: PackageName,
    pub declared_requirement: Requirement,
    pub requirement: Requirement,
    pub target: DeclarationTarget,
}

/// Interprets member edges without discovering paths or constructing lock records.
/// Peers retain local provider routing, including the existing alias-peer restriction.
pub fn interpret_workspace_member<'a>(
    manifest: &'a PackageManifest,
    members: &BTreeMap<String, PackageVersion>,
    select_registry: impl Fn(&str) -> Result<RegistryOrigin, String>,
) -> Result<Vec<InterpretedDeclaration<'a>>, String> {
    use tapid_manifest::DependencyKind;
    let mut result = Vec::new();
    // Preserve peer validation before ordinary dependency validation.
    for declaration in manifest
        .dependency_declarations()
        .filter(|d| d.kind == DependencyKind::PeerDependencies)
        .chain(
            manifest
                .dependency_declarations()
                .filter(|d| d.kind != DependencyKind::PeerDependencies),
        )
    {
        let name = declaration.name;
        let range = declaration.specification;
        let peer = declaration.kind == DependencyKind::PeerDependencies;
        let (requirement, target, local_name) = if let Some(version) = members.get(name) {
            let requirement = workspace_requirement(range, version)?;
            if !requirement.matches(version) {
                return Err(if peer {
                    format!(
                        "workspace member peer dependency '{}@{}' in '{}' does not match local provider version {}",
                        name,
                        range,
                        manifest.name(),
                        version
                    )
                } else {
                    format!(
                        "workspace dependency '{}@{}' in member '{}' does not match local member version {}",
                        name,
                        range,
                        manifest.name(),
                        version
                    )
                });
            }
            (
                requirement,
                DeclarationTarget::Workspace,
                name.parse::<PackageName>()
                    .map_err(|error| error.to_string())?,
            )
        } else {
            if range.starts_with("workspace:") {
                return Err(if peer {
                    format!(
                        "workspace peer dependency '{}' in member '{}' has no local workspace provider; refusing registry fallback",
                        name,
                        manifest.name()
                    )
                } else {
                    format!(
                        "workspace dependency '{}' in member '{}' has no local workspace target; refusing registry fallback",
                        name,
                        manifest.name()
                    )
                });
            }
            // Peer provider identities historically follow the local declaration, not aliases.
            let peer_registry = if peer {
                Some(select_registry(name)?)
            } else {
                None
            };
            if copied_declaration(range)
                .map_err(|reason| format!("dependency '{name}': {reason}"))?
                .is_some()
            {
                return Err(if peer {
                    format!(
                        "copied dependency '{name}' in a workspace member is unsupported; declare copied artifacts at the project root"
                    )
                } else {
                    format!(
                        "copied workspace member dependency '{name}' is unsupported; declare copied artifacts at the project root"
                    )
                });
            }
            let requirement = range.parse::<Requirement>().map_err(|error| {
                if peer {
                    format!(
                        "invalid workspace member peer dependency '{name}' range '{range}': {error}"
                    )
                } else {
                    format!("invalid workspace member dependency '{name}' range '{range}': {error}")
                }
            })?;
            let (origin, local_name) = dependency_parts(name)?;
            if !peer && origin.as_str() == JSR {
                if requirement.is_alias() {
                    return Err(format!(
                        "npm alias '{name}@{range}' cannot use a JSR dependency name"
                    ));
                }
                if requirement.dist_tag().is_some() {
                    return Err(format!(
                        "unsupported JSR dist-tag dependency '{name}@{range}': npm dist-tags only are supported"
                    ));
                }
            }
            let package = if peer {
                local_name.clone()
            } else {
                requirement.package_name(&local_name).clone()
            };
            let registry = match peer_registry {
                Some(registry) => registry.into(),
                None => declaration_registry(name, &requirement, &select_registry)?,
            };
            (
                requirement,
                DeclarationTarget::Registry { registry, package },
                local_name,
            )
        };
        result.push(InterpretedDeclaration {
            declaration,
            local_name,
            declared_requirement: requirement.clone(),
            requirement,
            target,
        });
    }
    Ok(result)
}

/// Root constraints and effective member requirements in input order.
#[derive(Clone, Debug)]
pub struct InterpretedRoots {
    pub roots: Vec<crate::Dependency>,
    pub direct_root_identities:
        std::collections::BTreeSet<(tapid_core::PackageSource, PackageName)>,
    pub member_requirements: Vec<Requirement>,
    pub overrides: BTreeMap<PackageName, Requirement>,
}
/// Rejects conflicting source identities for the same local root binding.
pub fn register_local_root_identity(
    identities: &mut BTreeMap<PackageName, String>,
    local_name: PackageName,
    identity: String,
) -> Result<(), String> {
    if identities
        .get(&local_name)
        .is_some_and(|previous| previous != &identity)
    {
        return Err(format!(
            "conflicting package identities for local dependency '{local_name}'"
        ));
    }
    identities.insert(local_name, identity);
    Ok(())
}

/// Interprets local root selection and effective member constraints for fresh resolution.
/// Member constraints retain their local binding and actual target through alias requirements.
/// All workspace facts are explicit; registry selection receives actual alias targets.
pub fn interpret_workspace_roots(
    manifest: &PackageManifest,
    members: &BTreeMap<String, PackageVersion>,
    member_dependencies: &[crate::Dependency],
    select_registry: impl Fn(&str) -> Result<RegistryOrigin, String>,
) -> Result<InterpretedRoots, String> {
    let overrides = manifest_overrides(manifest)?;
    let mut roots = Vec::new();
    let mut direct_root_identities = std::collections::BTreeSet::new();
    let mut member_requirements = Vec::with_capacity(member_dependencies.len());
    let mut local_root_identities = BTreeMap::new();
    for name in members.keys() {
        let local_name = name
            .parse::<PackageName>()
            .map_err(|error| error.to_string())?;
        register_local_root_identity(
            &mut local_root_identities,
            local_name,
            format!("workspace:{name}"),
        )?;
    }
    for declaration in manifest
        .dependency_declarations()
        .filter(|d| d.kind != tapid_manifest::DependencyKind::PeerDependencies)
    {
        let name = declaration.name;
        let range = declaration.specification;
        let kind = declaration.kind.field_name();
        if let Some(version) = members.get(name) {
            let requirement = workspace_requirement(range, version)?;
            if !requirement.matches(version) {
                return Err(format!(
                    "workspace dependency '{name}@{range}' does not match local member version {version}"
                ));
            }
            select_registry(name)?;
            let (_, package) = dependency_parts(name)?;
            if overrides
                .get(&package)
                .is_some_and(|override_requirement| override_requirement.raw != requirement.raw)
            {
                return Err(format!(
                    "unsupported direct dependency override for '{package}': npm requires the override range to match the declared dependency range"
                ));
            }
            let local_name = name
                .parse::<PackageName>()
                .map_err(|error| error.to_string())?;
            register_local_root_identity(
                &mut local_root_identities,
                local_name,
                format!("workspace:{name}"),
            )?;
            continue;
        }
        if range.starts_with("workspace:") {
            return Err(format!(
                "workspace dependency '{name}@{range}' has no matching local workspace member; refusing registry fallback"
            ));
        }
        if copied_declaration(range)
            .map_err(|reason| format!("dependency '{name}': {reason}"))?
            .is_some()
        {
            continue;
        }
        let requirement = range.parse::<Requirement>().map_err(|error| {
            format!("invalid {kind} dependency '{name}' range '{range}': {error}")
        })?;
        let (manifest_registry, local_package) = dependency_parts(name)?;
        if manifest_registry.to_string() == JSR && requirement.is_alias() {
            return Err(format!(
                "npm alias '{name}@{range}' cannot use a JSR dependency name"
            ));
        }
        let package = requirement.package_name(&local_package).clone();
        let registry = declaration_registry(name, &requirement, &select_registry)?;
        register_local_root_identity(
            &mut local_root_identities,
            local_package.clone(),
            format!("registry:{registry}|{package}"),
        )?;
        if overrides
            .get(&package)
            .is_some_and(|override_requirement| override_requirement.raw != requirement.raw)
        {
            return Err(format!(
                "unsupported direct dependency override for '{package}': npm requires the override range to match the declared dependency range"
            ));
        }
        direct_root_identities.insert((registry.clone(), package.clone()));
        roots.push(crate::Dependency::new(registry, local_package, requirement));
    }
    for dependency in member_dependencies {
        let local_name = dependency.name.clone();
        let package = dependency.requirement.package_name(&dependency.name);
        register_local_root_identity(
            &mut local_root_identities,
            local_name,
            format!("registry:{}|{}", dependency.registry, package),
        )?;
        let requirement = overrides
            .get(package)
            .cloned()
            .unwrap_or_else(|| dependency.requirement.clone());
        roots.push(crate::Dependency::new(
            dependency.registry.clone(),
            dependency.name.clone(),
            requirement.clone(),
        ));
        member_requirements.push(requirement);
    }
    Ok(InterpretedRoots {
        roots,
        direct_root_identities,
        member_requirements,
        overrides,
    })
}

/// Required and optional source-qualified declarations. Sets deliberately overlap.
#[derive(Clone, Debug, Default)]
pub struct RootMembership {
    pub required: std::collections::BTreeSet<(RegistryOrigin, PackageName)>,
    pub optional: std::collections::BTreeSet<(RegistryOrigin, PackageName)>,
}

/// Classifies membership without parsing requirements, including copied/workspace roots.
/// The selector preserves the caller's registry identity contract.
pub fn root_membership(
    manifest: &PackageManifest,
    select_identity: impl Fn(&str) -> Result<(RegistryOrigin, PackageName), String>,
) -> Result<RootMembership, String> {
    use tapid_manifest::DependencyKind;
    let mut membership = RootMembership::default();
    for declaration in manifest.dependency_declarations() {
        match declaration.kind {
            DependencyKind::PeerDependencies => continue,
            DependencyKind::OptionalDependencies => {
                membership
                    .optional
                    .insert(select_identity(declaration.name)?);
            }
            _ => {
                membership
                    .required
                    .insert(select_identity(declaration.name)?);
            }
        }
    }
    Ok(membership)
}

/// Legacy replay retains the declared identity and every applicable requirement.
/// It intentionally does not apply fresh-resolution overrides or alias projection.
pub fn legacy_root_requirements(
    manifest: &PackageManifest,
    select_identity: impl Fn(&str) -> Result<(RegistryOrigin, PackageName), String>,
) -> Result<BTreeMap<(RegistryOrigin, PackageName), Vec<Requirement>>, String> {
    let mut identities = BTreeMap::new();
    for declaration in manifest
        .dependency_declarations()
        .filter(|d| d.kind != tapid_manifest::DependencyKind::PeerDependencies)
    {
        let identity = select_identity(declaration.name)?;
        if declaration.specification.starts_with("workspace:") {
            continue;
        }
        identities.entry(identity).or_insert_with(Vec::new).push(
            declaration
                .specification
                .parse::<Requirement>()
                .map_err(|error| format!("{error:?}"))?,
        );
    }
    Ok(identities)
}

/// Workspace registry peers require an explicitly declared direct root provider.
/// This is distinct from registry-package peers, which use local root bindings.
/// The caller supplies retained metadata tags; absent evidence cannot satisfy a peer.
pub fn validate_workspace_registry_peer<'a>(
    local_name: &str,
    dependency: &crate::Dependency,
    direct_roots: &std::collections::BTreeSet<(tapid_core::PackageSource, PackageName)>,
    resolution: &crate::Resolution,
    tags_for: impl Fn(
        &tapid_registry_client::RegistryPackageId,
    ) -> Option<&'a std::collections::BTreeSet<String>>,
) -> Result<(), String> {
    if !direct_roots.contains(&(dependency.registry.clone(), dependency.name.clone()))
        || !resolution.roots.iter().any(|root| {
            root.registry == dependency.registry
                && root.name == dependency.name
                && tags_for(root).is_some_and(|tags| {
                    dependency
                        .requirement
                        .matches_tagged_version(&root.version, tags)
                })
        })
    {
        return Err(format!(
            "workspace member peer dependency '{}' has no direct root provider satisfying {:?}",
            local_name, dependency.requirement
        ));
    }
    Ok(())
}

/// Routes an alias by its actual package, preserving explicit registry spellings otherwise.
pub fn declaration_registry(
    name: &str,
    requirement: &Requirement,
    select_registry: impl Fn(&str) -> Result<RegistryOrigin, String>,
) -> Result<tapid_core::PackageSource, String> {
    if requirement.is_alias() {
        let (_, local) = dependency_parts(name)?;
        select_registry(requirement.package_name(&local).as_str()).map(Into::into)
    } else {
        select_registry(name).map(Into::into)
    }
}
