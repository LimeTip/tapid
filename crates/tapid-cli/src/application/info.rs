use super::outcome::{ErrorKind, OperationalError};
use std::{collections::BTreeMap, path::Path};
use tapid_core::PackageVersion;
use tapid_registry_client::{NpmInventory, NpmRegistry, NpmTagTarget, NpmVersionFacts};
use tapid_resolver::Requirement;
mod continuation;
pub(crate) use continuation::{Collection, PageRequest, Pages};

pub(crate) struct InfoReport {
    pub(crate) inventory: NpmInventory,
    pub(crate) selector: String,
    pub(crate) selector_kind: &'static str,
    pub(crate) selection_status: &'static str,
    pub(crate) facts: Option<NpmVersionFacts>,
    pub(crate) snapshot: bool,
    pub(crate) node_version: Option<PackageVersion>,
    pub(crate) node_match: Option<bool>,
    pub(crate) pages: Pages,
    pub(crate) peer_requirement_support: BTreeMap<tapid_core::PackageName, bool>,
}

pub(crate) fn inspect(
    spec: &str,
    project: &Path,
    metadata: Option<&Path>,
    offline: bool,
    node_version: Option<PackageVersion>,
    page_request: PageRequest<'_>,
) -> Result<InfoReport, OperationalError> {
    let invalid = || {
        OperationalError::new(
            ErrorKind::InvalidRequest,
            "info requires an npm package with a supported version, range, or tag",
        )
    };
    let spec = spec.trim();
    let bare = spec.strip_prefix("npm:").unwrap_or(spec);
    let split = bare.rfind('@').filter(|index| *index > 0);
    let (name, selector) = split.map_or((bare, "latest"), |index| {
        (&bare[..index], bare[index + 1..].trim())
    });
    // Validate before configuration, credential resolution, or transport creation.
    let _: tapid_core::PackageName = name.parse().map_err(|_| invalid())?;
    if selector.is_empty() {
        return Err(invalid());
    }
    let requirement: Requirement = selector.parse().map_err(|_| invalid())?;
    if requirement.is_alias() {
        return Err(invalid());
    }
    if offline && metadata.is_none() {
        return Err(OperationalError::new(
            ErrorKind::RegistryMetadataUnavailable,
            "offline info requires a metadata snapshot",
        ));
    }
    let config = crate::registry::RegistryConfig::load(project)
        .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?;
    let (origin, name) = config.identity_for_spec(name).map_err(|_| invalid())?;
    let inventory = if let Some(path) = metadata {
        let body =
            crate::filesystem::read::read_bounded_file(path, 32 * 1024 * 1024).map_err(|_| {
                OperationalError::new(
                    ErrorKind::RegistryMetadata,
                    "cannot read metadata snapshot as a regular file within 32 MiB",
                )
            })?;
        NpmInventory::from_metadata(&origin, name, &body)
            .map_err(|error| OperationalError::from_source(ErrorKind::RegistryMetadata, error))?
    } else {
        let mut cache = BTreeMap::new();
        let transport = crate::online::metadata_transport_for_package(
            &mut cache,
            &config,
            &origin,
            &name,
            &config.configured_origins(),
        )?;
        NpmRegistry::new(transport, origin)
            .inventory(name.as_str())
            .map_err(OperationalError::from)?
    };
    let pages = Pages::new(&inventory, selector, page_request)?;
    let (selector_kind, selection_status, version) = if let Some(tag) = requirement.dist_tag() {
        match inventory.tags().get(tag) {
            Some(NpmTagTarget::Version(version)) => ("tag", "selected", Some(version.clone())),
            Some(NpmTagTarget::MissingVersion(_)) => ("tag", "missing_tag_target", None),
            Some(NpmTagTarget::Unsupported) => ("tag", "unsupported_tag_target", None),
            None => ("tag", "missing_tag", None),
        }
    } else {
        let exact = selector.parse::<PackageVersion>().ok();
        let version = inventory
            .versions()
            .iter()
            .find(|version| {
                exact
                    .as_ref()
                    .map_or_else(|| requirement.matches(version), |exact| exact == *version)
            })
            .cloned();
        (
            if exact.is_some() { "exact" } else { "range" },
            if version.is_some() {
                "selected"
            } else {
                "no_supported_match"
            },
            version,
        )
    };
    let facts = version
        .as_ref()
        .map(|version| inventory.facts(version))
        .transpose()
        .map_err(|error| OperationalError::from_source(ErrorKind::RegistryMetadata, error))?;
    let node_match = node_version.as_ref().and_then(|version| {
        let expression = facts.as_ref()?.engines.as_ref()?.get("node")?;
        let requirement = supported_requirement(expression)?;
        Some(requirement.matches(version))
    });
    let peer_requirement_support = facts
        .as_ref()
        .and_then(|facts| facts.peers.as_ref())
        .into_iter()
        .flatten()
        .map(|peer| {
            (
                peer.name.clone(),
                supported_requirement(&peer.requirement).is_some(),
            )
        })
        .collect();
    Ok(InfoReport {
        inventory,
        selector: selector.to_owned(),
        selector_kind,
        selection_status,
        facts,
        snapshot: metadata.is_some(),
        node_version,
        node_match,
        pages,
        peer_requirement_support,
    })
}

fn supported_requirement(expression: &str) -> Option<Requirement> {
    let requirement: Requirement = expression.parse().ok()?;
    (!requirement.is_alias() && requirement.dist_tag().is_none()).then_some(requirement)
}
