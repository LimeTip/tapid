use super::*;
use tapid_test_support::{TempHome, TempProject};

fn populated() -> (TempHome, Store, PathBuf, PathBuf) {
    let home = TempHome::new("cache-maintenance").unwrap();
    let store = Store::new(home.path().join("store"));
    let bytes = b"cached artifact";
    let digest = digest_bytes(bytes).parse().unwrap();
    store.ingest(&digest, &bytes[..]).unwrap();
    let source = TempProject::new("cached-tree").unwrap();
    source.write("package.json", b"{}").unwrap();
    let tree_digest = tapid_archive::canonical_tree_digest(source.path())
        .unwrap()
        .parse()
        .unwrap();
    let tree = store
        .activate_verified_tree(&tree_digest, source.path())
        .unwrap();
    let artifact = store.artifact_path(&digest);
    (home, store, artifact, tree)
}

#[test]
fn cache_maintenance_preserves_project_state_staging_keys_and_unknown_entries() {
    let (_home, store, artifact, tree) = populated();
    for (name, bytes) in [
        ("package.json", b"manifest".as_slice()),
        ("tapid.lock", b"lock"),
        ("node_modules/keep", b"installed"),
        (".tapid-lifecycle-key", b"key"),
        (".staging/live/keep", b"active"),
        ("artifacts/user-file", b"unknown"),
        ("trees/unmarked/keep", b"unknown tree"),
    ] {
        let path = store.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let before = store.cache_info().unwrap();
    assert_eq!(
        before.artifacts,
        CacheUsage {
            entries: 1,
            bytes: 15
        }
    );
    assert_eq!(before.trees.entries, 1);
    assert_eq!(before.trees.bytes, 2 + 71);
    assert_eq!(before.preserved_entries, 2);
    assert_eq!(store.clean_cache().unwrap(), before);
    assert!(!artifact.exists());
    assert!(!tree.exists());
    for name in [
        "package.json",
        "tapid.lock",
        "node_modules/keep",
        ".tapid-lifecycle-key",
        ".staging/live/keep",
        "artifacts/user-file",
        "trees/unmarked/keep",
        ".store.lock",
    ] {
        assert!(store.root.join(name).exists(), "{name}");
    }
    assert_eq!(store.clean_cache().unwrap().trees.entries, 0);
}

#[test]
fn cache_maintenance_missing_and_empty_stores_create_nothing() {
    let home = TempHome::new("empty-cache").unwrap();
    let root = home.path().join("absent");
    let store = Store::new(&root);
    assert_eq!(store.cache_info().unwrap(), CacheSummary::default());
    assert_eq!(store.clean_cache().unwrap(), CacheSummary::default());
    assert!(!root.exists());
    fs::create_dir(&root).unwrap();
    assert_eq!(store.cache_info().unwrap(), CacheSummary::default());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::create_dir(root.join("artifacts")).unwrap();
    assert!(store.clean_cache().is_err());
    assert!(!root.join(".store.lock").exists());
}

#[test]
fn cache_maintenance_refuses_pending_recovery_without_changing_it() {
    let (_home, store, artifact, tree) = populated();
    fs::write(store.root.join(STORE_JOURNAL), b"malformed journal").unwrap();
    assert!(store.cache_info().is_err());
    assert!(store.clean_cache().is_err());
    assert!(artifact.exists());
    assert!(tree.exists());
    assert_eq!(
        fs::read(store.root.join(STORE_JOURNAL)).unwrap(),
        b"malformed journal"
    );
}

#[test]
fn cache_maintenance_refuses_active_readers_and_publications() {
    let (_home, store, artifact, tree) = populated();
    let reader = store.read_guard().unwrap();
    assert!(store.cache_info().is_ok());
    assert!(
        matches!(store.clean_cache(), Err(IngestError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock)
    );
    assert!(artifact.exists());
    assert!(tree.exists());
    drop(reader);
    let publication = store.transaction().publish().unwrap();
    assert!(
        matches!(store.cache_info(), Err(IngestError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock)
    );
    assert!(store.clean_cache().is_err());
    drop(publication);
    assert!(store.clean_cache().is_ok());
}

#[cfg(unix)]
#[test]
fn cache_maintenance_never_follows_symlinked_roots_namespaces_locks_or_entries() {
    use std::os::unix::fs::symlink;
    let (home, store, artifact, _tree) = populated();
    let outside = home.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"external").unwrap();
    let link = home.path().join("linked-root");
    symlink(store.root(), &link).unwrap();
    assert!(Store::new(link).clean_cache().is_err());
    fs::rename(store.root.join("trees"), store.root.join("saved-trees")).unwrap();
    symlink(&outside, store.root.join("trees")).unwrap();
    assert!(store.cache_info().is_err());
    assert!(store.clean_cache().is_err());
    assert!(
        artifact.exists(),
        "all namespaces must be checked before deletion"
    );
    fs::remove_file(store.root.join("trees")).unwrap();
    fs::rename(store.root.join("saved-trees"), store.root.join("trees")).unwrap();
    let digest = format!("sha256-{}", "b".repeat(64));
    symlink(&outside, store.root.join("trees").join(&digest)).unwrap();
    symlink(
        outside.join("keep"),
        store.root.join("artifacts").join(&digest),
    )
    .unwrap();
    assert_eq!(store.cache_info().unwrap().preserved_entries, 2);
    fs::remove_file(store.root.join(".store.lock")).unwrap();
    symlink(outside.join("keep"), store.root.join(".store.lock")).unwrap();
    assert!(store.clean_cache().is_err());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"external");
}

#[test]
fn cache_maintenance_preserves_malformed_and_unmarked_entries() {
    let (_home, store, artifact, _tree) = populated();
    let digest = format!("sha256-{}", "c".repeat(64));
    let tree = store.root.join("trees").join(&digest);
    fs::create_dir(&tree).unwrap();
    fs::write(tree.join(".tapid-tree"), b"wrong digest").unwrap();
    fs::create_dir(store.root.join("artifacts").join(&digest)).unwrap();
    assert_eq!(store.cache_info().unwrap().preserved_entries, 2);
    store.clean_cache().unwrap();
    assert!(tree.join(".tapid-tree").exists());
    assert!(store.root.join("artifacts").join(digest).exists());
    assert!(!artifact.exists());
}

#[cfg(unix)]
#[test]
fn cache_maintenance_counts_and_removes_tree_links_without_following_targets() {
    use std::os::unix::fs::symlink;
    let (home, store, _artifact, tree) = populated();
    let target = home.path().join("external-file");
    fs::write(&target, b"unrelated bytes").unwrap();
    symlink(&target, tree.join("link")).unwrap();
    assert_eq!(store.cache_info().unwrap().trees.bytes, 73);
    store.clean_cache().unwrap();
    assert_eq!(fs::read(target).unwrap(), b"unrelated bytes");
}

#[test]
fn cache_maintenance_refuses_artifact_ingestion_in_progress() {
    struct PausedReader {
        started: Option<std::sync::mpsc::Sender<()>>,
        resume: std::sync::mpsc::Receiver<()>,
        bytes: &'static [u8],
    }
    impl Read for PausedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
                self.resume.recv().unwrap();
            }
            self.bytes.read(buffer)
        }
    }
    let (_home, store, _artifact, _tree) = populated();
    let bytes = b"new artifact";
    let digest: ArtifactDigest = digest_bytes(bytes).parse().unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let worker_store = store.clone();
    let worker_digest = digest.clone();
    let worker = std::thread::spawn(move || {
        worker_store.ingest(
            &worker_digest,
            PausedReader {
                started: Some(started_tx),
                resume: resume_rx,
                bytes,
            },
        )
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let result = store.clean_cache();
    resume_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert!(matches!(result, Err(IngestError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock));
    assert_eq!(fs::read(store.artifact_path(&digest)).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn cache_maintenance_reports_cleanup_failure_after_eviction_starts() {
    use std::os::unix::fs::PermissionsExt;
    // Root ignores these permissions, so this fixture needs an ordinary user.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let (_home, store, artifact, _tree) = populated();
    let directory = store.root.join("artifacts");
    let original = fs::metadata(&directory).unwrap().permissions();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    let result = store.clean_cache();
    fs::set_permissions(&directory, original).unwrap();
    assert!(matches!(result, Err(IngestError::CacheCleanup(_))));
    assert!(artifact.exists());
    assert!(store.clean_cache().is_ok());
}
