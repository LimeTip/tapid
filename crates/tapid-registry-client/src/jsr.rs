use crate::{
    HttpResponse, HttpTransport, MetadataError, RegistryArtifact, RegistryClientError,
    RegistryKind, RegistryPackageId, artifact::download_artifact,
};
use std::collections::BTreeMap;
use tapid_core::{PackageName, PackageVersion, RegistryOrigin};
use url::Url;

pub struct JsrRegistry<T> {
    transport: T,
    origin: RegistryOrigin,
}
impl<T: HttpTransport> JsrRegistry<T> {
    pub fn new(transport: T, origin: RegistryOrigin) -> Self {
        Self { transport, origin }
    }
    pub fn fetch(&self, package: &str) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
        let name: PackageName = package.parse().map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidPackageName(package.into()))
        })?;
        if !package.starts_with('@') || package.matches('/').count() != 1 {
            return Err(RegistryClientError::Metadata(
                MetadataError::InvalidPackageName(package.into()),
            ));
        }
        let url = jsr_metadata_url(&self.origin, package)?;
        let response = self
            .transport
            .get(&url)
            .map_err(RegistryClientError::Transport)?;
        parse_jsr(&self.origin, &name, json_response(&response)?)
    }
    pub fn download_artifact(
        &self,
        artifact_url: &str,
    ) -> Result<HttpResponse, RegistryClientError> {
        download_artifact(&self.transport, artifact_url)
    }
}

fn json_response(response: &HttpResponse) -> Result<&[u8], RegistryClientError> {
    if response.status != 200 {
        return Err(RegistryClientError::Metadata(MetadataError::HttpStatus(
            response.status,
        )));
    }
    let Some(content_type) = response.content_type.as_deref() else {
        return Err(RegistryClientError::Metadata(
            MetadataError::UnsupportedContentType("missing content type".into()),
        ));
    };
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if media_type != "application/json" && !media_type.ends_with("+json") {
        return Err(RegistryClientError::Metadata(
            MetadataError::UnsupportedContentType(content_type.into()),
        ));
    }
    Ok(&response.body)
}

fn json_object(body: &[u8]) -> Result<serde_json::Map<String, serde_json::Value>, MetadataError> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|error| MetadataError::InvalidJson(error.to_string()))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| MetadataError::InvalidJson("metadata must be an object".into()))
}

fn jsr_metadata_url(origin: &RegistryOrigin, package: &str) -> Result<String, RegistryClientError> {
    let mut parts = package.split('/');
    let scope = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if !scope.starts_with('@') || name.is_empty() || parts.next().is_some() {
        return Err(RegistryClientError::Metadata(
            MetadataError::InvalidPackageName(package.into()),
        ));
    }
    let mut url = Url::parse(&format!("{}/", origin)).map_err(|_| {
        RegistryClientError::Metadata(MetadataError::InvalidRegistry(origin.to_string()))
    })?;
    {
        let mut segments = url.path_segments_mut().map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidRegistry(origin.to_string()))
        })?;
        segments.pop_if_empty();
        segments.push(scope).push(name).push("meta.json");
    }
    Ok(url.to_string())
}

