//! Deterministic package manifests, packing, and preview-before-promote publication.

#![deny(unsafe_code)]

mod npm;
pub use npm::{NpmArtifact, NpmFile, NpmPackLimits, NpmPackOptions, pack_npm};

use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs, io,
    path::{Component, Path, PathBuf},
};
use tapid_core::ArtifactDigest;
const PACK_MAGIC: &[u8] = b"TAPID-PACK-1\n";

/// Memory and traversal bounds applied to packing and manifest inspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackLimits {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_artifact_bytes: u64,
}
impl Default for PackLimits {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_file_bytes: 512 * 1024 * 1024,
            max_artifact_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Version of this library, not the `TAPID-PACK-1` encoding version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Filesystem input to a pack operation; callers must keep the tree stable.
/// The version is an opaque label, not a parsed or registry-validated version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageSource {
    pub root: PathBuf,
    pub version: String,
    pub exclusions: ExclusionRules,
    pub limits: PackLimits,
}
impl PackageSource {
    /// Select a root and version, with only the built-in `.git`/`target` exclusions.
    pub fn new(root: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            version: version.into(),
            exclusions: ExclusionRules::default(),
            limits: PackLimits::default(),
        }
    }
    /// Replace explicit path exclusions without reading the filesystem.
    pub fn with_exclusions(mut self, exclusions: ExclusionRules) -> Self {
        self.exclusions = exclusions;
        self
    }
    /// Replace the bounds for both packing and manifest inspection.
    pub fn with_limits(mut self, limits: PackLimits) -> Self {
        self.limits = limits;
        self
    }
}
/// Case-sensitive rules applied to normalized relative paths before collision checks.
/// Invalid rule strings are retained literally; they do not bypass source validation.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ExclusionRules {
    exact: BTreeSet<String>,
    prefixes: BTreeSet<String>,
}
impl ExclusionRules {
    /// Start with no explicit exclusions; built-in exclusions still apply.
    pub fn new() -> Self {
        Self::default()
    }
    /// Exclude exactly this normalized relative path (file or directory).
    pub fn exclude(mut self, path: impl AsRef<str>) -> Self {
        self.exact
            .insert(normalize_path(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_owned()));
        self
    }
    /// Exclude a normalized path and its descendants at separator boundaries.
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

/// Size and SHA-256 digest derived from the same bytes read for one file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    pub digest: ArtifactDigest,
}
/// Lexicographically sorted file metadata produced by source inspection or packing.
/// Public fields are caller-editable and do not themselves certify validity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedFileManifest {
    pub version: String,
    pub files: Vec<ManifestFile>,
}
/// Filesystem, path-policy, or publication-state failure.
#[derive(Debug)]
pub enum PublishError {
    Io(io::Error),
    UnsafePath(String),
    InvalidSource(String),
    ImmutableVersion(String),
    InvalidState(&'static str),
    LimitExceeded(&'static str),
}
impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::UnsafePath(p) => write!(f, "unsafe package path: {p}"),
            Self::InvalidSource(s) => f.write_str(s),
            Self::ImmutableVersion(v) => write!(f, "version is already promoted: {v}"),
            Self::InvalidState(s) => f.write_str(s),
            Self::LimitExceeded(s) => write!(f, "package publication limit exceeded: {s}"),
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
    /// Read each included file once and discard its content after hashing.
    ///
    /// This is an independent inspection, not a reusable atomic tree snapshot.
    /// A subsequent [`pack`] reads again; use its returned manifest for byte binding.
    /// Applies the same collision policy and errors as [`pack`].
    pub fn from_source(source: &PackageSource) -> Result<Self, PublishError> {
        inspect_source(source)
    }
    /// Iterate normalized paths in manifest order (sorted for generated manifests).
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|f| f.path.as_str())
    }
}

/// In-memory pack output whose generated manifest describes its encoded file bytes.
/// Public fields can be modified; consumers must verify untrusted artifacts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedArtifact {
    pub version: String,
    pub bytes: Vec<u8>,
    pub digest: ArtifactDigest,
    pub manifest: NormalizedFileManifest,
}
impl PackedArtifact {
    /// Return the stored digest without recomputing it after caller modifications.
    pub fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }
}

