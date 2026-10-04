//! Route selection and origin-scoped metadata/artifact transport caches.

use super::{BTreeMap, HttpsTransport, JSR, PackageName, RegistryOrigin};

fn package_registry_route(
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
) -> Result<crate::registry::RegistryRoute, String> {
    if registry.to_string() == JSR {
        return Ok(crate::registry::RegistryRoute {
            origin: registry.clone(),
            token: None,
            policy: "jsr".to_owned(),
        });
    }
    let route = registry_config.route(name.as_str())?;
    if route.origin != *registry {
        return Err(format!(
            "registry identity mismatch for package {name}: selected {}, requested {registry}",
            route.origin
        ));
    }
    Ok(route)
}

pub(super) fn transport_for_route<'a>(
    cache: &'a mut BTreeMap<(String, String), HttpsTransport>,
    route: crate::registry::RegistryRoute,
    allowed_origins: &[String],
    artifact: bool,
) -> Result<&'a HttpsTransport, String> {
    let origin = route.origin.to_string();
    let key = (origin.clone(), route.policy);
    if !cache.contains_key(&key) {
        let credentials = route
            .token
            .map(|token| vec![(origin.clone(), token)])
            .unwrap_or_default();
        let transport = if artifact {
            HttpsTransport::authenticated_artifact(allowed_origins.to_vec(), credentials)
        } else {
            HttpsTransport::authenticated_metadata(allowed_origins.to_vec(), credentials)
        }
        .map_err(|error| format!("cannot create registry transport: {error}"))?;
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
) -> Result<&'a HttpsTransport, String> {
    let route = package_registry_route(registry_config, registry, name)?;
    transport_for_route(cache, route, allowed_origins, false)
}

pub(super) fn artifact_transport_for_package<'a>(
    cache: &'a mut BTreeMap<(String, String), HttpsTransport>,
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
    allowed_origins: &[String],
) -> Result<&'a HttpsTransport, String> {
    let route = package_registry_route(registry_config, registry, name)?;
    transport_for_route(cache, route, allowed_origins, true)
}
