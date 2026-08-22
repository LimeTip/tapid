//! Deterministic package manifests, packing, and preview-before-promote publication.

#![deny(unsafe_code)]

use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs, io,
    path::{Component, Path, PathBuf},
};
use tapid_core::ArtifactDigest;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageSource {
    pub root: PathBuf,
    pub version: String,
    pub exclusions: ExclusionRules,
}
impl PackageSource {
    pub fn new(root: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            version: version.into(),
            exclusions: ExclusionRules::default(),
        }
    }
    pub fn with_exclusions(mut self, exclusions: ExclusionRules) -> Self {
        self.exclusions = exclusions;
        self
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ExclusionRules {
    exact: BTreeSet<String>,
    prefixes: BTreeSet<String>,
}
impl ExclusionRules {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn exclude(mut self, path: impl AsRef<str>) -> Self {
        self.exact
            .insert(normalize_path(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_owned()));
        self
    }
    pub fn exclude_prefix(mut self, path: impl AsRef<str>) -> Self {
        self.prefixes.insert(
            normalize_path(path.as_ref())
                .unwrap_or_else(|_| path.as_ref().to_owned())
                .trim_end_matches('/')
                .to_owned(),
        );
        self
    }
    fn matches(&self, path: &str) -> bool {
        self.exact.contains(path)
            || self
                .prefixes
                .iter()
                .any(|p| path == p || path.starts_with(&format!("{p}/")))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    pub digest: ArtifactDigest,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedFileManifest {
    pub version: String,
    pub files: Vec<ManifestFile>,
}
#[derive(Debug)]
pub enum PublishError {
    Io(io::Error),
    UnsafePath(String),
    InvalidSource(String),
    ImmutableVersion(String),
    InvalidState(&'static str),
}
impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::UnsafePath(p) => write!(f, "unsafe package path: {p}"),
            Self::InvalidSource(s) => f.write_str(s),
            Self::ImmutableVersion(v) => write!(f, "version is already promoted: {v}"),
            Self::InvalidState(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for PublishError {}
impl From<io::Error> for PublishError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl NormalizedFileManifest {
    pub fn from_source(source: &PackageSource) -> Result<Self, PublishError> {
        if !source.root.is_dir() {
            return Err(PublishError::InvalidSource(
                "package root must be a directory".into(),
            ));
        }
        let mut paths = Vec::new();
        collect_files(&source.root, &source.root, &source.exclusions, &mut paths)?;
        paths.sort();
        let files = paths
            .into_iter()
            .map(|(path, full)| {
                let bytes = fs::read(full)?;
                let digest = digest_bytes(&bytes);
                Ok(ManifestFile {
                    path,
                    size: bytes.len() as u64,
                    digest,
                })
            })
            .collect::<Result<Vec<_>, PublishError>>()?;
        Ok(Self {
            version: source.version.clone(),
            files,
        })
    }
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|f| f.path.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedArtifact {
    pub version: String,
    pub bytes: Vec<u8>,
    pub digest: ArtifactDigest,
    pub manifest: NormalizedFileManifest,
}
impl PackedArtifact {
    pub fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }
}

pub fn pack(source: &PackageSource) -> Result<PackedArtifact, PublishError> {
    let manifest = NormalizedFileManifest::from_source(source)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"TAPID-PACK-1\n");
    append_field(&mut bytes, manifest.version.as_bytes());
    for file in &manifest.files {
        let data = fs::read(
            source
                .root
                .join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        )?;
        append_field(&mut bytes, file.path.as_bytes());
        bytes.extend_from_slice(&(data.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&data);
    }
    let digest = digest_bytes(&bytes);
    Ok(PackedArtifact {
        version: manifest.version.clone(),
        bytes,
        digest,
        manifest,
    })
}
pub fn artifact_digest(bytes: &[u8]) -> ArtifactDigest {
    digest_bytes(bytes)
}
fn digest_bytes(bytes: &[u8]) -> ArtifactDigest {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("sha256-{}", hex_lower(&h.finalize()))
        .parse()
        .expect("sha256 digest is valid")
}
fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn append_field(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn normalize_path(raw: &str) -> Result<String, PublishError> {
    if raw.is_empty() || raw.contains('\0') {
        return Err(PublishError::UnsafePath(raw.into()));
    }
    let raw = raw.replace('\\', "/");
    let p = Path::new(&raw);
    if p.is_absolute() {
        return Err(PublishError::UnsafePath(raw));
    }
    let mut out = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => {
                let s = s
                    .to_str()
                    .ok_or_else(|| PublishError::UnsafePath(raw.clone()))?;
                out.push(s);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PublishError::UnsafePath(raw.clone()));
            }
        }
    }
    if out.is_empty() {
        return Err(PublishError::UnsafePath(raw));
    }
    Ok(out.join("/"))
}
fn collect_files(
    root: &Path,
    dir: &Path,
    rules: &ExclusionRules,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), PublishError> {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let full = e.path();
        let rel = full.strip_prefix(root).expect("root prefix");
        let rel = normalize_path(&rel.to_string_lossy())?;
        if rules.matches(&rel)
            || rel == ".git"
            || rel == "target"
            || rel.starts_with(".git/")
            || rel.starts_with("target/")
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&full)?;
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_file() && !metadata.file_type().is_dir()
        {
            return Err(PublishError::UnsafePath(rel));
        }
        if metadata.file_type().is_dir() {
            collect_files(root, &full, rules, out)?;
        } else if metadata.file_type().is_file() {
            out.push((rel, full));
        }
    }
    Ok(())
}

pub trait PublicationTransport {
    type Error;
    fn publish(&mut self, version: &str, artifact: &PackedArtifact) -> Result<(), Self::Error>;
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    pub version: String,
    pub artifact: PackedArtifact,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicationState {
    Previewed(Preview),
    Promoted {
        version: String,
        digest: ArtifactDigest,
    },
}
pub struct Publisher<T> {
    transport: T,
    promoted: BTreeMap<String, ArtifactDigest>,
}
impl<T> Publisher<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            promoted: BTreeMap::new(),
        }
    }
    pub fn preview(&self, source: &PackageSource) -> Result<Preview, PublishError> {
        Ok(Preview {
            version: source.version.clone(),
            artifact: pack(source)?,
        })
    }
}
impl<T: PublicationTransport> Publisher<T> {
    pub fn promote(&mut self, preview: Preview) -> Result<PublicationState, PublishError> {
        if self.promoted.contains_key(&preview.version) {
            return Err(PublishError::ImmutableVersion(preview.version));
        }
        self.transport
            .publish(&preview.version, &preview.artifact)
            .map_err(|_| PublishError::InvalidState("publication transport rejected artifact"))?;
        self.promoted
            .insert(preview.version.clone(), preview.artifact.digest.clone());
        Ok(PublicationState::Promoted {
            version: preview.version,
            digest: preview.artifact.digest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    fn tree(files: &[(&str, &[u8])]) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "tapid-publish-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        for (n, b) in files {
            let f = p.join(n);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, b).unwrap();
        }
        p
    }
    #[test]
    fn ordering_independent_and_exclusions_apply() {
        let a = tree(&[
            ("z.txt", b"z"),
            ("a.txt", b"a"),
            ("target/no", b"x"),
            ("secret", b"s"),
        ]);
        let b = tree(&[("a.txt", b"a"), ("z.txt", b"z")]);
        let rules = ExclusionRules::new().exclude("secret");
        let m = NormalizedFileManifest::from_source(
            &PackageSource::new(&a, "1.0.0").with_exclusions(rules),
        )
        .unwrap();
        assert_eq!(m.paths().collect::<Vec<_>>(), vec!["a.txt", "z.txt"]);
        assert_eq!(
            pack(
                &PackageSource::new(&a, "1.0.0")
                    .with_exclusions(ExclusionRules::new().exclude("secret"))
            )
            .unwrap()
            .bytes,
            pack(&PackageSource::new(&b, "1.0.0")).unwrap().bytes
        );
    }
    #[test]
    fn unsafe_paths_rejected() {
        for p in ["../x", "/x", "a\\..\\x", "a/\0"] {
            assert!(normalize_path(p).is_err(), "{p}");
        }
    }
    #[test]
    fn pack_is_byte_reproducible_and_digest_binds_bytes() {
        let p = tree(&[("x", b"1")]);
        let a = pack(&PackageSource::new(&p, "1.0.0")).unwrap();
        let b = pack(&PackageSource::new(&p, "1.0.0")).unwrap();
        assert_eq!(a.bytes, b.bytes);
        assert_eq!(a.digest, artifact_digest(&a.bytes));
        fs::write(p.join("x"), b"2").unwrap();
        assert_ne!(
            a.digest,
            pack(&PackageSource::new(&p, "1.0.0")).unwrap().digest
        );
    }
    struct Tx(usize);
    impl PublicationTransport for Tx {
        type Error = ();
        fn publish(&mut self, _: &str, _: &PackedArtifact) -> Result<(), ()> {
            self.0 += 1;
            Ok(())
        }
    }
    #[test]
    fn preview_has_no_transport_side_effect_and_versions_are_immutable() {
        let p = tree(&[("x", b"x")]);
        let mut pubr = Publisher::new(Tx(0));
        let preview = pubr.preview(&PackageSource::new(&p, "1.0.0")).unwrap();
        assert_eq!(pubr.transport.0, 0);
        assert!(matches!(
            pubr.promote(preview.clone()),
            Ok(PublicationState::Promoted { .. })
        ));
        assert_eq!(pubr.transport.0, 1);
        assert!(matches!(
            pubr.promote(preview),
            Err(PublishError::ImmutableVersion(_))
        ));
    }
}