/// Encode included files in sorted order using `TAPID-PACK-1`.
///
/// Each file is read once: its encoded content, size, and digest use that same
/// captured bytes. This is not an atomic directory snapshot; callers must prevent source
/// mutation, including symlink replacement during traversal and reads.
///
/// # Path policy and errors
///
/// After exclusions and separator normalization, equal Rust Unicode
/// `to_uppercase().to_lowercase()` keys are rejected as [`PublishError::UnsafePath`].
/// This conservative policy includes ASCII case pairs and Greek sigma variants,
/// but is not full Unicode case folding or a universal filesystem alias check.
/// It can reject distinct names (e.g. `ß` and `ss`), does not normalize Unicode,
/// and does not cover platform-specific aliases, reserved names, or directory
/// component collisions unless complete normalized file paths collide.
/// Casing follows the Unicode tables of the Rust toolchain.
///
/// Also rejects invalid roots, unsafe paths and observed symlinks/special files;
/// filesystem failures return [`PublishError::Io`]. No files are written and no
/// transport is invoked. Memory is bounded by the source's [`PackLimits`].
pub fn pack(source: &PackageSource) -> Result<PackedArtifact, PublishError> {
    let paths = normalized_source_paths(source)?;
    check_artifact_size(
        PACK_MAGIC.len() as u64 + 8,
        source.version.len() as u64,
        source.limits,
    )?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(PACK_MAGIC);
    append_field(&mut bytes, source.version.as_bytes());
    let mut manifest_files = Vec::with_capacity(paths.len());
    for (path, full) in paths {
        let framing = 16u64.saturating_add(path.len() as u64);
        check_artifact_size(bytes.len() as u64, framing, source.limits)?;
        append_field(&mut bytes, path.as_bytes());
        let size_offset = bytes.len();
        bytes.extend_from_slice(&0u64.to_le_bytes());
        let start = bytes.len();
        let maximum = source
            .limits
            .max_file_bytes
            .min(source.limits.max_artifact_bytes - start as u64);
        read_source_file(&full, maximum, &mut bytes)?;
        let size = (bytes.len() - start) as u64;
        bytes[size_offset..start].copy_from_slice(&size.to_le_bytes());
        let digest = digest_bytes(&bytes[start..]);
        manifest_files.push(ManifestFile { path, size, digest });
    }
    let manifest = NormalizedFileManifest {
        version: source.version.clone(),
        files: manifest_files,
    };
    let digest = digest_bytes(&bytes);
    Ok(PackedArtifact {
        version: manifest.version.clone(),
        bytes,
        digest,
        manifest,
    })
}
/// Compute the SHA-256 artifact identity of exactly these bytes.
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
fn normalized_source_paths(source: &PackageSource) -> Result<Vec<(String, PathBuf)>, PublishError> {
    if !source.root.is_dir() {
        return Err(PublishError::InvalidSource(
            "package root must be a directory".into(),
        ));
    }
    let mut paths = Vec::new();
    collect_files(
        &source.root,
        &source.root,
        &source.exclusions,
        source.limits,
        0,
        &mut paths,
    )?;
    paths.sort();
    let mut seen = BTreeSet::new();
    for (path, _) in &paths {
        if !seen.insert(path_collision_key(path)) {
            return Err(PublishError::UnsafePath(path.clone()));
        }
    }
    Ok(paths)
}
// A conservative casing policy, not Unicode case folding or a filesystem
// equivalence oracle. Uppercasing first also groups ordinary and final sigma.
fn path_collision_key(path: &str) -> String {
    path.to_uppercase().to_lowercase()
}

fn inspect_source(source: &PackageSource) -> Result<NormalizedFileManifest, PublishError> {
    let paths = normalized_source_paths(source)?;
    let mut total = check_artifact_size(
        PACK_MAGIC.len() as u64 + 8,
        source.version.len() as u64,
        source.limits,
    )?;
    let mut manifest_files = Vec::with_capacity(paths.len());
    for (path, full) in paths {
        use std::io::Read;
        let mut file = fs::File::open(&full)?;
        total = check_artifact_size(
            total,
            16u64.saturating_add(path.len() as u64),
            source.limits,
        )?;
        let maximum = source
            .limits
            .max_file_bytes
            .min(source.limits.max_artifact_bytes - total);
        if file.metadata()?.len() > maximum {
            return Err(PublishError::LimitExceeded("file bytes"));
        }
        let mut hash = Sha256::new();
        let mut size = 0;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            size += count as u64;
            if size > maximum {
                return Err(PublishError::LimitExceeded("file bytes"));
            }
        }
        total = check_artifact_size(total, size, source.limits)?;
        manifest_files.push(ManifestFile {
            path,
            size,
            digest: format!("sha256-{}", hex_lower(&hash.finalize()))
                .parse()
                .expect("sha256 digest is valid"),
        });
    }
    Ok(NormalizedFileManifest {
        version: source.version.clone(),
        files: manifest_files,
    })
}

fn check_artifact_size(current: u64, added: u64, limits: PackLimits) -> Result<u64, PublishError> {
    current
        .checked_add(added)
        .filter(|size| *size <= limits.max_artifact_bytes)
        .ok_or(PublishError::LimitExceeded("artifact bytes"))
}

