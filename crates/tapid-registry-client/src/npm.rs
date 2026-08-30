use crate::{
    HttpResponse, HttpTransport, MetadataError, RegistryArtifact, RegistryClientError,
    RegistryKind, RegistryPackageId, artifact::download_artifact,
};
use std::collections::BTreeMap;
use tapid_core::{PackageName, PackageVersion, RegistryOrigin};
use url::Url;

pub struct NpmRegistry<T> {
    transport: T,
    origin: RegistryOrigin,
}
impl<T: HttpTransport> NpmRegistry<T> {
    pub fn new(transport: T, origin: RegistryOrigin) -> Self {
        Self { transport, origin }
    }
    pub fn fetch(&self, package: &str) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
        self.fetch_with_options(package, false)
    }
    pub fn fetch_with_options(
        &self,
        package: &str,
        allow_missing_integrity: bool,
    ) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
        let name: PackageName = package.parse().map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidPackageName(package.into()))
        })?;
        let url = format!("{}/{}", self.origin, package.replace('/', "%2f"));
        let response = self
            .transport
            .get(&url)
            .map_err(RegistryClientError::Transport)?;
        if response.status == 404 {
            return Ok(Vec::new());
        }
        parse_npm(
            &self.origin,
            &name,
            json_response(&response)?,
            allow_missing_integrity,
        )
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

fn required_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str, MetadataError> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MetadataError::MissingField(key.into()))
}

