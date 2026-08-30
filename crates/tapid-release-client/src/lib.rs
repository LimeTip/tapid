//! Release sequence is intentionally not accepted here: v1's strict schema has no sequence field.
//! A future schema version must add it and bind monotonic policy explicitly.
#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tapid_signatures::{KeyRing, VerificationError, release};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug)]
pub enum Error {
    InvalidManifest(String),
    Signature(VerificationError),
    StaleMetadata,
    TargetNotFound(String),
    ArtifactDigestMismatch,
    ArtifactSizeMismatch {
        expected: u64,
        actual: u64,
    },
    Fetch(String),
    State(String),
    ReleaseReplay {
        current_sequence: u64,
        received_sequence: u64,
    },
    ReleaseDowngrade {
        floor: String,
        received: String,
    },
    AllEndpointsFailed {
        attempts: usize,
    },
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChannelIndex {
    pub channel: String,
    pub manifests: Vec<String>,
}

const MAX_MANIFESTS_PER_INDEX: usize = 16;
const MAX_CHANNEL_INDEX_BYTES: usize = 256 * 1024;
const MAX_MANIFEST_BYTES: usize = 256 * 1024;

impl ChannelIndex {
    fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let index: Self = serde_json::from_slice(bytes)
            .map_err(|e| Error::InvalidManifest(format!("invalid channel index: {e}")))?;
        if index.channel != "stable"
            || index.manifests.is_empty()
            || index.manifests.len() > MAX_MANIFESTS_PER_INDEX
            || index.manifests.iter().any(|url| !https(url))
        {
            return Err(Error::InvalidManifest(
                "invalid stable channel index".into(),
            ));
        }
        Ok(index)
    }
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
    for endpoint in endpoints {
        if !https(endpoint) {
            continue;
        }
        let body = match fetcher.fetch(endpoint) {
            Ok(body) => body,
            Err(_) => continue,
        };
        if body.len() > MAX_CHANNEL_INDEX_BYTES {
            continue;
        }
        let index = match ChannelIndex::parse(&body) {
            Ok(index) => index,
            Err(_) => continue,
        };
        for manifest_url in &index.manifests {
            let body = match fetcher.fetch(manifest_url) {
                Ok(body) => body,
                Err(_) => continue,
            };
            if body.len() > MAX_MANIFEST_BYTES {
                continue;
            }
            if let Ok(manifest) =
                ReleaseManifest::parse_and_verify(&body, keyring, target, now, max_age)
            {
                return Ok(manifest);
            }
        }
    }
    Err(Error::AllEndpointsFailed {
        attempts: endpoints.len(),
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LastKnownGood {
    pub version: String,
    pub artifact_sha256: String,
}
pub fn write_last_known_good(path: &Path, state: &LastKnownGood) -> Result<(), Error> {
    validate_lkg(state)?;
    safe_atomic_write(
        path,
        &serde_json::to_vec(state).map_err(|e| Error::State(e.to_string()))?,
    )
}
pub fn read_last_known_good(path: &Path) -> Result<LastKnownGood, Error> {
    let bytes = fs::read(path).map_err(|e| Error::State(e.to_string()))?;
    let state: LastKnownGood =
        serde_json::from_slice(&bytes).map_err(|e| Error::State(e.to_string()))?;
    validate_lkg(&state)?;
    Ok(state)
}

/// Durable client policy state. This is deliberately separate from v1 manifests:
/// v1 remains byte-for-byte and parser compatible, while state has an explicit v2 schema.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseState {
    pub schema: String,
    pub release_floor: String,
    pub release_sequence: u64,
    pub last_known_good: LastKnownGood,
}

impl ReleaseState {
    pub fn new(version: &str, sequence: u64, artifact_sha256: String) -> Result<Self, Error> {
        let state = Self {
            schema: "tapid-release-state-v2".into(),
            release_floor: version.into(),
            release_sequence: sequence,
            last_known_good: LastKnownGood {
                version: version.into(),
                artifact_sha256,
            },
        };
        validate_state(&state)?;
        Ok(state)
    }
}

pub fn accept_release(
    state: &ReleaseState,
    version: &str,
    sequence: u64,
    artifact_sha256: String,
) -> Result<ReleaseState, Error> {
    validate_state(state)?;
    if sequence <= state.release_sequence {
        return Err(Error::ReleaseReplay {
            current_sequence: state.release_sequence,
            received_sequence: sequence,
        });
    }
    if compare_version(version, &state.release_floor)? == std::cmp::Ordering::Less {
        return Err(Error::ReleaseDowngrade {
            floor: state.release_floor.clone(),
            received: version.into(),
        });
    }
    let floor = if compare_version(version, &state.release_floor)? != std::cmp::Ordering::Less {
        version
    } else {
        &state.release_floor
    };
    ReleaseState::new(floor, sequence, artifact_sha256)
}

pub fn write_release_state(path: &Path, state: &ReleaseState) -> Result<(), Error> {
    validate_state(state)?;
    safe_atomic_write(
        path,
        &serde_json::to_vec(state).map_err(|e| Error::State(e.to_string()))?,
    )
}

pub fn read_release_state(path: &Path) -> Result<ReleaseState, Error> {
    let bytes = fs::read(path).map_err(|e| Error::State(e.to_string()))?;
    let state: ReleaseState =
        serde_json::from_slice(&bytes).map_err(|e| Error::State(e.to_string()))?;
    validate_state(&state)?;
    Ok(state)
}

fn validate_state(state: &ReleaseState) -> Result<(), Error> {
    if state.schema != "tapid-release-state-v2" || !version(&state.release_floor) {
        return Err(Error::State(
            "unsupported or malformed release state".into(),
        ));
    }
    validate_lkg(&state.last_known_good)?;
    if compare_version(&state.last_known_good.version, &state.release_floor)?
        == std::cmp::Ordering::Less
    {
        return Err(Error::State(
            "last-known-good is below release floor".into(),
        ));
    }
    Ok(())
}

fn validate_lkg(state: &LastKnownGood) -> Result<(), Error> {
    if !version(&state.version) || !hex64(&state.artifact_sha256) {
        return Err(Error::State("malformed last-known-good state".into()));
    }
    Ok(())
}

fn compare_version(left: &str, right: &str) -> Result<std::cmp::Ordering, Error> {
    let parse = |s: &str| -> Result<[u64; 3], Error> {
        let p: Vec<_> = s.split('.').collect();
        if p.len() != 3 || p[0] != "0" {
            return Err(Error::State("invalid release version".into()));
        }
        Ok([
            0,
            p[1].parse()
                .map_err(|_| Error::State("invalid release version".into()))?,
            p[2].parse()
                .map_err(|_| Error::State("invalid release version".into()))?,
        ])
    };
    Ok(parse(left)?.cmp(&parse(right)?))
}

fn safe_atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::State("state path has no parent".into()))?;
    let parent_meta = fs::symlink_metadata(parent).map_err(|e| Error::State(e.to_string()))?;
    if !parent_meta.file_type().is_dir() || parent_meta.file_type().is_symlink() {
        return Err(Error::State("state parent must be a real directory".into()));
    }
    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(Error::State("state path must not be a symlink".into()));
            }
            if !metadata.file_type().is_file() {
                return Err(Error::State("state path must be a regular file".into()));
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(Error::State(error.to_string())),
    };
    let name = path
        .file_name()
        .ok_or_else(|| Error::State("state path has no filename".into()))?
        .to_string_lossy();
    let tmp: PathBuf = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    let backup: PathBuf = parent.join(format!(".{name}.bak-{}", std::process::id()));
    let mut created = false;
    let mut moved_existing = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| Error::State(e.to_string()))?;
        created = true;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|e| Error::State(e.to_string()))?;
        }
        file.write_all(bytes)
            .map_err(|e| Error::State(e.to_string()))?;
        file.sync_all().map_err(|e| Error::State(e.to_string()))?;
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::State("state path became a symlink".into()));
        }
        if existing {
            fs::rename(path, &backup).map_err(|e| Error::State(e.to_string()))?;
            moved_existing = true;
        }
        if let Err(error) = fs::rename(&tmp, path) {
            if moved_existing {
                let _ = fs::rename(&backup, path);
                moved_existing = false;
            }
            return Err(Error::State(error.to_string()));
        }
        if moved_existing {
            fs::remove_file(&backup).map_err(|e| Error::State(e.to_string()))?;
            moved_existing = false;
        }
        #[cfg(unix)]
        {
            OpenOptions::new()
                .read(true)
                .open(parent)
                .and_then(|d| d.sync_all())
                .map_err(|e| Error::State(e.to_string()))?;
        }
        Ok(())
    })();
    if result.is_err() {
        if created {
            let _ = fs::remove_file(&tmp);
        }
        if moved_existing {
            let _ = fs::remove_file(path);
            let _ = fs::rename(&backup, path);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    include!("tests.rs");
}
