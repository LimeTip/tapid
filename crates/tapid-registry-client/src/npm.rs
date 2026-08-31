use crate::{
    HttpResponse, HttpTransport, MetadataError, PackagePlatform, RegistryArtifact,
    RegistryClientError, RegistryKind, RegistryPackageId, artifact::download_artifact,
    transport::request_url_is_safe,
};
use std::collections::BTreeMap;
use tapid_core::{PackageName, PackageVersion, RegistryOrigin};
use url::Url;

const NPM_INSTALL_V1_ACCEPT: &str = "application/vnd.npm.install-v1+json";

/// Read-only npm metadata and artifact client over an injected transport.
pub struct NpmRegistry<T> {
    transport: T,
    origin: RegistryOrigin,
}
impl<T: HttpTransport> NpmRegistry<T> {
    /// Creates a client bound to one canonical npm registry origin.
    pub fn new(transport: T, origin: RegistryOrigin) -> Self {
        Self { transport, origin }
    }
    /// Fetches abbreviated npm metadata and excludes versions without integrity.
    pub fn fetch(&self, package: &str) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
        self.fetch_with_options(package, false)
    }
    /// Fetches abbreviated npm metadata with an explicit integrity compatibility policy.
    ///
    /// When `allow_missing_integrity` is false, versions without registry-declared
    /// SHA-512 integrity are excluded. Setting it to true preserves those versions
    /// for a caller that separately warns and records the weaker trust property.
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
            .get_with_accept(&url, NPM_INSTALL_V1_ACCEPT)
            .map_err(RegistryClientError::Transport)?;
        if response.status == 404 {
            // A package may exist only in obsolete dependency metadata. Treat
            // an exact not-found response as no candidates so graph selection
            // decides whether the missing package is relevant.
            return Ok(Vec::new());
        }
        parse_npm(
            &self.origin,
            &name,
            json_response(&response)?,
            allow_missing_integrity,
        )
    }
    /// Downloads one validated artifact URL through this registry's transport policy.
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
    let Some(versions) = root.get("versions") else {
        let is_tombstone = root.get("name").and_then(|value| value.as_str())
            == Some(name.to_string().as_str())
            && root
                .get("modified")
                .and_then(|value| value.as_str())
                .is_some_and(|modified| !modified.is_empty())
            && root.keys().all(|key| key == "name" || key == "modified");
        if is_tombstone {
            // npm's abbreviated representation keeps a narrow name/modified
            // tombstone for a fully unpublished package.
            return Ok(Vec::new());
        }
        return Err(RegistryClientError::Metadata(MetadataError::MissingField(
            "versions".into(),
        )));
    };
    let versions = versions.as_object().ok_or_else(|| {
        RegistryClientError::Metadata(MetadataError::InvalidJson(
            "versions must be an object".into(),
        ))
    })?;
    let mut artifacts = Vec::new();
    for (key, value) in versions {
        let version = key.parse::<PackageVersion>().map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidVersion(key.clone()))
        })?;
        let version_entry = value.as_object().ok_or_else(|| {
            RegistryClientError::Metadata(MetadataError::InvalidJson(
                "version entry must be an object".into(),
            ))
        })?;

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
            // Historical npm records may predate integrity metadata. Exclude those
            // records rather than making newer verifiable releases unusable.
            None => continue,
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
        if !request_url_is_safe(artifact_url, &parsed_artifact_url) {
            return Err(RegistryClientError::Metadata(
                MetadataError::InvalidArtifact(artifact_url.into()),
            ));
        }
        let artifact_url = artifact_url.to_owned();
        artifacts.push(RegistryArtifact {
            identity: RegistryPackageId::new(origin.clone(), name.clone(), version),
            artifact_url,
            integrity,
            dependencies: parse_dependencies(version_entry.get("dependencies"))?,
            optional_dependencies: parse_dependencies(version_entry.get("optionalDependencies"))?,
            platform: PackagePlatform {
                os: parse_platform_list(version_entry.get("os"), "os")?,
                cpu: parse_platform_list(version_entry.get("cpu"), "cpu")?,
                libc: parse_platform_list(version_entry.get("libc"), "libc")?,
            },
            registry_kind: RegistryKind::Npm,
        });
    }
    artifacts.sort_by_key(|artifact| std::cmp::Reverse(artifact.identity.version.clone()));
    Ok(artifacts)
}