fn parse_jsr(
    origin: &RegistryOrigin,
    name: &PackageName,
    body: &[u8],
) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
    let root = json_object(body).map_err(RegistryClientError::Metadata)?;
    if root.get("scope").and_then(|value| value.as_str())
        != name
            .to_string()
            .split('/')
            .next()
            .map(|scope| scope.trim_start_matches('@'))
        || root.get("name").and_then(|value| value.as_str()) != name.to_string().split('/').nth(1)
    {
        return Err(RegistryClientError::Metadata(
            MetadataError::ConflictingField("package identity".into()),
        ));
    }
    let versions = root
        .get("versions")
        .and_then(|value| value.as_object())
        .ok_or_else(|| {
            RegistryClientError::Metadata(MetadataError::MissingField("versions".into()))
        })?;
    let mut artifacts = Vec::new();
    for (key, value) in versions {
        if value.as_object().is_none() {
            return Err(RegistryClientError::Metadata(MetadataError::InvalidJson(
                "version entry must be an object".into(),
            )));
        }
        let version: PackageVersion = key.parse().map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidVersion(key.clone()))
        })?;
        let version_object = value.as_object().expect("checked above");
        let npm = version_object
            .get("npm")
            .and_then(|value| value.as_object());
        let artifact_url = npm
            .and_then(|value| value.get("tarball"))
            .and_then(|value| value.as_str());
        let integrity_value = npm.and_then(|value| value.get("integrity"));
        let (artifact_url, integrity) = match (artifact_url, integrity_value) {
            (Some(url), Some(value)) => {
                let integrity = value
                    .as_str()
                    .ok_or_else(|| {
                        RegistryClientError::Metadata(MetadataError::InvalidIntegrity(key.clone()))
                    })?
                    .parse()
                    .map_err(|_| {
                        RegistryClientError::Metadata(MetadataError::InvalidIntegrity(key.clone()))
                    })?;
                let artifact_url = Url::parse(url).map_err(|_| {
                    RegistryClientError::Metadata(MetadataError::InvalidArtifact(url.into()))
                })?;
                if artifact_url.scheme() != "https" {
                    return Err(RegistryClientError::Metadata(
                        MetadataError::InvalidArtifact(url.into()),
                    ));
                }
                (artifact_url.to_string(), integrity)
            }
            _ => {
                return Err(RegistryClientError::Metadata(
                    MetadataError::UnsupportedIntegrity(key.clone()),
                ));
            }
        };
        artifacts.push(RegistryArtifact {
            identity: RegistryPackageId::new(origin.clone(), name.clone(), version),
            artifact_url,
            integrity: Some(integrity),
            dependencies: parse_jsr_dependencies(version_object)?,
            registry_kind: RegistryKind::Jsr,
        });
    }
    artifacts.sort_by_key(|artifact| std::cmp::Reverse(artifact.identity.version));
    Ok(artifacts)
}

fn parse_jsr_dependencies(
    version: &serde_json::Map<String, serde_json::Value>,
) -> Result<BTreeMap<PackageName, String>, RegistryClientError> {
    let Some(manifest) = version.get("manifest") else {
        return Ok(BTreeMap::new());
    };
    let manifest = manifest.as_object().ok_or_else(|| {
        RegistryClientError::Metadata(MetadataError::InvalidJson(
            "manifest must be an object".into(),
        ))
    })?;
    let mut dependencies = BTreeMap::new();
    for field in ["dependencies", "peerDependencies"] {
        let Some(value) = manifest.get(field) else {
            continue;
        };
        let parsed = parse_dependencies(Some(value))?;
        dependencies.extend(parsed);
    }
    Ok(dependencies)
}

