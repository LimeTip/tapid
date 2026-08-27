//! Release sequence is intentionally not accepted here: v1's strict schema has no sequence field.
//! A future schema version must add it and bind monotonic policy explicitly.
#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, fs, path::Path, time::Duration};
use tapid_signatures::{KeyRing, VerificationError, release};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug)]
pub enum Error {
    InvalidManifest(String),
    Signature(VerificationError),
    StaleMetadata,
    TargetNotFound(String),
    ArtifactDigestMismatch,
    ArtifactSizeMismatch { expected: u64, actual: u64 },
    Fetch(String),
    State(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema: String,
    pub product: String,
    pub version: String,
    pub tag: String,
    pub commit: String,
    pub created_at: String,
    pub expires_at: String,
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub sbom: Option<Document>,
    #[serde(default)]
    pub provenance: Option<Document>,
    pub signature: serde_json::Value,
    #[serde(skip)]
    selected_target: String,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub name: String,
    pub target: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub url: String,
    pub sha256: String,
    pub media_type: String,
}

impl ReleaseManifest {
    pub fn parse_and_verify(
        bytes: &[u8],
        keyring: &KeyRing,
        target: &str,
        now: &str,
        max_age: Option<Duration>,
    ) -> Result<Self, Error> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| Error::InvalidManifest(e.to_string()))?;
        let mut manifest: Self = serde_json::from_value(value.clone())
            .map_err(|e| Error::InvalidManifest(e.to_string()))?;
        validate_shape(&manifest, target, now, max_age)?;
        release::verify(&value, keyring).map_err(Error::Signature)?;
        manifest.selected_target = target.into();
        Ok(manifest)
    }
    pub fn artifact(&self) -> Option<&Artifact> {
        self.artifacts
            .iter()
            .find(|a| a.target == self.selected_target)
    }
    pub fn verify_artifact(&self, bytes: &[u8]) -> Result<(), Error> {
        let artifact = self
            .artifact()
            .ok_or_else(|| Error::TargetNotFound("no artifact".into()))?;
        if bytes.len() as u64 != artifact.size {
            return Err(Error::ArtifactSizeMismatch {
                expected: artifact.size,
                actual: bytes.len() as u64,
            });
        }
        let actual = hex_digest(bytes);
        if actual != artifact.sha256 {
            return Err(Error::ArtifactDigestMismatch);
        }
        Ok(())
    }
}

fn validate_shape(
    m: &ReleaseManifest,
    target: &str,
    now: &str,
    max_age: Option<Duration>,
) -> Result<(), Error> {
    if m.schema != "tapid-release-manifest-v1"
        || m.product != "tapid"
        || !version(&m.version)
        || m.tag != format!("v{}", m.version)
        || !(40..=64).contains(&m.commit.len())
        || !m
            .commit
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Error::InvalidManifest(
            "identity fields do not match v1".into(),
        ));
    }
    let now = parse_time(now)?;
    let created = parse_time(&m.created_at)?;
    let expires = parse_time(&m.expires_at)?;
    if created > now
        || expires <= now
        || expires <= created
        || max_age.is_some_and(|age| {
            now - created > time::Duration::try_from(age).unwrap_or(time::Duration::MAX)
        })
    {
        return Err(Error::StaleMetadata);
    }
    if m.artifacts.is_empty() {
        return Err(Error::InvalidManifest("artifacts must not be empty".into()));
    }
    if m.artifacts.iter().filter(|a| a.target == target).count() != 1 {
        return Err(Error::TargetNotFound(target.into()));
    }
    for a in &m.artifacts {
        if !valid_target(&a.target)
            || !hex64(&a.sha256)
            || a.size == 0
            || !https(&a.url)
            || !a
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            || !a.name.starts_with(&format!("tapid-{}-", m.version))
            || !(a.name.ends_with(".tar.gz") || a.name.ends_with(".zip"))
        {
            return Err(Error::InvalidManifest("invalid artifact fields".into()));
        }
    }
    for d in m.sbom.as_ref().into_iter().chain(m.provenance.as_ref()) {
        if !https(&d.url) || !hex64(&d.sha256) || d.media_type.is_empty() {
            return Err(Error::InvalidManifest("invalid document fields".into()));
        }
    }
    Ok(())
}
fn parse_time(s: &str) -> Result<OffsetDateTime, Error> {
    OffsetDateTime::parse(s, &Rfc3339)
        .map_err(|e| Error::InvalidManifest(format!("invalid timestamp: {e}")))
}
fn version(s: &str) -> bool {
    let p: Vec<_> = s.split('.').collect();
    p.len() == 3 && p[0] == "0" && p[1].parse::<u64>().is_ok() && p[2].parse::<u64>().is_ok()
}
fn valid_target(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn https(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("https://") else {
        return false;
    };
    !rest.is_empty()
        && !rest.bytes().any(|b| b.is_ascii_whitespace() || b == b'@')
        && rest
            .split('/')
            .next()
            .is_some_and(|h| !h.is_empty() && !h.starts_with(':'))
}
fn hex_digest(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}

pub trait Fetcher {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>, String>;
}
pub fn discover<F: Fetcher>(
    fetcher: &mut F,
    endpoints: &[&str],
    keyring: &KeyRing,
    target: &str,
    now: &str,
    max_age: Option<Duration>,
) -> Result<ReleaseManifest, Error> {
    let mut last = None;
    for endpoint in endpoints {
        if !https(endpoint) {
            last = Some(Error::InvalidManifest(
                "discovery endpoint must use HTTPS".into(),
            ));
            continue;
        }
        match fetcher.fetch(endpoint) {
            Ok(body) => {
                match ReleaseManifest::parse_and_verify(&body, keyring, target, now, max_age) {
                    Ok(m) => return Ok(m),
                    Err(e) => last = Some(e),
                }
            }
            Err(e) => last = Some(Error::Fetch(e)),
        }
    }
    Err(last.unwrap_or_else(|| Error::Fetch("no discovery endpoints".into())))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LastKnownGood {
    pub version: String,
    pub artifact_sha256: String,
}
pub fn write_last_known_good(path: &Path, state: &LastKnownGood) -> Result<(), Error> {
    let bytes = serde_json::to_vec(state).map_err(|e| Error::State(e.to_string()))?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&tmp, bytes).map_err(|e| Error::State(e.to_string()))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::State(e.to_string())
    })
}
pub fn read_last_known_good(path: &Path) -> Result<LastKnownGood, Error> {
    let bytes = fs::read(path).map_err(|e| Error::State(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::State(e.to_string()))
}

#[cfg(test)]
mod tests {
    include!("tests.rs");
}