fn read_source_file(path: &Path, maximum: u64, bytes: &mut Vec<u8>) -> Result<(), PublishError> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > maximum {
        return Err(PublishError::LimitExceeded("file bytes"));
    }
    let start = bytes.len();
    file.take(maximum.saturating_add(1)).read_to_end(bytes)?;
    if (bytes.len() - start) as u64 > maximum {
        return Err(PublishError::LimitExceeded("file bytes"));
    }
    #[cfg(test)]
    maybe_mutate_test_source(path);
    Ok(())
}

#[cfg(test)]
fn test_mutation_state() -> &'static std::sync::Mutex<Option<PathBuf>> {
    static STATE: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
        std::sync::OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
fn configure_test_mutation(path: PathBuf) {
    *test_mutation_state().lock().unwrap() = Some(path);
}

#[cfg(test)]
fn maybe_mutate_test_source(path: &Path) {
    let mut target = test_mutation_state().lock().unwrap();
    if target.as_deref() == Some(path) {
        fs::write(path, b"after").unwrap();
        *target = None;
    }
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
    limits: PackLimits,
    depth: usize,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), PublishError> {
    if depth > 128 {
        return Err(PublishError::LimitExceeded("directory depth"));
    }
    for e in fs::read_dir(dir)? {
        let e = e?;
        let full = e.path();
        let rel = full.strip_prefix(root).expect("root prefix");
        let rel = normalize_path(&rel.to_string_lossy())?;
        if rel.len() > 4096 {
            return Err(PublishError::LimitExceeded("path bytes"));
        }
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
            collect_files(root, &full, rules, limits, depth + 1, out)?;
        } else if metadata.file_type().is_file() {
            if out.len() >= limits.max_files {
                return Err(PublishError::LimitExceeded("file count"));
            }
            out.push((rel, full));
        }
    }
    Ok(())
}

/// Injected publication boundary; implementations own registry authentication
/// and durable server-side version immutability.
pub trait PublicationTransport {
    /// Transport-specific failure, hidden by [`Publisher::promote`].
    type Error;
    /// Publish the supplied bytes under a version, enforcing remote invariants.
    fn publish(&mut self, version: &str, artifact: &PackedArtifact) -> Result<(), Self::Error>;
}
/// Prepared artifact for explicit promotion; construction does not contact a registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    pub version: String,
    pub artifact: PackedArtifact,
}
/// Local preview/promotion result, not a signed registry receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicationState {
    Previewed(Preview),
    Promoted {
        version: String,
        digest: ArtifactDigest,
    },
}
/// Preview/promote coordinator with in-memory, per-instance version tracking.
/// This is a native publishing foundation, not an operational registry client.
pub struct Publisher<T> {
    transport: T,
    promoted: BTreeMap<String, ArtifactDigest>,
}
impl<T> Publisher<T> {
    /// Wrap a transport with an empty, non-persistent promotion history.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            promoted: BTreeMap::new(),
        }
    }
    /// Pack locally without calling the transport; inherits [`pack`] limitations.
    pub fn preview(&self, source: &PackageSource) -> Result<Preview, PublishError> {
        Ok(Preview {
            version: source.version.clone(),
            artifact: pack(source)?,
        })
    }
}
impl<T: PublicationTransport> Publisher<T> {
    /// Send a prepared artifact and record its version only after transport success.
    /// Rejects versions already promoted by this instance; does not re-read source
    /// files or validate caller-modified public preview fields.
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
    #[test]
    fn publication_limits_include_framing_and_apply_to_inspection_and_preview() {
        let fixture = tapid_test_support::TempProject::new("publish-total-limit").unwrap();
        fixture.write("a", b"1234").unwrap();
        fixture.write("b", b"5678").unwrap();
        let source = PackageSource::new(fixture.path(), "1.0.0");
        // 13 magic bytes, 8-byte version length, 5 version bytes, then
        // two records with 16 framing bytes, one path byte, and four data bytes.
        let exact = source.clone().with_limits(PackLimits {
            max_files: 2,
            max_file_bytes: 4,
            max_artifact_bytes: 68,
        });
        assert_eq!(pack(&exact).unwrap().bytes.len(), 68);
        assert_eq!(
            NormalizedFileManifest::from_source(&exact)
                .unwrap()
                .files
                .len(),
            2
        );
        let too_small = source.clone().with_limits(PackLimits {
            max_artifact_bytes: 67,
            ..PackLimits::default()
        });
        assert!(matches!(
            pack(&too_small),
            Err(PublishError::LimitExceeded(_))
        ));
        assert!(matches!(
            NormalizedFileManifest::from_source(&too_small),
            Err(PublishError::LimitExceeded(_))
        ));
        assert!(Publisher::new(Tx(0)).preview(&too_small).is_err());
        let too_many = source.with_limits(PackLimits {
            max_files: 1,
            ..PackLimits::default()
        });
        assert!(matches!(
            pack(&too_many),
            Err(PublishError::LimitExceeded(_))
        ));
        assert!(NormalizedFileManifest::from_source(&too_many).is_err());
    }

