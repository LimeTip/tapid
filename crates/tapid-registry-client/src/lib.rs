//! Read-only registry metadata clients with an injected HTTP boundary.
#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, io::Read, time::Duration};
use tapid_core::{PackageIntegrity, PackageName, PackageVersion, RegistryOrigin};
use url::Url;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FetchMode {
    #[default]
    Online,
    Offline,
    Frozen,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RawRegistrySnapshot {
    pub registry: String,
    pub packages: Vec<RawPackageMetadata>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RawPackageMetadata {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub integrity: Option<String>,
    #[serde(default)]
    pub artifact: Option<String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrySnapshot {
    registry: RegistryOrigin,
    packages: BTreeMap<PackageName, Vec<PackageMetadata>>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageMetadata {
    pub identity: RegistryPackageId,
    pub integrity: Option<PackageIntegrity>,
    pub artifact: Option<String>,
    pub dependencies: BTreeMap<PackageName, String>,
    pub registry_kind: RegistryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryKind {
    Npm,
    Jsr,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RegistryPackageId {
    pub registry: RegistryOrigin,
    pub name: PackageName,
    pub version: PackageVersion,
}
impl RegistryPackageId {
    pub fn new(registry: RegistryOrigin, name: PackageName, version: PackageVersion) -> Self {
        Self {
            registry,
            name,
            version,
        }
    }
}
impl fmt::Display for RegistryPackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}@{}", self.registry, self.name, self.version)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataError {
    InvalidRegistry(String),
    InvalidPackageName(String),
    InvalidVersion(String),
    InvalidIntegrity(String),
    InvalidArtifact(String),
    InvalidDependency(String),
    UnsupportedIntegrity(String),
    InvalidJson(String),
    MissingField(String),
    ConflictingField(String),
    DuplicateVersion(String),
}
impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MetadataError {}

impl RegistrySnapshot {
    pub fn normalize(raw: RawRegistrySnapshot) -> Result<Self, MetadataError> {
        let registry: RegistryOrigin = raw
            .registry
            .parse()
            .map_err(|_| MetadataError::InvalidRegistry(raw.registry.clone()))?;
        let registry_kind = if registry.to_string() == "https://jsr.io" {
            RegistryKind::Jsr
        } else {
            RegistryKind::Npm
        };
        let mut packages = BTreeMap::new();
        for entry in raw.packages {
            let name: PackageName = entry
                .name
                .trim()
                .parse()
                .map_err(|_| MetadataError::InvalidPackageName(entry.name.clone()))?;
            let version: PackageVersion = entry
                .version
                .trim()
                .parse()
                .map_err(|_| MetadataError::InvalidVersion(entry.version.clone()))?;
            let integrity = entry
                .integrity
                .map(|v| v.parse().map_err(|_| MetadataError::InvalidIntegrity(v)))
                .transpose()?;
            let id = RegistryPackageId::new(registry.clone(), name.clone(), version);
            let candidates = packages.entry(name).or_insert_with(Vec::new);
            if candidates
                .iter()
                .any(|p: &PackageMetadata| p.identity.version == version)
            {
                return Err(MetadataError::DuplicateVersion(id.to_string()));
            }
            let artifact = entry
                .artifact
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty());
            if artifact.as_deref().is_some_and(|v| Url::parse(v).is_err()) {
                return Err(MetadataError::InvalidArtifact(id.to_string()));
            }
            let dependencies = entry
                .dependencies
                .into_iter()
                .map(|(name, requirement)| {
                    let package: PackageName = name
                        .parse()
                        .map_err(|_| MetadataError::InvalidDependency(name.clone()))?;
                    if requirement.trim().is_empty() {
                        return Err(MetadataError::InvalidDependency(name));
                    }
                    Ok((package, requirement))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            candidates.push(PackageMetadata {
                identity: id,
                integrity,
                artifact,
                dependencies,
                registry_kind,
            });
        }
        for candidates in packages.values_mut() {
            candidates.sort_by_key(|p| std::cmp::Reverse(p.identity.version));
        }
        Ok(Self { registry, packages })
    }
    pub fn registry(&self) -> &RegistryOrigin {
        &self.registry
    }
    pub fn packages(&self) -> &BTreeMap<PackageName, Vec<PackageMetadata>> {
        &self.packages
    }
    pub fn candidates(&self, name: &PackageName) -> &[PackageMetadata] {
        self.packages.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// A deliberately small boundary: production uses HTTPS, tests can use a local server.
pub trait HttpTransport: Send + Sync {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError>;
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
    InvalidUrl(String),
    OriginNotAllowed(String),
    Http(String),
    TooLarge { limit: usize },
    InvalidResponse(String),
}
impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for TransportError {}

/// HTTPS transport with an allow-list and bounded response body. It sends no credentials.
pub struct HttpsTransport {
    client: reqwest::blocking::Client,
    allowed_origins: Vec<Origin>,
    max_response_bytes: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}
impl Origin {
    fn parse(s: &str) -> Result<Self, TransportError> {
        let u = Url::parse(s).map_err(|_| TransportError::InvalidUrl(s.into()))?;
        if u.scheme() != "https" {
            return Err(TransportError::OriginNotAllowed(s.into()));
        }
        Ok(Self {
            scheme: u.scheme().into(),
            host: u.host_str().unwrap_or_default().to_ascii_lowercase(),
            port: u.port_or_known_default(),
        })
    }
    fn of(u: &Url) -> Self {
        Self {
            scheme: u.scheme().into(),
            host: u.host_str().unwrap_or_default().to_ascii_lowercase(),
            port: u.port_or_known_default(),
        }
    }
}
impl HttpsTransport {
    pub fn new<I, S>(
        allowed_origins: I,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, TransportError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let allowed_origins = allowed_origins
            .into_iter()
            .map(|s| Origin::parse(s.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        if allowed_origins.is_empty() || max_response_bytes == 0 {
            return Err(TransportError::InvalidResponse(
                "non-empty origins and positive response limit required".into(),
            ));
        }
        let policy = reqwest::redirect::Policy::custom({
            let allowed = allowed_origins.clone();
            move |attempt| {
                let from = attempt.previous().last().map(Origin::of);
                let to = Origin::of(attempt.url());
                if from.as_ref().is_some_and(|o| *o != to) || !allowed.contains(&to) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .redirect(policy)
            .build()
            .map_err(|e| TransportError::Http(e.to_string()))?;
        Ok(Self {
            client,
            allowed_origins,
            max_response_bytes,
        })
    }
    pub fn standard() -> Result<Self, TransportError> {
        Self::new(
            [
                "https://registry.npmjs.org",
                "https://jsr.io",
                "https://npm.jsr.io",
            ],
            Duration::from_secs(20),
            4 * 1024 * 1024,
        )
    }
}
impl HttpTransport for HttpsTransport {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        let parsed = Url::parse(url).map_err(|_| TransportError::InvalidUrl(url.into()))?;
        if parsed.scheme() != "https"
            || !self
                .allowed_origins
                .iter()
                .any(|o| *o == Origin::of(&parsed))
        {
            return Err(TransportError::OriginNotAllowed(url.into()));
        }
        let response = self
            .client
            .get(parsed)
            .send()
            .map_err(|e| TransportError::Http(e.to_string()))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        response
            .take(self.max_response_bytes as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| TransportError::Http(e.to_string()))?;
        if body.len() > self.max_response_bytes {
            return Err(TransportError::TooLarge {
                limit: self.max_response_bytes,
            });
        }
        Ok(HttpResponse {
            status,
            content_type,
            body,
        })
    }
}

pub trait RegistryTransport {
    type Error: std::error::Error + Send + Sync + 'static;
    fn fetch(&self, registry: &RegistryOrigin) -> Result<RawRegistrySnapshot, Self::Error>;
}
pub struct RegistryClient<T> {
    transport: T,
}
impl<T> RegistryClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}
impl<T: RegistryTransport> RegistryClient<T> {
    pub fn snapshot(
        &self,
        registry: &RegistryOrigin,
        mode: FetchMode,
    ) -> Result<RegistrySnapshot, ClientError<T::Error>> {
        if mode != FetchMode::Online {
            return Err(ClientError::NetworkDisabled(mode));
        }
        RegistrySnapshot::normalize(
            self.transport
                .fetch(registry)
                .map_err(ClientError::Transport)?,
        )
        .map_err(ClientError::Metadata)
    }
}
#[derive(Debug)]
pub enum ClientError<E> {
    Transport(E),
    Metadata(MetadataError),
    NetworkDisabled(FetchMode),
}
impl<E: fmt::Display> fmt::Display for ClientError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "registry transport failed: {e}"),
            Self::Metadata(e) => write!(f, "invalid registry metadata: {e}"),
            Self::NetworkDisabled(m) => write!(f, "network disabled in {m:?} mode"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for ClientError<E> {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryArtifact {
    pub identity: RegistryPackageId,
    pub artifact_url: String,
    pub integrity: Option<PackageIntegrity>,
    pub dependencies: BTreeMap<PackageName, String>,
    pub registry_kind: RegistryKind,
}

pub struct NpmRegistry<T> {
    transport: T,
    origin: RegistryOrigin,
}
impl<T: HttpTransport> NpmRegistry<T> {
    pub fn new(transport: T, origin: RegistryOrigin) -> Self {
        Self { transport, origin }
    }
    pub fn fetch(&self, package: &str) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
        let name: PackageName = package.parse().map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidPackageName(package.into()))
        })?;
        let url = format!("{}/{}", self.origin, package.replace('/', "%2f"));
        let response = self
            .transport
            .get(&url)
            .map_err(RegistryClientError::Transport)?;
        parse_npm(&self.origin, &name, &response.body)
    }
    pub fn download_artifact(
        &self,
        artifact_url: &str,
    ) -> Result<HttpResponse, RegistryClientError> {
        download_artifact(&self.transport, artifact_url)
    }
}

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
        let url = format!(
            "{}/{}/meta.json",
            self.origin,
            package.trim_start_matches('@')
        );
        let response = self
            .transport
            .get(&url)
            .map_err(RegistryClientError::Transport)?;
        parse_jsr(&self.origin, &name, &response.body)
    }
    pub fn download_artifact(
        &self,
        artifact_url: &str,
    ) -> Result<HttpResponse, RegistryClientError> {
        download_artifact(&self.transport, artifact_url)
    }
}
#[derive(Debug)]
pub enum RegistryClientError {
    Transport(TransportError),
    Metadata(MetadataError),
}
impl fmt::Display for RegistryClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for RegistryClientError {}

fn json_object(body: &[u8]) -> Result<serde_json::Map<String, serde_json::Value>, MetadataError> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| MetadataError::InvalidJson(e.to_string()))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| MetadataError::InvalidJson("metadata must be an object".into()))
}

fn download_artifact<T: HttpTransport>(
    transport: &T,
    artifact_url: &str,
) -> Result<HttpResponse, RegistryClientError> {
    let url = Url::parse(artifact_url).map_err(|_| {
        RegistryClientError::Metadata(MetadataError::InvalidArtifact(artifact_url.into()))
    })?;
    if url.scheme() != "https" {
        return Err(RegistryClientError::Transport(
            TransportError::OriginNotAllowed(artifact_url.into()),
        ));
    }
    transport
        .get(artifact_url)
        .map_err(RegistryClientError::Transport)
}
fn required_str<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str, MetadataError> {
    obj.get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| MetadataError::MissingField(key.into()))
}
fn parse_npm(
    origin: &RegistryOrigin,
    name: &PackageName,
    body: &[u8],
) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
    let root = json_object(body).map_err(RegistryClientError::Metadata)?;
    if let Some(n) = root.get("name").and_then(|v| v.as_str()) {
        if n != name.to_string() {
            return Err(RegistryClientError::Metadata(
                MetadataError::ConflictingField("name".into()),
            ));
        }
    }
    let versions = root
        .get("versions")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            RegistryClientError::Metadata(MetadataError::MissingField("versions".into()))
        })?;
    let mut out = Vec::new();
    for (key, value) in versions {
        let v = value.as_object().ok_or_else(|| {
            RegistryClientError::Metadata(MetadataError::InvalidJson(
                "version entry must be an object".into(),
            ))
        })?;
        let version: PackageVersion = key.parse().map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidVersion(key.clone()))
        })?;
        if v.get("version").and_then(|x| x.as_str()) != Some(key) {
            return Err(RegistryClientError::Metadata(
                MetadataError::ConflictingField("version".into()),
            ));
        }
        if v.get("name")
            .and_then(|x| x.as_str())
            .is_some_and(|n| n != name.to_string())
        {
            return Err(RegistryClientError::Metadata(
                MetadataError::ConflictingField("name".into()),
            ));
        }
        let dist = v.get("dist").and_then(|x| x.as_object()).ok_or_else(|| {
            RegistryClientError::Metadata(MetadataError::MissingField("dist".into()))
        })?;
        let artifact_url = required_str(dist, "tarball").map_err(RegistryClientError::Metadata)?;
        let integrity = dist
            .get("integrity")
            .map(|x| {
                x.as_str().ok_or_else(|| {
                    RegistryClientError::Metadata(MetadataError::InvalidIntegrity(key.clone()))
                })
            })
            .transpose()?
            .map(|x| {
                x.parse().map_err(|_| {
                    RegistryClientError::Metadata(MetadataError::InvalidIntegrity(x.into()))
                })
            })
            .transpose()?;
        let artifact_url = Url::parse(artifact_url)
            .map_err(|_| {
                RegistryClientError::Metadata(MetadataError::InvalidArtifact(artifact_url.into()))
            })?
            .to_string();
        out.push(RegistryArtifact {
            identity: RegistryPackageId::new(origin.clone(), name.clone(), version),
            artifact_url,
            integrity,
            dependencies: parse_dependencies(v.get("dependencies"))?,
            registry_kind: RegistryKind::Npm,
        });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.identity.version));
    Ok(out)
}
fn parse_jsr(
    origin: &RegistryOrigin,
    name: &PackageName,
    body: &[u8],
) -> Result<Vec<RegistryArtifact>, RegistryClientError> {
    let root = json_object(body).map_err(RegistryClientError::Metadata)?;
    let versions = root
        .get("versions")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            RegistryClientError::Metadata(MetadataError::MissingField("versions".into()))
        })?;
    let mut out = Vec::new();
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
        let integrity_value = version_object
            .get("integrity")
            .or_else(|| version_object.get("dist").and_then(|v| v.get("integrity")))
            .or_else(|| version_object.get("npm").and_then(|v| v.get("integrity")));
        let integrity = integrity_value
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                RegistryClientError::Metadata(MetadataError::UnsupportedIntegrity(key.clone()))
            })?
            .parse()
            .map_err(|_| {
                RegistryClientError::Metadata(MetadataError::InvalidIntegrity(key.clone()))
            })?;
        let encoded = name.to_string().trim_start_matches('@').replace('/', "__");
        let artifact_url = format!("https://npm.jsr.io/~/{encoded}/{key}.tgz");
        out.push(RegistryArtifact {
            identity: RegistryPackageId::new(origin.clone(), name.clone(), version),
            artifact_url,
            integrity: Some(integrity),
            dependencies: parse_dependencies(version_object.get("dependencies"))?,
            registry_kind: RegistryKind::Jsr,
        });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.identity.version));
    Ok(out)
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
                .filter(|s| !s.trim().is_empty())
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
    struct Fake {
        body: Vec<u8>,
        url: String,
    }
    impl HttpTransport for Fake {
        fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
            assert_eq!(url, self.url);
            Ok(HttpResponse {
                status: 200,
                content_type: Some("application/json".into()),
                body: self.body.clone(),
            })
        }
    }
    #[test]
    fn npm_metadata_maps_tarball_and_integrity() {
        let b = br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","dependencies":{"bar":"^2.0.0"},"dist":{"tarball":"https://cdn.example/foo.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#.to_vec();
        let r: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let x = NpmRegistry::new(
            Fake {
                body: b,
                url: "https://registry.npmjs.org/foo".into(),
            },
            r,
        )
        .fetch("foo")
        .unwrap();
        assert_eq!(x[0].artifact_url, "https://cdn.example/foo.tgz");
        assert!(x[0].integrity.is_some());
        assert_eq!(
            x[0].dependencies.get(&"bar".parse().unwrap()),
            Some(&"^2.0.0".to_owned())
        );
    }
    #[test]
    fn jsr_without_sha512_integrity_fails_closed() {
        let r: RegistryOrigin = "https://jsr.io".parse().unwrap();
        let e = JsrRegistry::new(
            Fake {
                body: br#"{"versions":{"1.0.0":{}}}"#.to_vec(),
                url: "https://jsr.io/std/path/meta.json".into(),
            },
            r,
        )
        .fetch("@std/path");
        assert!(matches!(
            e,
            Err(RegistryClientError::Metadata(
                MetadataError::UnsupportedIntegrity(_)
            ))
        ));
    }
    #[test]
    fn artifact_download_rejects_non_https_before_transport() {
        let r: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let client = NpmRegistry::new(
            Fake {
                body: vec![],
                url: "unused".into(),
            },
            r,
        );
        assert!(matches!(
            client.download_artifact("http://evil.example/a.tgz"),
            Err(RegistryClientError::Transport(
                TransportError::OriginNotAllowed(_)
            ))
        ));
    }
    #[test]
    fn malformed_npm_is_rejected() {
        let r: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
        let e = NpmRegistry::new(
            Fake {
                body: br#"{"versions":[]}"#.to_vec(),
                url: "https://registry.npmjs.org/foo".into(),
            },
            r,
        )
        .fetch("foo");
        assert!(e.is_err());
    }
    #[test]
    fn jsr_maps_supported_scope() {
        let r: RegistryOrigin = "https://jsr.io".parse().unwrap();
        let x = JsrRegistry::new(
            Fake {
                body: br#"{"versions":{"1.0.0":{"integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}"#.to_vec(),
                url: "https://jsr.io/std/path/meta.json".into(),
            },
            r,
        )
        .fetch("@std/path")
        .unwrap();
        assert_eq!(
            x[0].artifact_url,
            "https://npm.jsr.io/~/std__path/1.0.0.tgz"
        );
    }
    #[test]
    fn normalization_sorts_versions() {
        let p = |v: &str| RawPackageMetadata {
            name: "foo".into(),
            version: v.into(),
            integrity: None,
            artifact: None,
            dependencies: BTreeMap::new(),
        };
        let s = RegistrySnapshot::normalize(RawRegistrySnapshot {
            registry: "https://x".into(),
            packages: vec![p("1.0.0"), p("2.0.0")],
        })
        .unwrap();
        assert_eq!(
            s.candidates(&"foo".parse().unwrap())[0]
                .identity
                .version
                .to_string(),
            "2.0.0"
        );
    }
    #[test]
    fn registry_identity_is_part_of_package_identity() {
        let a = RegistryPackageId::new(
            "https://a".parse().unwrap(),
            "foo".parse().unwrap(),
            "1.0.0".parse().unwrap(),
        );
        let b = RegistryPackageId::new(
            "https://b".parse().unwrap(),
            "foo".parse().unwrap(),
            "1.0.0".parse().unwrap(),
        );
        assert_ne!(a, b);
    }
}
