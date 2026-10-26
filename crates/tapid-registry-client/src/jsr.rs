use crate::{
    HttpResponse, HttpTransport, MetadataError, RegistryArtifact, RegistryClientError,
    RegistryKind,
    artifact::download_artifact,
    response::{json_object, json_response},
    transport::request_url_is_safe,
};
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
        let name: PackageName = package
            .parse()
            .map_err(|_| MetadataError::InvalidPackageName(package.into()))
            .map_err(RegistryClientError::Metadata)?;
        let compatibility = compatibility_name(package)?;
        if self.origin.as_str() != "https://jsr.io" {
            return Err(RegistryClientError::Metadata(
                MetadataError::InvalidRegistry(self.origin.to_string()),
            ));
        }
        let response = self
            .transport
            .get_with_accept(
                &format!("https://npm.jsr.io/{compatibility}"),
                "application/vnd.npm.install-v1+json",
            )
            .map_err(RegistryClientError::Transport)?;
        let root = json_object(json_response(&response)?).map_err(RegistryClientError::Metadata)?;
        if root.get("name").and_then(serde_json::Value::as_str) != Some(compatibility.as_str()) {
            return Err(RegistryClientError::Metadata(
                MetadataError::ConflictingField("name".into()),
            ));
        }
        let versions = root
            .get("versions")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                RegistryClientError::Metadata(MetadataError::MissingField("versions".into()))
            })?;
        for (version, entry) in versions {
            if version.parse::<PackageVersion>().is_err() {
                continue;
            }
            if entry.get("name").and_then(serde_json::Value::as_str) != Some(compatibility.as_str())
                || entry.get("version").and_then(serde_json::Value::as_str)
                    != Some(version.as_str())
            {
                return Err(RegistryClientError::Metadata(
                    MetadataError::ConflictingField("version identity".into()),
                ));
            }
            if entry.pointer("/dist/integrity").is_none() {
                return Err(RegistryClientError::Metadata(
                    MetadataError::UnsupportedIntegrity(version.clone()),
                ));
            }
            let artifact = entry
                .pointer("/dist/tarball")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    RegistryClientError::Metadata(MetadataError::MissingField(
                        "dist.tarball".into(),
                    ))
                })?;
            validate_artifact_url(artifact, &compatibility, version)?;
        }
        let mut artifacts = crate::npm::parse_npm_object(
            &self.origin,
            &compatibility.parse().expect("validated compatibility name"),
            root,
            false,
        )?;
        for artifact in &mut artifacts {
            artifact.identity.name = name.clone();
            artifact.registry_kind = RegistryKind::Jsr;
        }
        Ok(artifacts)
    }
    pub fn download_artifact(
        &self,
        artifact_url: &str,
    ) -> Result<HttpResponse, RegistryClientError> {
        download_artifact(&self.transport, artifact_url)
    }
}

/// Validates a safely extracted generated manifest against its selected JSR artifact.
/// Callers must verify the archive's declared integrity before invoking this check.
pub fn validate_jsr_artifact_manifest(
    package: &str,
    version: &str,
    artifact_url: &str,
    manifest: &[u8],
) -> Result<(), RegistryClientError> {
    let compatibility = compatibility_name(package)?;
    let revision = validate_artifact_url(artifact_url, &compatibility, version)?;
    let manifest = json_object(manifest).map_err(RegistryClientError::Metadata)?;
    if manifest.get("name").and_then(serde_json::Value::as_str) != Some(compatibility.as_str())
        || manifest.get("version").and_then(serde_json::Value::as_str) != Some(version)
        || manifest
            .get("_jsr_revision")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(revision))
    {
        return Err(RegistryClientError::Metadata(
            MetadataError::ConflictingField("JSR archive identity or revision".into()),
        ));
    }
    Ok(())
}

