mod selection;
use selection::Pattern;

use crate::{artifact_digest, path_collision_key};
use flate2::{Compression, GzBuilder};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use tapid_core::ArtifactDigest;
use tapid_manifest::PackageManifest;

/// Resource budgets for npm packing, separate from custom-format framing limits.
#[derive(Clone, Debug)]
pub struct NpmPackLimits {
    pub max_files: usize,
    pub max_visited_entries: usize,
    pub max_depth: usize,
    pub max_path_bytes: usize,
    pub max_file_bytes: u64,
    pub max_content_bytes: u64,
    pub max_tar_bytes: u64,
    pub max_archive_bytes: u64,
    pub max_ignore_file_bytes: u64,
    pub max_ignore_bytes: u64,
    pub max_patterns: usize,
    pub max_match_steps: u64,
}
impl Default for NpmPackLimits {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_visited_entries: 1_000_000,
            max_depth: 128,
            max_path_bytes: 4096,
            max_file_bytes: 512 << 20,
            max_content_bytes: 512 << 20,
            max_tar_bytes: 640 << 20,
            max_archive_bytes: 512 << 20,
            max_ignore_file_bytes: 1 << 20,
            max_ignore_bytes: 8 << 20,
            max_patterns: 10_000,
            max_match_steps: 10_000_000,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct NpmPackOptions {
    pub limits: NpmPackLimits,
    /// Absolute destination resolved by the caller; excluded before capture.
    pub destination: Option<PathBuf>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmFile {
    pub path: String,
    pub size: u64,
    pub digest: ArtifactDigest,
    pub mode: u32,
}
/// Immutable inspection bound to completed gzip bytes.
#[derive(Debug)]
pub struct NpmArtifact {
    gitignore_not_used: bool,
    name: String,
    version: String,
    files: Vec<NpmFile>,
    bytes: Vec<u8>,
    digest: ArtifactDigest,
}
impl NpmArtifact {
    pub fn gitignore_not_used(&self) -> bool {
        self.gitignore_not_used
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn files(&self) -> &[NpmFile] {
        &self.files
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }
    pub fn content_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Capture a trusted stable directory without lifecycle execution or output writes.
/// Reads are not an atomic source snapshot. Preview and creation are independent reads.
pub fn pack_npm(root: &Path, options: &NpmPackOptions) -> io::Result<NpmArtifact> {
    let root = root.canonicalize()?;

    let manifest_bytes = read_bounded(
        &root.join("package.json"),
        options
            .limits
            .max_file_bytes
            .min(tapid_manifest::MAX_MANIFEST_BYTES as u64),
    )?;
    let manifest = PackageManifest::parse(
        std::str::from_utf8(&manifest_bytes).map_err(|_| invalid("package.json is not UTF-8"))?,
    )
    .map_err(|error| invalid(error.to_string()))?;
    let metadata = manifest
        .packaging()
        .map_err(|error| invalid(error.to_string()))?;
    if metadata
        .files
        .as_ref()
        .is_some_and(|patterns| patterns.len() > options.limits.max_patterns)
    {
        return Err(invalid("pattern count limit exceeded"));
    }
    let allowlist = metadata
        .files
        .as_ref()
        .map(|patterns| {
            patterns
                .iter()
                .map(|pattern| Pattern::parse(pattern, false, ""))
                .collect::<io::Result<Vec<_>>>()
        })
        .transpose()?;
    let mut required = std::collections::BTreeSet::new();
    for target in metadata
        .main
        .into_iter()
        .chain(manifest.bin().into_iter().flat_map(|bin| {
            bin.targets()
                .iter()
                .map(|target| target.target.to_str().unwrap())
        }))
    {
        required.insert(target.strip_prefix("./").unwrap_or(target).to_owned());
    }
    required.insert("package.json".into());
    let patterns = allowlist.as_ref().map_or(0, Vec::len);
    let mut capture = Capture {
        options,
        visited: 0,
        total: 0,
        keys: BTreeMap::new(),
        files: BTreeMap::new(),
        allowlist,
        required,
        manifest_bytes: &manifest_bytes,
        ignore_bytes: 0,
        patterns,
        steps: options.limits.max_match_steps,
    };
    capture.walk(&root, "", 0, &[])?;
    for target in &capture.required {
        validate_path(target, options.limits.max_path_bytes)?;
        if !capture.files.contains_key(target) {
            return Err(invalid(format!(
                "required target is absent or excluded: {target}"
            )));
        }
    }
    if let Some(bin) = manifest.bin() {
        for target in bin.targets() {
            capture
                .files
                .get_mut(target.target.to_str().unwrap())
                .unwrap()
                .1 = 0o755;
        }
    }
    let output = Capped {
        inner: Vec::new(),
        remaining: options.limits.max_archive_bytes,
    };
    let gzip = GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(output, Compression::new(6));
    let stream = Capped {
        inner: gzip,
        remaining: options.limits.max_tar_bytes,
    };
    let mut tar = tar::Builder::new(stream);
    let mut files = Vec::new();
    for (path, (bytes, mode)) in capture.files {
        let mut header = tar::Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_mode(mode);
        header.set_size(bytes.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append_data(&mut header, format!("package/{path}"), bytes.as_slice())?;
        files.push(NpmFile {
            path,
            size: bytes.len() as u64,
            digest: artifact_digest(&bytes),
            mode,
        });
    }
    let bytes = tar.into_inner()?.inner.finish()?.inner;
    let digest = artifact_digest(&bytes);
    Ok(NpmArtifact {
        gitignore_not_used: root.join(".gitignore").exists() && !root.join(".npmignore").exists(),
        name: manifest.name().to_string(),
        version: manifest.version().to_string(),
        files,
        bytes,
        digest,
    })
}

struct Capture<'a> {
    options: &'a NpmPackOptions,
    visited: usize,
    total: u64,
    keys: BTreeMap<String, String>,
    files: BTreeMap<String, (Vec<u8>, u32)>,
    allowlist: Option<Vec<Pattern>>,
    required: std::collections::BTreeSet<String>,
    manifest_bytes: &'a [u8],
    ignore_bytes: u64,
    patterns: usize,
    steps: u64,
}
impl Capture<'_> {
    fn walk(
        &mut self,
        directory: &Path,
        prefix: &str,
        depth: usize,
        inherited: &[Pattern],
    ) -> io::Result<()> {
        if depth > self.options.limits.max_depth {
            return Err(invalid("directory depth limit exceeded"));
        }
        let mut rules = inherited.to_vec();
        let ignore_path = directory.join(".npmignore");
        if !(prefix.is_empty() && self.allowlist.is_some())
            && fs::symlink_metadata(&ignore_path).is_ok()
        {
            let remaining = self
                .options
                .limits
                .max_ignore_bytes
                .checked_sub(self.ignore_bytes)
                .ok_or_else(|| invalid("ignore byte limit exceeded"))?;
            let bytes = read_bounded(
                &ignore_path,
                remaining.min(self.options.limits.max_ignore_file_bytes),
            )?;
            self.ignore_bytes += bytes.len() as u64;
            let text =
                std::str::from_utf8(&bytes).map_err(|_| invalid("ignore policy must be UTF-8"))?;
            for (line, text) in text.lines().enumerate() {
                if text.is_empty() || text.starts_with('#') {
                    continue;
                }
                self.patterns += 1;
                if self.patterns > self.options.limits.max_patterns {
                    return Err(invalid("pattern count limit exceeded"));
                }
                rules.push(Pattern::parse(text, true, prefix).map_err(|error| {
                    invalid(format!("{prefix}/.npmignore line {}: {error}", line + 1))
                })?);
            }
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            self.visited = self
                .visited
                .checked_add(1)
                .ok_or_else(|| invalid("entry count overflow"))?;
            if self.visited > self.options.limits.max_visited_entries {
                return Err(invalid("visited entry limit exceeded"));
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid("non-UTF-8 filename"))?;
            if excluded(&name) || self.options.destination.as_ref() == Some(&entry.path()) {
                continue;
            }
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            validate_path(&path, self.options.limits.max_path_bytes)?;
            let key = path_collision_key(&path);
            if let Some(previous) = self.keys.insert(key, path.clone())
                && previous != path
            {
                return Err(invalid(format!("case collision: {previous} and {path}")));
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() && entry.path().is_dir() {
                return Err(invalid(format!("directory symlink: {path}")));
            }
            if kind.is_dir() {
                self.walk(&entry.path(), &path, depth + 1, &rules)?;
            } else {
                let mandatory = path == "package.json"
                    || self.required.contains(&path)
                    || (prefix.is_empty() && mandatory_document(&path));
                let mut selected = self.allowlist.is_none();
                if let Some(patterns) = &self.allowlist {
                    for pattern in patterns {
                        if pattern.matches(&path, &mut self.steps)? {
                            selected = true;
                        }
                    }
                }
                let mut ignored = false;
                for pattern in &rules {
                    if pattern.matches(&path, &mut self.steps)? {
                        ignored = !pattern.negated;
                    }
                }
                selected &= !ignored;
                if !selected && !mandatory {
                    continue;
                }
                if !kind.is_file() {
                    return Err(invalid(format!("unsupported file kind: {path}")));
                }
                if self.files.len() >= self.options.limits.max_files {
                    return Err(invalid("file count limit exceeded"));
                }
                let remaining = self
                    .options
                    .limits
                    .max_content_bytes
                    .checked_sub(self.total)
                    .ok_or_else(|| invalid("content limit exceeded"))?;
                let limit = remaining.min(self.options.limits.max_file_bytes);
                let bytes = if path == "package.json" {
                    if self.manifest_bytes.len() as u64 > limit {
                        return Err(invalid("content byte limit exceeded"));
                    }
                    self.manifest_bytes.to_vec()
                } else {
                    read_bounded(&entry.path(), limit)?
                };
                self.total = self
                    .total
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| invalid("content byte count overflow"))?;
                let mode = file_mode(&entry.metadata()?);
                self.files.insert(path, (bytes, mode));
            }
        }
        Ok(())
    }
}
fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid("expected regular file"));
    }
    let file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid("file size or kind limit exceeded"));
    }
    read_capped(file, limit)
}
fn read_capped(reader: impl Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("file grew beyond byte limit"));
    }
    Ok(bytes)
}
#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}
#[cfg(not(unix))]
fn file_mode(_: &fs::Metadata) -> u32 {
    0o644
}
fn excluded(name: &str) -> bool {
    let name = name.to_lowercase();
    [
        ".git",
        ".hg",
        ".svn",
        "node_modules",
        "target",
        ".tapid",
        ".npm",
        ".npmrc",
        ".yarnrc",
        ".yarnrc.yml",
        ".netrc",
        ".npmignore",
        ".env",
        ".ds_store",
        "package-lock.json",
        "npm-shrinkwrap.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lock",
        "bun.lockb",
        "tapid.lock",
    ]
    .contains(&name.as_str())
        || name.starts_with(".env.")
        || name.starts_with("npm-debug.log")
        || name.starts_with(".tapid-pack-")
        || name.ends_with(".tgz")
        || name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swo")
        || (name.starts_with('#') && name.ends_with('#'))
}
fn mandatory_document(path: &str) -> bool {
    let name = path.split('.').next().unwrap_or("").to_ascii_uppercase();
    ["README", "LICENSE", "LICENCE", "COPYING"].contains(&name.as_str())
}
fn validate_path(path: &str, limit: usize) -> io::Result<()> {
    if path.len() > limit {
        return Err(invalid("path byte limit exceeded"));
    }
    for component in path.split('/') {
        let stem = component
            .split('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with(['.', ' '])
            || component
                .chars()
                .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
            || ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| ["¹", "²", "³"].contains(&suffix))
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(invalid(format!("unsafe package path: {path:?}")));
        }
    }
    Ok(())
}
struct Capped<W> {
    inner: W,
    remaining: u64,
}
impl<W: Write> Write for Capped<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(invalid("encoded byte limit exceeded"));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_stops_when_opened_file_grows_after_metadata_check() {
        let project = tapid_test_support::TempProject::new("npm-growing-file").unwrap();
        project.write("growing", b"small").unwrap();
        let file = fs::File::open(project.path().join("growing")).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 5);
        project
            .write("growing", b"larger than the byte budget")
            .unwrap();
        assert!(read_capped(file, 8).is_err());
    }
}
