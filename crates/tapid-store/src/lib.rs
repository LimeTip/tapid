use std::path::{Path, PathBuf};

use tapid_core::ArtifactDigest;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn artifact_path(&self, digest: &ArtifactDigest) -> PathBuf {
        self.root.join("artifacts").join(digest.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn derives_stable_paths_from_verified_digests() {
        let store = Store::new("/tmp/tapid");
        let digest = ArtifactDigest::from_str(&format!("sha256-{}", "a".repeat(64))).unwrap();
        assert_eq!(
            store.artifact_path(&digest),
            PathBuf::from("/tmp/tapid/artifacts/").join(digest.as_str())
        );
    }
}