fn parse_dependencies(
    value: Option<&serde_json::Value>,
) -> Result<BTreeMap<PackageName, String>, RegistryClientError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        RegistryClientError::Metadata(MetadataError::InvalidDependency("dependencies".into()))
    })?;
    object
        .iter()
        .map(|(name, requirement)| {
            let package = name.parse().map_err(|_| {
                RegistryClientError::Metadata(MetadataError::InvalidDependency(name.clone()))
            })?;
            let requirement = requirement
                .as_str()
                .filter(|requirement| !requirement.trim().is_empty())
                .ok_or_else(|| {
                    RegistryClientError::Metadata(MetadataError::InvalidDependency(name.clone()))
                })?;
            Ok((package, requirement.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransportError;

    struct Fake {
        body: Vec<u8>,
        url: String,
    }
    impl HttpTransport for Fake {
        fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
            assert_eq!(url, self.url);
            Ok(HttpResponse {
                status: 200,
                content_type: Some("application/json; charset=utf-8".into()),
                body: self.body.clone(),
            })
        }
    }
    fn fake(body: &[u8], url: &str) -> Fake {
        Fake {
            body: body.to_vec(),
            url: url.into(),
        }
    }

    #[test]
    fn jsr_without_sha512_integrity_fails_closed() {
        let origin: RegistryOrigin = "https://jsr.io".parse().unwrap();
        let error = JsrRegistry::new(
            fake(
                br#"{"scope":"std","name":"path","latest":"1.0.0","versions":{"1.0.0":{"createdAt":"2025-01-01T00:00:00Z"}}}"#,
                "https://jsr.io/@std/path/meta.json",
            ),
            origin,
        )
        .fetch("@std/path");
        assert!(matches!(
            error,
            Err(RegistryClientError::Metadata(
                MetadataError::UnsupportedIntegrity(_)
            ))
        ));
    }

    #[test]
    fn jsr_maps_supported_scope() {
        let origin: RegistryOrigin = "https://jsr.io".parse().unwrap();
        let artifacts = JsrRegistry::new(
            fake(
                br#"{"scope":"std","name":"path","latest":"1.0.0","versions":{"1.0.0":{"createdAt":"2025-01-01T00:00:00Z","npm":{"tarball":"https://npm.jsr.io/~/@std__path/1.0.0.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="},"manifest":{"dependencies":{"@std/assert":"^1.0.0"}}}}}"#,
                "https://jsr.io/@std/path/meta.json",
            ),
            origin,
        )
        .fetch("@std/path")
        .unwrap();
        assert_eq!(
            artifacts[0].artifact_url,
            "https://npm.jsr.io/~/@std__path/1.0.0.tgz"
        );
        assert_eq!(artifacts[0].dependencies.len(), 1);
    }

    #[test]
    fn malformed_jsr_manifest_dependency_is_rejected() {
        let origin: RegistryOrigin = "https://jsr.io".parse().unwrap();
        let body = br#"{"scope":"std","name":"path","versions":{"1.0.0":{"npm":{"tarball":"https://npm.jsr.io/a.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="},"manifest":{"dependencies":[]}}}}"#;
        assert!(
            JsrRegistry::new(fake(body, "https://jsr.io/@std/path/meta.json"), origin)
                .fetch("@std/path")
                .is_err()
        );
    }

    #[test]
    fn jsr_unsupported_integrity_is_rejected() {
        let origin: RegistryOrigin = "https://jsr.io".parse().unwrap();
        let body = br#"{"scope":"std","name":"path","versions":{"1.0.0":{"npm":{"tarball":"https://npm.jsr.io/a.tgz","integrity":"sha256-deadbeef"}}}}"#;
        assert!(matches!(
            JsrRegistry::new(fake(body, "https://jsr.io/@std/path/meta.json"), origin)
                .fetch("@std/path"),
            Err(RegistryClientError::Metadata(
                MetadataError::InvalidIntegrity(_)
            ))
        ));
    }

    #[test]
    fn jsr_versions_are_sorted_semver_descending() {
        let origin: RegistryOrigin = "https://jsr.io".parse().unwrap();
        let npm = |version: &str| {
            format!(
                r#"{{"createdAt":"2025-01-01T00:00:00Z","npm":{{"tarball":"https://npm.jsr.io/~/@std__path/{version}.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#
            )
        };
        let body = serde_json::json!({
            "scope": "std", "name": "path", "latest": "2.0.0",
            "versions": {"1.0.0": serde_json::from_str::<serde_json::Value>(&npm("1.0.0")).unwrap(), "2.0.0": serde_json::from_str::<serde_json::Value>(&npm("2.0.0")).unwrap()}
        }).to_string();
        let result = JsrRegistry::new(
            fake(body.as_bytes(), "https://jsr.io/@std/path/meta.json"),
            origin,
        )
        .fetch("@std/path")
        .unwrap();
        assert_eq!(
            result
                .iter()
                .map(|artifact| artifact.identity.version.to_string())
                .collect::<Vec<_>>(),
            ["2.0.0", "1.0.0"]
        );
    }
}