fn compatibility_name(package: &str) -> Result<String, RegistryClientError> {
    let invalid =
        || RegistryClientError::Metadata(MetadataError::InvalidPackageName(package.into()));
    let (scope, name) = package
        .strip_prefix('@')
        .and_then(|name| name.split_once('/'))
        .ok_or_else(invalid)?;
    // JSR names cannot contain underscores: accepting them makes the generated mapping ambiguous.
    if scope.len() > 20
        || name.len() > 58
        || [scope, name].iter().any(|part| {
            part.len() < 2
                || part.starts_with('-')
                || part.ends_with('-')
                || part.contains("--")
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        return Err(invalid());
    }
    Ok(format!("@jsr/{scope}__{name}"))
}

fn validate_artifact_url(
    artifact: &str,
    compatibility: &str,
    version: &str,
) -> Result<u32, RegistryClientError> {
    let invalid = || RegistryClientError::Metadata(MetadataError::InvalidArtifact(artifact.into()));
    let url = Url::parse(artifact).map_err(|_| invalid())?;
    let parts: Vec<_> = url.path().split('/').collect();
    if !request_url_is_safe(artifact, &url)
        || url.origin().ascii_serialization() != "https://npm.jsr.io"
        || url.query().is_some()
        || url.fragment().is_some()
        || parts.len() != 6
        || parts[1] != "~"
        || parts[2].is_empty()
        || !parts[2].bytes().all(|byte| byte.is_ascii_digit())
        || parts[2].parse::<u32>().is_err()
        || format!("{}/{}", parts[3], parts[4]) != compatibility
        || parts[5] != format!("{version}.tgz")
    {
        return Err(invalid());
    }
    parts[2].parse().map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransportError;
    use serde_json::{Value, json};
    struct Fake(Value);
    impl HttpTransport for Fake {
        fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
            assert_eq!(url, "https://npm.jsr.io/@jsr/std__path");
            Ok(HttpResponse {
                status: 200,
                content_type: Some("application/json".into()),
                body: self.0.to_string().into_bytes(),
            })
        }
    }
    fn metadata() -> Value {
        json!({"name":"@jsr/std__path","versions":{"1.0.0":{
            "name":"@jsr/std__path", "version":"1.0.0",
            "dist":{"tarball":"https://npm.jsr.io/~/11/@jsr/std__path/1.0.0.tgz", "integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="},
            "dependencies":{"@jsr/std__assert":"^1.0.0", "foo":"^1.0.0"},
            "peerDependencies":{"bar":"^2.0.0"}, "peerDependenciesMeta":{"bar":{"optional":true}},
            "optionalDependencies":{"foo":"^2.0.0"}, "os":["linux"]
        }}})
    }
    fn fetch(body: Value) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
        JsrRegistry::new(Fake(body), "https://jsr.io".parse().unwrap()).fetch("@std/path")
    }
    #[test]
    fn jsr_archive_manifest_is_bound_to_selected_artifact() {
        let url = "https://npm.jsr.io/~/11/@jsr/std__path/1.0.0.tgz";
        let valid = json!({"name":"@jsr/std__path", "version":"1.0.0", "_jsr_revision":11});
        assert!(
            validate_jsr_artifact_manifest("@std/path", "1.0.0", url, valid.to_string().as_bytes())
                .is_ok()
        );
        for field in ["name", "version", "_jsr_revision"] {
            let mut invalid = valid.clone();
            invalid[field] = json!("wrong");
            assert!(
                validate_jsr_artifact_manifest(
                    "@std/path",
                    "1.0.0",
                    url,
                    invalid.to_string().as_bytes()
                )
                .is_err()
            );
            invalid.as_object_mut().unwrap().remove(field);
            assert!(
                validate_jsr_artifact_manifest(
                    "@std/path",
                    "1.0.0",
                    url,
                    invalid.to_string().as_bytes()
                )
                .is_err()
            );
        }
        assert!(validate_jsr_artifact_manifest("@std/path", "1.0.0", url, b"[]").is_err());
    }
    #[test]
    fn jsr_fetches_compatibility_metadata_with_logical_identity() {
        let artifacts = fetch(metadata()).unwrap();
        let artifact = &artifacts[0];
        assert_eq!(artifact.identity.name.to_string(), "@std/path");
        assert_eq!(artifact.identity.registry.to_string(), "https://jsr.io");
        assert_eq!(artifact.registry_kind, RegistryKind::Jsr);
        assert_eq!(
            artifact.artifact_url,
            "https://npm.jsr.io/~/11/@jsr/std__path/1.0.0.tgz"
        );
        assert_eq!(artifact.dependencies.len(), 1);
        assert!(
            artifact
                .dependencies
                .contains_key(&"@jsr/std__assert".parse().unwrap())
        );
        assert_eq!(
            artifact.optional_dependencies[&"foo".parse().unwrap()],
            "^2.0.0"
        );
        assert_eq!(
            artifact.peer_dependencies[&"bar".parse().unwrap()],
            "^2.0.0"
        );
        assert!(
            artifact
                .optional_peer_dependencies
                .contains(&"bar".parse().unwrap())
        );
        assert_eq!(artifact.platform.os, ["linux"]);
    }
    #[test]
    fn jsr_rejects_missing_and_malformed_integrity() {
        for value in [
            Value::Null,
            json!("sha256-deadbeef"),
            json!("sha512-bad"),
            json!(12),
        ] {
            let mut body = metadata();
            body["versions"]["1.0.0"]["dist"]["integrity"] = value;
            assert!(fetch(body).is_err());
        }
        let mut body = metadata();
        body["versions"]["1.0.0"]["dist"]
            .as_object_mut()
            .unwrap()
            .remove("integrity");
        assert!(matches!(
            fetch(body),
            Err(RegistryClientError::Metadata(
                MetadataError::UnsupportedIntegrity(_)
            ))
        ));
    }
    #[test]
    fn jsr_rejects_conflicting_metadata_identity() {
        for pointer in ["/name", "/versions/1.0.0/name", "/versions/1.0.0/version"] {
            let mut body = metadata();
            *body.pointer_mut(pointer).unwrap() = json!("unrelated");
            assert!(fetch(body).is_err(), "accepted {pointer}");
        }
    }
    #[test]
    fn jsr_rejects_artifact_origin_path_and_canonicalization_attacks() {
        for url in [
            "https://registry.npmjs.org/~/11/@jsr/std__path/1.0.0.tgz",
            "https://npm.jsr.io/~/11/@jsr/std__assert/1.0.0.tgz",
            "https://npm.jsr.io/~/11/@jsr/std__path/2.0.0.tgz",
            "https://npm.jsr.io/~/x/@jsr/std__path/1.0.0.tgz",
            "https://npm.jsr.io/~/11/@jsr/std__path/1.0.0.tgz?other",
            "https://npm.jsr.io/~/11/@jsr/std__path/1.0.0.tgz#other",
            "https://npm.jsr.io:443/~/11/@jsr/std__path/1.0.0.tgz",
            "https://NPM.JSR.IO/~/11/@jsr/std__path/1.0.0.tgz",
            "https://npm.jsr.io/~/11/@jsr/%73td__path/1.0.0.tgz",
            "http://npm.jsr.io/~/11/@jsr/std__path/1.0.0.tgz",
            "https://npm.jsr.io/archive.tgz",
        ] {
            let mut body = metadata();
            body["versions"]["1.0.0"]["dist"]["tarball"] = json!(url);
            assert!(
                matches!(
                    fetch(body),
                    Err(RegistryClientError::Metadata(
                        MetadataError::InvalidArtifact(_)
                    ))
                ),
                "accepted {url}"
            );
        }
    }
    #[test]
    fn jsr_rejects_ambiguous_names_before_transport() {
        for name in [
            "foo",
            "@std/a__b",
            "@a__b/c",
            "@Std/path",
            "@std/path/extra",
            "@-std/path",
            "@std/a--b",
            "@s/foo",
        ] {
            assert!(
                JsrRegistry::new(Fake(metadata()), "https://jsr.io".parse().unwrap())
                    .fetch(name)
                    .is_err()
            );
        }
    }
    #[test]
    fn jsr_rejects_malformed_metadata_and_dependencies() {
        for pointer in [
            "/versions",
            "/versions/1.0.0",
            "/versions/1.0.0/dependencies",
            "/versions/1.0.0/peerDependencies",
            "/versions/1.0.0/optionalDependencies",
        ] {
            let mut body = metadata();
            *body.pointer_mut(pointer).unwrap() = json!([]);
            assert!(fetch(body).is_err(), "accepted {pointer}");
        }
    }
    #[test]
    fn jsr_sorts_versions_and_ignores_invalid_version_keys() {
        let mut body = metadata();
        let mut newer = body["versions"]["1.0.0"].clone();
        newer["version"] = json!("2.0.0");
        newer["dist"]["tarball"] = json!("https://npm.jsr.io/~/12/@jsr/std__path/2.0.0.tgz");
        body["versions"]["2.0.0"] = newer;
        body["versions"]["not-semver"] = Value::Null;
        let artifacts = fetch(body).unwrap();
        assert_eq!(
            artifacts
                .iter()
                .map(|a| a.identity.version.to_string())
                .collect::<Vec<_>>(),
            ["2.0.0", "1.0.0"]
        );
    }
}