    #[test]
    fn packing_and_inspection_reject_oversized_files_before_reading_them() {
        let fixture = tapid_test_support::TempProject::new("publish-file-limit").unwrap();
        fixture.write("file", b"12345").unwrap();
        let source = PackageSource::new(fixture.path(), "1.0.0").with_limits(PackLimits {
            max_file_bytes: 4,
            ..PackLimits::default()
        });
        assert!(matches!(pack(&source), Err(PublishError::LimitExceeded(_))));
        assert!(matches!(
            NormalizedFileManifest::from_source(&source),
            Err(PublishError::LimitExceeded(_))
        ));
    }

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
    fn packing_binds_manifest_metadata_to_packed_bytes() {
        let p = tree(&[("x", b"before")]);
        let artifact = pack(&PackageSource::new(&p, "1.0.0")).unwrap();
        let mut cursor = b"TAPID-PACK-1\n".len();
        let read_field = |bytes: &[u8], cursor: &mut usize| {
            let end = *cursor + 8;
            let len = u64::from_le_bytes(bytes[*cursor..end].try_into().unwrap()) as usize;
            *cursor = end;
            let end = *cursor + len;
            let field = bytes[*cursor..end].to_vec();
            *cursor = end;
            field
        };
        assert_eq!(read_field(&artifact.bytes, &mut cursor), b"1.0.0");
        assert_eq!(read_field(&artifact.bytes, &mut cursor), b"x");
        let end = cursor + 8;
        let packed_size = u64::from_le_bytes(artifact.bytes[cursor..end].try_into().unwrap());
        cursor = end;
        let packed_data = &artifact.bytes[cursor..cursor + packed_size as usize];

        assert_eq!(artifact.manifest.files[0].size, packed_size);
        assert_eq!(packed_data, b"before");
        assert_eq!(
            artifact.manifest.files[0].digest,
            artifact_digest(packed_data)
        );
    }

    #[cfg(unix)]
    #[test]
    fn normalized_path_collisions_are_rejected() {
        let p = tree(&[("a/b", b"nested"), (r"a\b", b"literal")]);
        assert!(matches!(
            pack(&PackageSource::new(&p, "1.0.0")),
            Err(PublishError::UnsafePath(path)) if path == "a/b"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unicode_sigma_collisions_are_rejected_by_pack() {
        // A literal backslash keeps both source files distinct even on a
        // case-insensitive host; packing normalizes it to a separator.
        let p = tree(&[("σ/config.json", b"ordinary"), (r"ς\CONFIG.JSON", b"final")]);
        assert_eq!(fs::read(p.join("σ/config.json")).unwrap(), b"ordinary");
        assert_eq!(fs::read(p.join(r"ς\CONFIG.JSON")).unwrap(), b"final");
        let source = PackageSource::new(&p, "1.0.0");
        assert!(matches!(pack(&source), Err(PublishError::UnsafePath(_))));
        assert!(matches!(
            NormalizedFileManifest::from_source(&source),
            Err(PublishError::UnsafePath(_))
        ));
        let allowed =
            pack(&source.with_exclusions(ExclusionRules::new().exclude("ς/CONFIG.JSON"))).unwrap();
        assert_eq!(
            allowed.manifest.paths().collect::<Vec<_>>(),
            ["σ/config.json"]
        );
        assert_eq!(
            allowed.manifest.files[0].digest,
            artifact_digest(b"ordinary")
        );
    }

    #[test]
    fn case_insensitive_path_collision_keys_match() {
        assert_eq!(
            path_collision_key("A/config.json"),
            path_collision_key("a/CONFIG.JSON")
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn case_insensitive_path_collisions_are_rejected_by_pack() {
        let p = tree(&[("A/config.json", b"upper"), ("a/CONFIG.JSON", b"lower")]);
        assert!(matches!(
            pack(&PackageSource::new(&p, "1.0.0")),
            Err(PublishError::UnsafePath(_))
        ));
    }

    #[test]
    fn packing_uses_the_snapshot_when_source_changes_after_a_read() {
        let p = tree(&[("x", b"before")]);
        configure_test_mutation(p.join("x"));
        let artifact = pack(&PackageSource::new(&p, "1.0.0")).unwrap();
        assert_eq!(artifact.manifest.files[0].size, 6);
        assert_eq!(
            artifact.manifest.files[0].digest,
            artifact_digest(b"before")
        );
        assert_eq!(&artifact.bytes[artifact.bytes.len() - 6..], b"before");
        assert_eq!(fs::read(p.join("x")).unwrap(), b"after");
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