fn parse_platform_list(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<Vec<String>, RegistryClientError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values: Vec<&str> = if let Some(value) = value.as_str() {
        vec![value]
    } else if let Some(values) = value.as_array() {
        values
            .iter()
            .map(serde_json::Value::as_str)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                RegistryClientError::Metadata(MetadataError::InvalidDependency(field.into()))
            })?
    } else {
        return Err(RegistryClientError::Metadata(
            MetadataError::InvalidDependency(field.into()),
        ));
    };
    if values.len() > 32
        || values.iter().any(|value| {
            value.is_empty()
                || value.len() > 32
                || !value.is_ascii()
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_!.-".contains(&byte))
        })
    {
        return Err(RegistryClientError::Metadata(
            MetadataError::InvalidDependency(field.into()),
        ));
    }
    Ok(values.into_iter().map(str::to_owned).collect())
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
            let requirement = requirement.as_str().ok_or_else(|| {
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

    struct AcceptAwareFake {
        body: Vec<u8>,
        url: String,
    }
    impl HttpTransport for AcceptAwareFake {
        fn get(&self, _url: &str) -> Result<HttpResponse, TransportError> {
            panic!("npm metadata must use the accept-aware transport method")
        }

        fn get_with_accept(&self, url: &str, accept: &str) -> Result<HttpResponse, TransportError> {
            assert_eq!(url, self.url);
            assert_eq!(accept, "application/vnd.npm.install-v1+json");
            Ok(HttpResponse {
                status: 200,
                content_type: Some("application/json".into()),
                body: self.body.clone(),
            })
        }
    }

    #[test]
    fn npm_metadata_requests_the_abbreviated_install_representation() {
        let body = br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

        let artifacts = NpmRegistry::new(
            AcceptAwareFake {
                body: body.to_vec(),
                url: "https://registry.npmjs.org/foo".into(),
            },
            origin,
        )
        .fetch("foo")
        .unwrap();

        assert_eq!(artifacts.len(), 1);
    }

    #[test]
    fn npm_maps_platform_compatible_optional_dependency_metadata() {
        let body = br#"{"name":"parent","versions":{"1.0.0":{"name":"parent","version":"1.0.0","optionalDependencies":{"native":"1.0.0"},"os":["darwin"],"cpu":["arm64"],"libc":["glibc"],"dist":{"tarball":"https://cdn.example/parent.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let artifacts = NpmRegistry::new(fake(body, "https://registry.npmjs.org/parent"), origin)
            .fetch("parent")
            .unwrap();

        assert_eq!(
            artifacts[0]
                .optional_dependencies
                .get(&"native".parse().unwrap()),
            Some(&"1.0.0".to_owned())
        );
        assert_eq!(artifacts[0].platform.os, vec!["darwin"]);
        assert_eq!(artifacts[0].platform.cpu, vec!["arm64"]);
        assert_eq!(artifacts[0].platform.libc, vec!["glibc"]);
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
    fn npm_preserves_empty_historical_dependency_ranges_for_candidate_filtering() {
        let body = br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","dependencies":{"bar":""},"dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

        let artifacts = NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin)
            .fetch("foo")
            .unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            artifacts[0].dependencies.get(&"bar".parse().unwrap()),
            Some(&String::new())
        );
    }

    #[test]
    fn npm_unpublished_package_has_no_install_candidates() {
        let body = br#"{"name":"foo","modified":"2026-01-01T00:00:00.000Z"}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

        let artifacts = NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin)
            .fetch("foo")
            .unwrap();

        assert!(artifacts.is_empty());
    }

    #[test]
    fn npm_rejects_arbitrary_metadata_without_versions() {
        for body in [
            br#"{}"#.as_slice(),
            br#"{"name":"foo","error":"temporary failure"}"#.as_slice(),
            br#"{"name":"foo","modified":"2026-01-01T00:00:00.000Z","extra":true}"#.as_slice(),
        ] {
            let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
            let result =
                NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin).fetch("foo");

            assert!(matches!(
                result,
                Err(RegistryClientError::Metadata(MetadataError::MissingField(field)))
                    if field == "versions"
            ));
        }
    }

    #[test]
    fn npm_rejects_non_object_versions_metadata() {
        let body = br#"{"name":"foo","versions":[]}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

        let result =
            NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin).fetch("foo");

        assert!(matches!(
            result,
            Err(RegistryClientError::Metadata(MetadataError::InvalidJson(message)))
                if message == "versions must be an object"
        ));
    }

    #[test]
    fn npm_retains_valid_prereleases_for_requirement_selection() {
        let body = br#"{"name":"foo","versions":{"1.0.0-rc.1":{"name":"foo","version":"1.0.0-rc.1","dependencies":{"unenv":"2.0.0-rc.24"},"dist":{"tarball":"https://cdn.example/foo-rc.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}},"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

        let artifacts = NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin)
            .fetch("foo")
            .unwrap();

        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].identity.version.to_string(), "1.0.0");
        assert_eq!(artifacts[1].identity.version.to_string(), "1.0.0-rc.1");
        assert_eq!(
            artifacts[1]
                .dependencies
                .get(&"unenv".parse().unwrap())
                .map(String::as_str),
            Some("2.0.0-rc.24")
        );
    }

    #[test]
    fn npm_rejects_non_object_prerelease_entries() {
        let body = br#"{"name":"foo","versions":{"1.0.0-rc.1":null,"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

        let result =
            NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin).fetch("foo");

        assert!(matches!(
            result,
            Err(RegistryClientError::Metadata(MetadataError::InvalidJson(message)))
                if message == "version entry must be an object"
        ));
    }

    #[test]
    fn npm_skips_stable_historical_versions_without_integrity() {
        let body = br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"https://cdn.example/foo-old.tgz"}},"2.0.0":{"name":"foo","version":"2.0.0","dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#;
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

        let artifacts = NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin)
            .fetch("foo")
            .unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].identity.version.to_string(), "2.0.0");
    }

    #[test]
    fn npm_missing_integrity_produces_no_unverified_candidates() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let body = br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","dist":{"tarball":"https://cdn.example/foo.tgz"}}}}"#;

        let artifacts = NpmRegistry::new(fake(body, "https://registry.npmjs.org/foo"), origin)
            .fetch("foo")
            .unwrap();

        assert!(artifacts.is_empty());
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
    fn npm_rejects_noncanonical_artifact_urls() {
        for (case, artifact_url) in [
            ("leading space", " https://cdn.example/archive.tgz"),
            ("path space", "https://cdn.example/archive .tgz"),
            ("trailing newline", "https://cdn.example/archive.tgz\n"),
            ("uppercase scheme", "HTTPS://cdn.example/archive.tgz"),
            ("uppercase host", "https://CDN.EXAMPLE/archive.tgz"),
            ("backslash", "https://cdn.example\\archive.tgz"),
            ("empty userinfo", "https://@cdn.example/archive.tgz"),
            (
                "Unicode IDNA separator",
                "https://cdn\u{3002}example/archive.tgz",
            ),
            (
                "explicit default port",
                "https://cdn.example:443/archive.tgz",
            ),
            ("percent-encoded host", "https://%63dn.example/archive.tgz"),
            (
                "encoded dot path",
                "https://cdn.example/a/%2e%2e/archive.tgz",
            ),
            ("encoded control path", "https://cdn.example/%0Aarchive.tgz"),
            (
                "encoded unreserved path",
                "https://cdn.example/%61rchive.tgz",
            ),
        ] {
            let body = serde_json::json!({
                "name": "foo",
                "versions": {
                    "1.0.0": {
                        "name": "foo",
                        "version": "1.0.0",
                        "dist": {
                            "tarball": artifact_url,
                            "integrity": "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="
                        }
                    }
                }
            })
            .to_string();
            let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();

            let result = NpmRegistry::new(
                fake(body.as_bytes(), "https://registry.npmjs.org/foo"),
                origin,
            )
            .fetch("foo");

            assert!(
                matches!(
                    result,
                    Err(RegistryClientError::Metadata(MetadataError::InvalidArtifact(url)))
                        if url == artifact_url
                ),
                "npm accepted {case}: {artifact_url:?}"
            );
        }
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
    fn npm_package_not_found_has_no_install_candidates() {
        let origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let mut response = fake(
            br#"{"error":"Not found"}"#,
            "https://registry.npmjs.org/foo",
        );
        response.status = 404;

        let artifacts = NpmRegistry::new(response, origin).fetch("foo").unwrap();

        assert!(artifacts.is_empty());
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
