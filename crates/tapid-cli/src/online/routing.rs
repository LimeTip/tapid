//! Route selection and origin-scoped metadata/artifact transport caches.
use crate::application::outcome::{ErrorKind, OperationalError};

use super::{
    BTreeMap, HttpsTransport, JSR, PackageName, PackageSource, RegistryOrigin, Requirement,
};

/// Alias requirements follow their actual target's route; ordinary declarations
/// retain their explicitly selected registry identity, including JSR.
pub(super) fn registry_for_requirement(
    config: &crate::registry::RegistryConfig,
    name: &str,
    requirement: &Requirement,
) -> Result<PackageSource, String> {
    if requirement.is_alias() {
        let (_, local) = super::dep_parts(name)?;
        config
            .origin_for_name(requirement.package_name(&local))
            .map(Into::into)
    } else {
        super::dependency_identity(config, name).map(|(registry, _)| registry.into())
    }
}

fn package_registry_route(
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
) -> Result<crate::registry::RegistryRoute, OperationalError> {
    if registry.to_string() == JSR {
        return Ok(crate::registry::RegistryRoute {
            origin: registry.clone(),
            token: None,
            policy: "jsr".to_owned(),
        });
    }
    let route = registry_config.route(name.as_str())?;
    if route.origin != *registry {
        return Err(OperationalError::new(
            ErrorKind::RegistryConfiguration,
            format!(
                "registry identity mismatch for package {name}: selected {}, requested {registry}",
                route.origin
            ),
        ));
    }
    Ok(route)
}

pub(super) fn transport_for_route<'a>(
    cache: &'a mut BTreeMap<(String, String), HttpsTransport>,
    route: crate::registry::RegistryRoute,
    allowed_origins: &[String],
    artifact: bool,
    network: &tapid_registry_client::NetworkOptions,
) -> Result<&'a HttpsTransport, OperationalError> {
    let origin = route.origin.to_string();
    let key = (origin.clone(), route.policy);
    if !cache.contains_key(&key) {
        let credentials = route
            .token
            .map(|token| vec![(origin.clone(), token)])
            .unwrap_or_default();
        let transport = if artifact {
            HttpsTransport::authenticated_artifact_with_network(
                allowed_origins.to_vec(),
                credentials,
                network,
            )
        } else {
            HttpsTransport::authenticated_metadata_with_network(
                allowed_origins.to_vec(),
                credentials,
                network,
            )
        }
        .map_err(|error| {
            OperationalError::from_source(ErrorKind::RegistryTransport, error)
                .context("cannot create registry transport")
        })?;
        cache.insert(key.clone(), transport);
    }
    Ok(cache
        .get(&key)
        .expect("transport cache entry was just found or inserted"))
}

pub(crate) fn metadata_transport_for_package<'a>(
    cache: &'a mut BTreeMap<(String, String), HttpsTransport>,
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
    allowed_origins: &[String],
) -> Result<&'a HttpsTransport, OperationalError> {
    let route = package_registry_route(registry_config, registry, name)?;
    let network = registry_config
        .network_options()
        .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?;
    transport_for_route(cache, route, allowed_origins, false, network)
}

pub(super) fn artifact_transport_for_package<'a>(
    cache: &'a mut BTreeMap<(String, String), HttpsTransport>,
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
    allowed_origins: &[String],
) -> Result<&'a HttpsTransport, OperationalError> {
    let route = package_registry_route(registry_config, registry, name)?;
    let network = registry_config
        .network_options()
        .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?;
    transport_for_route(cache, route, allowed_origins, true, network)
}
