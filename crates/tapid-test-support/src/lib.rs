//! Reusable, dependency-free test infrastructure for Tapid crates.
//!
//! The helpers in this crate deliberately avoid production dependencies and network
//! access. They provide isolated filesystem fixtures, deterministic names, an
//! in-memory registry seam, and adversarial inputs for boundary testing.

#![deny(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Returns a deterministic, filesystem-safe fixture name.
///
/// The ordinal is zero-padded so listings remain naturally sorted. Unsafe or
/// platform-specific punctuation is replaced with `-`.
pub fn fixture_name(label: &str, ordinal: usize) -> String {
    let mut slug = String::new();
    for character in label.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "fixture" } else { slug };
    format!("{slug}-{ordinal:03}")
}

fn temporary_root(label: &str) -> io::Result<PathBuf> {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let name = format!("{}-{}-{}", fixture_name(label, 0), std::process::id(), id);
    let path = std::env::temp_dir().join("tapid-test-support").join(name);
    fs::create_dir_all(&path)?;
    Ok(path)
}

/// An isolated temporary project directory removed when dropped.
pub struct TempProject {
    root: PathBuf,
}

impl TempProject {
    /// Creates a uniquely isolated project under the platform temp directory.
    pub fn new(label: &str) -> io::Result<Self> {
        Ok(Self {
            root: temporary_root(&format!("project-{label}"))?,
        })
    }

    /// Returns the project root.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Writes a file below the project root, rejecting path escapes.
    pub fn write(&self, relative: impl AsRef<Path>, contents: &[u8]) -> io::Result<PathBuf> {
        write_relative(&self.root, relative.as_ref(), contents)
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// An isolated temporary home directory removed when dropped.
pub struct TempHome {
    root: PathBuf,
}

impl TempHome {
    /// Creates a uniquely isolated home under the platform temp directory.
    pub fn new(label: &str) -> io::Result<Self> {
        Ok(Self {
            root: temporary_root(&format!("home-{label}"))?,
        })
    }

    /// Returns the home root.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Writes a file below the home root, rejecting path escapes.
    pub fn write(&self, relative: impl AsRef<Path>, contents: &[u8]) -> io::Result<PathBuf> {
        write_relative(&self.root, relative.as_ref(), contents)
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_relative(root: &Path, relative: &Path, contents: &[u8]) -> io::Result<PathBuf> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture path escapes its root",
        ));
    }
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, contents)?;
    Ok(path)
}

/// An in-memory registry seam for tests. It never opens sockets or touches disk.
#[derive(Debug, Default)]
pub struct FakeRegistry {
    origin: String,
    packages: BTreeMap<String, (Vec<u8>, Vec<u8>)>,
}

impl FakeRegistry {
    /// Creates an empty fake registry with a caller-supplied origin.
    pub fn new(origin: impl Into<String>) -> Self {
        Self {
            origin: origin.into().trim_end_matches('/').to_owned(),
            packages: BTreeMap::new(),
        }
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Inserts metadata and archive bytes, copying both inputs.
    pub fn insert(&mut self, name: &str, version: &str, metadata: &[u8], archive: &[u8]) {
        self.packages.insert(
            format!("{name}@{version}"),
            (metadata.to_vec(), archive.to_vec()),
        );
    }

    pub fn metadata(&self, name: &str, version: &str) -> Option<Vec<u8>> {
        self.packages
            .get(&format!("{name}@{version}"))
            .map(|(metadata, _)| metadata.clone())
    }

    pub fn archive(&self, name: &str, version: &str) -> Option<Vec<u8>> {
        self.packages
            .get(&format!("{name}@{version}"))
            .map(|(_, archive)| archive.clone())
    }

    /// Returns sorted package identities and deterministic archive URLs.
    pub fn packages(&self) -> Vec<(String, String)> {
        self.packages
            .keys()
            .map(|identity| {
                let (name, version) = identity
                    .rsplit_once('@')
                    .expect("registry identity contains version");
                let url_name = name.trim_start_matches('@').replace('/', "-");
                (
                    identity.clone(),
                    format!("{}/{name}/-/{}-{version}.tgz", self.origin, url_name),
                )
            })
            .collect()
    }
}

/// Boundary-oriented inputs for path, encoding, and size validation tests.
pub fn adversarial_inputs() -> Vec<String> {
    vec![
        "../escape".to_owned(),
        "..\\escape".to_owned(),
        "/absolute/path".to_owned(),
        "C:\\\\absolute\\path".to_owned(),
        "nested/../../escape".to_owned(),
        "nul\0byte".to_owned(),
        "".to_owned(),
        "こんにちは".to_owned(),
        "name with spaces".to_owned(),
    ]
}

/// Returns the current crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_names_are_stable_and_safe() {
        assert_eq!(
            fixture_name("archive traversal", 7),
            "archive-traversal-007"
        );
        assert_eq!(
            fixture_name("archive traversal", 7),
            fixture_name("archive traversal", 7)
        );
        assert!(!fixture_name("../unsafe", 1).contains(".."));
    }

    #[test]
    fn temporary_project_and_home_are_isolated() {
        let project = TempProject::new("cli test").unwrap();
        let home = TempHome::new("cli test").unwrap();
        assert!(project.path().is_dir());
        assert!(home.path().is_dir());
        assert_ne!(project.path(), home.path());
        project.write("package.json", b"{}\n").unwrap();
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"{}\n"
        );
    }

    #[test]
    fn fake_registry_is_deterministic_and_returns_owned_data() {
        let mut registry = FakeRegistry::new("https://registry.test");
        registry.insert("demo", "1.0.0", b"metadata", b"archive");
        assert_eq!(registry.origin(), "https://registry.test");
        assert_eq!(
            registry.metadata("demo", "1.0.0"),
            Some(b"metadata".to_vec())
        );
        assert_eq!(registry.archive("demo", "1.0.0"), Some(b"archive".to_vec()));
        let listing = registry.packages();
        assert_eq!(
            listing,
            vec![(
                "demo@1.0.0".to_owned(),
                "https://registry.test/demo/-/demo-1.0.0.tgz".to_owned()
            )]
        );
    }

    #[test]
    fn adversarial_fixtures_cover_boundary_inputs() {
        let inputs = adversarial_inputs();
        assert!(inputs.iter().any(|input| input == "../escape"));
        assert!(inputs.iter().any(|input| input.contains('\0')));
        assert!(inputs.iter().any(|input| input.starts_with('/')));
    }
}