fn parse_npm(
    origin: &RegistryOrigin,
    name: &PackageName,
    body: &[u8],
    allow_missing_integrity: bool,
) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
    let root = json_object(body).map_err(RegistryClientError::Metadata)?;
    if let Some(metadata_name) = root.get("name").and_then(|value| value.as_str())
        && metadata_name != name.to_string()
    {
        return Err(RegistryClientError::Metadata(
            MetadataError::ConflictingField("name".into()),
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
        let parsed_version = semver::Version::parse(key).map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidVersion(key.clone()))
        })?;
        if !parsed_version.pre.is_empty() {
            continue;
        }
        if !parsed_version.build.is_empty() {
            return Err(RegistryClientError::Metadata(
                MetadataError::InvalidVersion(key.clone()),
            ));
        }
        let version_entry = value.as_object().ok_or_else(|| {
            RegistryClientError::Metadata(MetadataError::InvalidJson(
                "version entry must be an object".into(),
            ))
        })?;
        let version = PackageVersion {
            major: parsed_version.major,
            minor: parsed_version.minor,
            patch: parsed_version.patch,
        };
        if version_entry
            .get("version")
            .and_then(|value| value.as_str())
            != Some(key)
        {
            return Err(RegistryClientError::Metadata(
                MetadataError::ConflictingField("version".into()),
            ));
        }
        if version_entry
            .get("name")
            .and_then(|value| value.as_str())
            .is_some_and(|metadata_name| metadata_name != name.to_string())
        {
            return Err(RegistryClientError::Metadata(
                MetadataError::ConflictingField("name".into()),
            ));
        }
        let dist = version_entry
            .get("dist")
            .and_then(|value| value.as_object())
            .ok_or_else(|| {
                RegistryClientError::Metadata(MetadataError::MissingField("dist".into()))
            })?;
        let artifact_url = required_str(dist, "tarball").map_err(RegistryClientError::Metadata)?;
        let integrity = match dist.get("integrity") {
            None if allow_missing_integrity => None,
            None => {
                return Err(RegistryClientError::Metadata(MetadataError::MissingField(
                    "dist.integrity".into(),
                )));
            }
            Some(value) => {
                let value = value.as_str().ok_or_else(|| {
                    RegistryClientError::Metadata(MetadataError::InvalidIntegrity(key.clone()))
                })?;
                Some(value.parse().map_err(|_| {
                    RegistryClientError::Metadata(MetadataError::InvalidIntegrity(value.into()))
                })?)
            }
        };
        let parsed_artifact_url = Url::parse(artifact_url).map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidArtifact(artifact_url.into()))
        })?;
        if parsed_artifact_url.scheme() != "https" {
            return Err(RegistryClientError::Metadata(
                MetadataError::InvalidArtifact(artifact_url.into()),
            ));
        }
        let artifact_url = parsed_artifact_url.to_string();
        artifacts.push(RegistryArtifact {
            identity: RegistryPackageId::new(origin.clone(), name.clone(), version),
            artifact_url,
            integrity,
            dependencies: parse_dependencies(version_entry.get("dependencies"))?,
            registry_kind: RegistryKind::Npm,
        });
    }
    artifacts.sort_by_key(|artifact| std::cmp::Reverse(artifact.identity.version));
    Ok(artifacts)
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
        status: u16,
        content_type: Option<String>,
    }
    impl HttpTransport for Fake {
        fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
            assert_eq!(url, self.url);
            Ok(HttpResponse {
                status: self.status,
                content_type: self.content_type.clone(),
                body: self.body.clone(),
            })
        }
    }
    fn fake(body: &[u8], url: &str) -> Fake {
        Fake {
            body: body.to_vec(),
            url: url.into(),
            status: 200,
            content_type: Some("application/json; charset=utf-8".into()),
        }
    }

    #[test]
    fn npm_metadata_maps_tarball_and_integrity() {
        let body = br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","dependencies":{"bar":"^2.0.0"},"dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let artifacts = NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin)
            .fetch("foo")
            .unwrap();
        assert_eq!(artifacts[0].artifact_url, "https://cdn.example/foo.tgz");
        assert!(artifacts[0].integrity.is_some());
        assert_eq!(
            artifacts[0].dependencies.get(&"bar".parse().unwrap()),
            Some(&"^2.0.0".to_owned())
        );
    }

    #[test]
    fn npm_skips_valid_prereleases_during_stable_resolution() {
        let body = br#"{"name":"foo","versions":{"1.0.0-rc.1":{"name":"foo","version":"1.0.0-rc.1","dist":{"tarball":"https://cdn.example/foo-rc.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}},"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

        let artifacts = NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin)
            .fetch("foo")
            .unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].identity.version.to_string(), "1.0.0");
    }

    #[test]
    fn npm_skips_non_object_prerelease_entries() {
        let body = br#"{"name":"foo","versions":{"1.0.0-rc.1":null,"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

        let artifacts = NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin)
            .fetch("foo")
            .unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].identity.version.to_string(), "1.0.0");
    }

    #[test]
    fn npm_missing_integrity_fails_closed() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let body = br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"https://cdn.example/foo.tgz"}}}}"#;
        let result =
            NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin).fetch("foo");
        assert!(matches!(
            result,
            Err(RegistryClientError::Metadata(MetadataError::MissingField(field)))
                if field == "dist.integrity"
        ));
    }

    #[test]
    fn npm_rejects_non_https_artifact_url() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let body = br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"http://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;

        let result =
            NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin).fetch("foo");

        assert!(matches!(
            result,
            Err(RegistryClientError::Metadata(MetadataError::InvalidArtifact(url)))
                if url == "http://cdn.example/foo.tgz"
        ));
    }

    #[test]
    fn npm_missing_integrity_can_be_explicitly_allowed() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let body = br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"https://cdn.example/foo.tgz"}}}}"#;
        let result = NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin)
            .fetch_with_options("foo", true)
            .unwrap();
        assert!(result[0].integrity.is_none());
    }

    #[test]
    fn npm_rejects_malformed_version_keys() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let body = br#"{"name":"foo","versions":{"not-semver":{"name":"foo","version":"not-semver","dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;

        let result =
            NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin).fetch("foo");

        assert!(matches!(
            result,
            Err(RegistryClientError::Metadata(MetadataError::InvalidVersion(version)))
                if version == "not-semver"
        ));
    }

    #[test]
    fn npm_rejects_malformed_version_keys_before_entry_validation() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let body = br#"{"name":"foo","versions":{"not-semver":null}}"#;

        let result =
            NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin).fetch("foo");

        assert!(matches!(
            result,
            Err(RegistryClientError::Metadata(MetadataError::InvalidVersion(version)))
                if version == "not-semver"
        ));
    }

    #[test]
    fn npm_rejects_stable_versions_with_build_metadata() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let body = br#"{"name":"foo","versions":{"1.0.0+build":{"name":"foo","version":"1.0.0+build","dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;

        let result =
            NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin).fetch("foo");

        assert!(matches!(
            result,
            Err(RegistryClientError::Metadata(MetadataError::InvalidVersion(version)))
                if version == "1.0.0+build"
        ));
    }

    #[test]
    fn malformed_npm_is_rejected() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let error = NpmRegistry::new(
            fake(br#"{"versions":[]}"#, "https://registry.npmjs.org/foo"),
            origin,
        )
        .fetch("foo");
        assert!(error.is_err());
    }

    #[test]
    fn npm_not_found_maps_to_empty_candidates() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let mut response = fake(br"not json", "https://registry.npmjs.org/missing");
        response.status = 404;
        response.content_type = None;

        let result = NpmRegistry::new(response, origin).fetch("missing").unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn npm_other_http_errors_remain_errors() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let mut response = fake(br"not json", "https://registry.npmjs.org/unavailable");
        response.status = 500;

        let result = NpmRegistry::new(response, origin).fetch("unavailable");

        assert!(matches!(
            result,
            Err(RegistryClientError::Metadata(MetadataError::HttpStatus(
                500
            )))
        ));
    }

    #[test]
    fn metadata_requires_success_and_json_content_type() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let mut response = fake(br#"{"versions":{}}"#, "https://registry.npmjs.org/foo");
        response.status = 204;
        assert!(matches!(
            NpmRegistry::new(response, origin.clone()).fetch("foo"),
            Err(RegistryClientError::Metadata(MetadataError::HttpStatus(
                204
            )))
        ));
        let mut response = fake(br#"{"versions":{}}"#, "https://registry.npmjs.org/foo");
        response.content_type = Some("text/plain".into());
        assert!(matches!(
            NpmRegistry::new(response, origin).fetch("foo"),
            Err(RegistryClientError::Metadata(
                MetadataError::UnsupportedContentType(_)
            ))
        ));
    }
}
