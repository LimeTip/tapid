//! Store-local authentication distinguishes executed outputs from ordinary
//! ingested package trees, even when a project lockfile has been forged.
use super::*;
use hmac::{Hmac, Mac};

impl Store {
    pub fn attest_lifecycle_output(
        &self,
        recipe: &ArtifactDigest,
        output: &ArtifactDigest,
    ) -> Result<String, IngestError> {
        let _guard = lock_file(&self.root, true)?;
        let key = lifecycle_key(&self.root, true)?;
        let mac = output_mac(&key, recipe, output);
        Ok(format!(
            "hmac-sha256-{}",
            hex::encode(mac.finalize().into_bytes())
        ))
    }
    pub fn verified_lifecycle_snapshot(
        &self,
        recipe: &ArtifactDigest,
        output: &ArtifactDigest,
        attestation: &str,
    ) -> Result<PathBuf, IngestError> {
        let key = lifecycle_key(&self.root, false)?;
        let tag = attestation
            .strip_prefix("hmac-sha256-")
            .and_then(|s| hex::decode(s).ok())
            .ok_or_else(|| invalid("invalid lifecycle output attestation"))?;
        output_mac(&key, recipe, output)
            .verify_slice(&tag)
            .map_err(|_| invalid("lifecycle output attestation does not match recipe/tree"))?;
        self.verified_tree_snapshot(output)
    }
}
fn invalid(message: &str) -> IngestError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}
fn output_mac(key: &[u8], recipe: &ArtifactDigest, output: &ArtifactDigest) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key");
    for value in [
        "tapid-lifecycle-output-v1",
        recipe.as_str(),
        output.as_str(),
    ] {
        mac.update(&(value.len() as u64).to_le_bytes());
        mac.update(value.as_bytes());
    }
    mac
}
#[cfg(unix)]
fn lifecycle_key(root: &Path, create: bool) -> Result<[u8; 32], IngestError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = root.join(".tapid-lifecycle-key");
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
            let mut key = [0u8; 32];
            File::open("/dev/urandom")?.read_exact(&mut key)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)?;
            if let Err(error) = file.write_all(&key).and_then(|()| file.sync_all()) {
                let _ = fs::remove_file(&path);
                return Err(error.into());
            }
            File::open(root)?.sync_all()?;
            return Ok(key);
        }
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() != 32
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(invalid(
            "lifecycle authentication key must be an owner-only regular 32-byte file",
        ));
    }
    let mut key = [0u8; 32];
    file.read_exact(&mut key)?;
    let mut extra = [0u8; 1];
    if file.read(&mut extra)? != 0 {
        return Err(invalid(
            "lifecycle authentication key changed while reading",
        ));
    }
    Ok(key)
}
#[cfg(not(unix))]
fn lifecycle_key(_root: &Path, _create: bool) -> Result<[u8; 32], IngestError> {
    Err(invalid(
        "lifecycle output authentication currently requires a Unix store",
    ))
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use tapid_test_support::{TempHome, TempProject};
    #[test]
    fn lifecycle_authentication_rejects_forged_recipe_tree_and_missing_key() {
        let home = TempHome::new("lifecycle-auth").unwrap();
        let source = TempProject::new("lifecycle-auth-tree").unwrap();
        source.write("output", b"generated").unwrap();
        let store = Store::new(home.path().join("store"));
        let tree: ArtifactDigest = tapid_archive::canonical_tree_digest(source.path())
            .unwrap()
            .parse()
            .unwrap();
        let recipe: ArtifactDigest = format!("sha256-{}", "a".repeat(64)).parse().unwrap();
        let other: ArtifactDigest = format!("sha256-{}", "b".repeat(64)).parse().unwrap();
        assert!(
            store
                .verified_lifecycle_snapshot(
                    &recipe,
                    &tree,
                    &format!("hmac-sha256-{}", "0".repeat(64))
                )
                .is_err()
        );
        assert!(!store.root().join(".tapid-lifecycle-key").exists());
        let mut transaction = store.transaction();
        transaction
            .stage_verified_tree(&tree, source.path())
            .unwrap();
        transaction.publish().unwrap().commit().unwrap();
        let tag = store.attest_lifecycle_output(&recipe, &tree).unwrap();
        let snapshot = store
            .verified_lifecycle_snapshot(&recipe, &tree, &tag)
            .unwrap();
        fs::remove_dir_all(snapshot).unwrap();
        assert!(
            store
                .verified_lifecycle_snapshot(&other, &tree, &tag)
                .is_err()
        );
        assert!(
            store
                .verified_lifecycle_snapshot(&recipe, &other, &tag)
                .is_err()
        );
        assert!(
            store
                .verified_lifecycle_snapshot(
                    &recipe,
                    &tree,
                    &format!("hmac-sha256-{}", "0".repeat(64))
                )
                .is_err()
        );
        fs::remove_file(store.root().join(".tapid-lifecycle-key")).unwrap();
        assert!(
            store
                .verified_lifecycle_snapshot(&recipe, &tree, &tag)
                .is_err()
        );
        assert!(!store.root().join(".tapid-lifecycle-key").exists());
    }
    #[test]
    #[cfg(unix)]
    fn lifecycle_authentication_refuses_symlinked_or_readable_keys() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempHome::new("lifecycle-auth-key").unwrap();
        let store = Store::new(home.path().join("store"));
        let digest: ArtifactDigest = format!("sha256-{}", "a".repeat(64)).parse().unwrap();
        store.attest_lifecycle_output(&digest, &digest).unwrap();
        let key = store.root().join(".tapid-lifecycle-key");
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.attest_lifecycle_output(&digest, &digest).is_err());
        fs::remove_file(&key).unwrap();
        let external = home.path().join("external-key");
        fs::write(&external, [1u8; 32]).unwrap();
        std::os::unix::fs::symlink(&external, &key).unwrap();
        assert!(store.attest_lifecycle_output(&digest, &digest).is_err());
        assert_eq!(fs::read(external).unwrap(), vec![1u8; 32]);
    }
    #[cfg(windows)]
    #[test]
    fn lifecycle_authentication_rejects_corrupt_and_hardlinked_windows_keys() {
        let home = TempHome::new("windows-lifecycle-key").unwrap();
        let store = Store::new(home.path().join("store"));
        let digest: ArtifactDigest = format!("sha256-{}", "a".repeat(64)).parse().unwrap();
        store.attest_lifecycle_output(&digest, &digest).unwrap();
        let path = store.root().join(".tapid-lifecycle-key");
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"invalid protected key").unwrap();
        assert!(store.attest_lifecycle_output(&digest, &digest).is_err());
        fs::write(&path, &original).unwrap();
        fs::hard_link(&path, home.path().join("key-alias")).unwrap();
        assert!(store.attest_lifecycle_output(&digest, &digest).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}
