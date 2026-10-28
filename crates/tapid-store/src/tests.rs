use super::*;
use std::io::Cursor;
use std::str::FromStr;
fn digest(data: &[u8]) -> ArtifactDigest {
    let mut h = Sha256::new();
    h.update(data);
    ArtifactDigest::from_str(&format!("sha256-{}", hex::encode(h.finalize()))).unwrap()
}
fn root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "tapid-store-test-{}-{}",
        std::process::id(),
        unique_nonce()
    ))
}

fn make_marked_tree(root: &Path, contents: &str) -> ArtifactDigest {
    let tree = root.join("trees").join(unique_nonce().to_string());
    fs::create_dir_all(&tree).unwrap();
    fs::write(tree.join("package.json"), contents).unwrap();
    let digest = tapid_archive::canonical_tree_digest(&tree)
        .unwrap()
        .parse::<ArtifactDigest>()
        .unwrap();
    fs::write(tree.join(".tapid-tree"), digest.as_str()).unwrap();
    let destination = root.join("trees").join(digest.as_str());
    fs::rename(&tree, &destination).unwrap();
    digest
}

#[test]
fn verified_tree_miss_is_distinct_from_a_missing_marker_or_symlink() {
    let project = tapid_test_support::TempProject::new("tree-miss").unwrap();
    let store = Store::new(project.path());
    let digest = digest(b"missing");
    assert!(matches!(
        store.verified_tree_path(&digest),
        Err(IngestError::TreeMissing(_))
    ));
    assert!(matches!(
        store.verified_tree_snapshot(&digest),
        Err(IngestError::TreeMissing(_))
    ));
    let tree = project.path().join("trees").join(digest.as_str());
    fs::create_dir_all(&tree).unwrap();
    assert!(!matches!(
        store.verified_tree_path(&digest).unwrap_err(),
        IngestError::TreeMissing(_)
    ));
    assert!(store.verified_tree_snapshot(&digest).is_err());
    #[cfg(unix)]
    {
        fs::remove_dir(&tree).unwrap();
        std::os::unix::fs::symlink(project.path().join("absent"), &tree).unwrap();
        assert!(!matches!(
            store.verified_tree_path(&digest).unwrap_err(),
            IngestError::TreeMissing(_)
        ));
        assert!(store.verified_tree_snapshot(&digest).is_err());
    }
}

fn coordinator(project: &Path, state: &str) -> PathBuf {
    fs::create_dir_all(project).unwrap();
    let path = project.join(".tapid-lifecycle-journal.json");
    fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "owner": "123-deadbeef\\n",
            "state": state,
            "manifest_existed": true,
            "manifest": "e30=",
            "lock_existed": false,
            "lock": "",
            "marker_existed": false,
            "node_modules_existed": false,
            "store_root": null
        })
        .to_string(),
    )
    .unwrap();
    path
}

fn version_two_coordinator(project: &Path, state: &str) -> PathBuf {
    let path = coordinator(project, state);
    let mut decision: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    decision["version"] = serde_json::Value::from(2);
    fs::write(&path, decision.to_string()).unwrap();
    path
}

#[test]
fn prepared_coordinator_recovery_removes_only_transaction_created_tree() {
    let root = root();
    let project = root.with_extension("project");
    let store_root = root.join("store");
    fs::create_dir_all(&store_root).unwrap();
    let old_digest = make_marked_tree(&store_root, "old baseline");
    let created_digest = make_marked_tree(&store_root, "new transaction");
    let coordinator = coordinator(&project, "Prepared");
    write_store_journal(
        &store_root,
        &coordinator,
        &[created_digest.as_str().to_owned()],
        false,
    )
    .unwrap();

    Store::new(&store_root).recover_transactions().unwrap();

    assert!(store_root.join("trees").join(old_digest.as_str()).exists());
    assert!(
        !store_root
            .join("trees")
            .join(created_digest.as_str())
            .exists()
    );
    assert!(!store_root.join(STORE_JOURNAL).exists());
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(project);
}

#[test]
fn committed_coordinator_recovery_keeps_published_tree() {
    let root = root();
    let project = root.with_extension("project");
    let store_root = root.join("store");
    fs::create_dir_all(&store_root).unwrap();
    let digest = make_marked_tree(&store_root, "committed tree");
    let coordinator = coordinator(&project, "Prepared");
    write_store_journal(
        &store_root,
        &coordinator,
        &[digest.as_str().to_owned()],
        false,
    )
    .unwrap();
    let mut decision: serde_json::Value =
        serde_json::from_slice(&fs::read(&coordinator).unwrap()).unwrap();
    decision["state"] = serde_json::Value::String("Committed".into());
    fs::write(&coordinator, decision.to_string()).unwrap();

    Store::new(&store_root).recover_transactions().unwrap();

    assert!(store_root.join("trees").join(digest.as_str()).exists());
    assert!(!store_root.join(STORE_JOURNAL).exists());
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(project);
}

#[test]
fn coordinator_larger_than_store_journal_limit_is_supported() {
    let root = root();
    let project = root.with_extension("large-coordinator-project");
    let store_root = root.join("store");
    fs::create_dir_all(&store_root).unwrap();
    let digest = make_marked_tree(&store_root, "large coordinator transaction");
    let coordinator = coordinator(&project, "Prepared");
    let mut decision: serde_json::Value =
        serde_json::from_slice(&fs::read(&coordinator).unwrap()).unwrap();
    decision["padding"] = serde_json::Value::String("x".repeat(2 * 1024 * 1024));
    fs::write(&coordinator, decision.to_string()).unwrap();

    write_store_journal(
        &store_root,
        &coordinator,
        &[digest.as_str().to_owned()],
        false,
    )
    .unwrap();
    Store::new(&store_root).recover_transactions().unwrap();

    assert!(!store_root.join("trees").join(digest.as_str()).exists());
    assert!(!store_root.join(STORE_JOURNAL).exists());
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(project);
}

#[test]
fn version_two_coordinator_recovery_removes_prepared_transaction_tree() {
    let root = root();
    let project = root.with_extension("v2-coordinator-project");
    let store_root = root.join("store");
    fs::create_dir_all(&store_root).unwrap();
    let created_digest = make_marked_tree(&store_root, "version two transaction");
    let coordinator = version_two_coordinator(&project, "Prepared");
    write_store_journal(
        &store_root,
        &coordinator,
        &[created_digest.as_str().to_owned()],
        false,
    )
    .unwrap();

    Store::new(&store_root).recover_transactions().unwrap();

    assert!(
        !store_root
            .join("trees")
            .join(created_digest.as_str())
            .exists()
    );
    assert!(!store_root.join(STORE_JOURNAL).exists());
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(project);
}

#[test]
fn version_three_coordinator_recovery_removes_prepared_transaction_tree() {
    let root = root();
    let project = root.with_extension("v3-coordinator-project");
    let store_root = root.join("store");
    fs::create_dir_all(&store_root).unwrap();
    let created_digest = make_marked_tree(&store_root, "version three transaction");
    let coordinator = version_two_coordinator(&project, "Prepared");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&coordinator).unwrap()).unwrap();
    record["version"] = serde_json::json!(3);
    record["members"] = serde_json::json!([]);
    fs::write(&coordinator, record.to_string()).unwrap();
    write_store_journal(
        &store_root,
        &coordinator,
        &[created_digest.as_str().to_owned()],
        false,
    )
    .unwrap();

    Store::new(&store_root).recover_transactions().unwrap();

    assert!(
        !store_root
            .join("trees")
            .join(created_digest.as_str())
            .exists()
    );
    assert!(!store_root.join(STORE_JOURNAL).exists());
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(project);
}

#[test]
fn version_four_coordinator_preserves_the_durable_store_decision() {
    for committed in [false, true] {
        let project = tapid_test_support::TempProject::new("v4-coordinator").unwrap();
        let store_root = project.path().join("store");
        fs::create_dir_all(&store_root).unwrap();
        let digest = make_marked_tree(&store_root, "version four transaction");
        let coordinator = coordinator(project.path(), "Prepared");
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&coordinator).unwrap()).unwrap();
        record["version"] = serde_json::json!(4);
        record["manifests"] = serde_json::json!([{"path":"apps/a/package.json","bytes":"e30="},{"path":"apps/b/package.json","bytes":"e30="}]);
        record["version"] = serde_json::json!(5);
        fs::write(&coordinator, record.to_string()).unwrap();
        assert!(
            write_store_journal(
                &store_root,
                &coordinator,
                &[digest.as_str().to_owned()],
                false
            )
            .is_err()
        );
        record["version"] = serde_json::json!(4);
        fs::write(&coordinator, record.to_string()).unwrap();
        // This exercises the publication reader as well as the recovery reader.
        write_store_journal(
            &store_root,
            &coordinator,
            &[digest.as_str().to_owned()],
            false,
        )
        .unwrap();
        record["version"] = serde_json::json!(5);
        fs::write(&coordinator, record.to_string()).unwrap();
        assert!(Store::new(&store_root).recover_transactions().is_err());
        assert!(store_root.join("trees").join(digest.as_str()).exists());
        assert!(store_root.join(STORE_JOURNAL).exists());
        record["version"] = serde_json::json!(4);
        if committed {
            record["state"] = serde_json::json!("Committed");
        }
        fs::write(&coordinator, record.to_string()).unwrap();
        Store::new(&store_root).recover_transactions().unwrap();
        assert_eq!(
            store_root.join("trees").join(digest.as_str()).exists(),
            committed
        );
        assert!(!store_root.join(STORE_JOURNAL).exists());
    }
}

#[test]
fn missing_project_directory_rolls_back_orphaned_transaction() {
    let root = root();
    let project = root.with_extension("orphaned-project");
    let store_root = root.join("store");
    fs::create_dir_all(&store_root).unwrap();
    let digest = make_marked_tree(&store_root, "orphaned transaction");
    let coordinator = coordinator(&project, "Prepared");
    write_store_journal(
        &store_root,
        &coordinator,
        &[digest.as_str().to_owned()],
        false,
    )
    .unwrap();
    fs::remove_dir_all(&project).unwrap();

    Store::new(&store_root).recover_transactions().unwrap();

    assert!(!store_root.join("trees").join(digest.as_str()).exists());
    assert!(!store_root.join(STORE_JOURNAL).exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn missing_coordinator_fails_closed_without_deleting_tree() {
    let root = root();
    let project = root.with_extension("project");
    let store_root = root.join("store");
    fs::create_dir_all(&store_root).unwrap();
    let digest = make_marked_tree(&store_root, "do not delete");
    let coordinator = coordinator(&project, "Prepared");
    write_store_journal(
        &store_root,
        &coordinator,
        &[digest.as_str().to_owned()],
        false,
    )
    .unwrap();
    fs::remove_file(&coordinator).unwrap();

    assert!(Store::new(&store_root).recover_transactions().is_err());
    assert!(store_root.join("trees").join(digest.as_str()).exists());
    assert!(store_root.join(STORE_JOURNAL).exists());
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(project);
}

#[test]
fn streams_verifies_and_atomically_activates() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let data = b"hostile input";
    let store = Store::new(&root);
    let expected = digest(data);
    let result = store.ingest(&expected, Cursor::new(data)).unwrap();
    assert!(matches!(result, IngestResult::Activated(_)));
    assert_eq!(fs::read(store.artifact_path(&expected)).unwrap(), data);
    assert!(!root.join(".staging").read_dir().unwrap().any(|x| x.is_ok()));
    let _ = fs::remove_dir_all(root);
}
#[test]
fn rejects_bad_digest_and_leaves_no_activated_bytes() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let store = Store::new(&root);
    let expected = digest(b"right");
    assert!(matches!(
        store.ingest(&expected, Cursor::new(b"wrong")),
        Err(IngestError::DigestMismatch { .. })
    ));
    assert!(!store.artifact_path(&expected).exists());
    let _ = fs::remove_dir_all(root);
}
#[test]
fn existing_files_are_authoritative_and_idempotent() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let store = Store::new(&root);
    let data = b"one";
    let expected = digest(data);
    let path = store.artifact_path(&expected);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, data).unwrap();
    assert_eq!(
        store.ingest(&expected, Cursor::new(b"different")).unwrap(),
        IngestResult::AlreadyPresent(path)
    );
    let _ = fs::remove_dir_all(root);
}
#[test]
fn existing_wrong_bytes_are_rejected() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let store = Store::new(&root);
    let expected = digest(b"expected");
    let path = store.artifact_path(&expected);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"tampered").unwrap();
    assert!(matches!(
        store.ingest(&expected, Cursor::new(b"expected")),
        Err(IngestError::DigestMismatch { .. })
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
/// Reject a mismatched publication winner, preserving its bytes and cleaning staging.
fn racing_wrong_artifact_is_rejected_and_preserved() {
    let root = root();
    let store = Store::new(&root);
    let expected_bytes = b"expected artifact";
    let expected = digest(expected_bytes);
    let destination = store.artifact_path(&expected);
    struct CompetingReader {
        destination: PathBuf,
        bytes: &'static [u8],
        inserted: bool,
    }
    impl Read for CompetingReader {
        /// Insert mismatched bytes on the first read, after lookup and before publication.
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.inserted {
                fs::create_dir_all(self.destination.parent().unwrap())?;
                fs::write(&self.destination, b"attacker bytes")?;
                self.inserted = true;
            }
            let count = self.bytes.len().min(buffer.len());
            buffer[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            Ok(count)
        }
    }

    let result = store.ingest(
        &expected,
        CompetingReader {
            destination: destination.clone(),
            bytes: expected_bytes,
            inserted: false,
        },
    );

    assert!(matches!(result, Err(IngestError::DigestMismatch { .. })));
    assert_eq!(fs::read(&destination).unwrap(), b"attacker bytes");
    assert!(
        !root
            .join(".staging")
            .read_dir()
            .unwrap()
            .any(|entry| entry.is_ok())
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
/// Accept a verified matching publication winner and clean the losing staging file.
fn racing_matching_artifact_is_accepted_and_preserved() {
    let root = root();
    let store = Store::new(&root);
    let bytes = b"expected artifact";
    let expected = digest(bytes);
    let destination = store.artifact_path(&expected);
    struct CompetingReader {
        destination: PathBuf,
        bytes: &'static [u8],
        inserted: bool,
    }
    impl Read for CompetingReader {
        /// Insert matching bytes on the first read, deterministically forcing a collision.
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.inserted {
                fs::create_dir_all(self.destination.parent().unwrap())?;
                fs::write(&self.destination, self.bytes)?;
                self.inserted = true;
            }
            let count = self.bytes.len().min(buffer.len());
            buffer[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            Ok(count)
        }
    }
    let result = store.ingest(
        &expected,
        CompetingReader {
            destination: destination.clone(),
            bytes,
            inserted: false,
        },
    );
    assert!(matches!(result, Ok(IngestResult::AlreadyPresent(_))));
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert!(
        !root
            .join(".staging")
            .read_dir()
            .unwrap()
            .any(|entry| entry.is_ok())
    );
    let _ = fs::remove_dir_all(root);
}

/// Recognize unavailable Windows symlink support, including missing privilege 1314.
fn symlink_creation_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
    ) || error.raw_os_error() == Some(1314)
}

#[test]
/// Keep Windows privilege error 1314 eligible for the symlink probe skip.
fn windows_symlink_privilege_error_is_unavailable() {
    let error = io::Error::from_raw_os_error(1314);
    assert!(symlink_creation_unavailable(&error));
}

#[test]
/// Reject a competing symlink without changing it or its target, and clean staging.
fn racing_symlink_artifact_is_rejected_and_preserved() {
    let root = root();
    let store = Store::new(&root);
    let bytes = b"expected artifact";
    let expected = digest(bytes);
    let destination = store.artifact_path(&expected);
    fs::create_dir_all(&root).unwrap();
    let target = root.join("target");
    fs::write(&target, bytes).unwrap();
    #[cfg(windows)]
    {
        let probe = root.join("symlink-probe");
        match std::os::windows::fs::symlink_file(&target, &probe) {
            Ok(()) => fs::remove_file(probe).unwrap(),
            Err(error) if symlink_creation_unavailable(&error) => {
                eprintln!("skipping symlink race case: symlink creation unavailable: {error}");
                let _ = fs::remove_dir_all(root);
                return;
            }
            Err(error) => panic!("failed to probe Windows symlink support: {error}"),
        }
    }
    struct CompetingReader {
        destination: PathBuf,
        target: PathBuf,
        bytes: &'static [u8],
        inserted: bool,
    }
    impl Read for CompetingReader {
        /// Insert the symlink on the first read, after lookup and before publication.
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.inserted {
                fs::create_dir_all(self.destination.parent().unwrap())?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(&self.target, &self.destination)?;
                #[cfg(windows)]
                std::os::windows::fs::symlink_file(&self.target, &self.destination)?;
                self.inserted = true;
            }
            let count = self.bytes.len().min(buffer.len());
            buffer[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            Ok(count)
        }
    }
    let result = store.ingest(
        &expected,
        CompetingReader {
            destination: destination.clone(),
            target: target.clone(),
            bytes,
            inserted: false,
        },
    );
    #[cfg(unix)]
    assert!(matches!(
        result,
        Err(IngestError::Io(error)) if error.raw_os_error() == Some(libc::ELOOP)
    ));
    #[cfg(windows)]
    assert!(matches!(
        result,
        Err(IngestError::Io(error))
            if error.kind() == io::ErrorKind::AlreadyExists
                && error.to_string() == "artifact path is not a regular file"
    ));
    assert!(
        fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(target).unwrap(), bytes);
    assert!(
        !root
            .join(".staging")
            .read_dir()
            .unwrap()
            .any(|entry| entry.is_ok())
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
/// Ensure the Unix no-follow open rejects the final symlink.
fn no_follow_artifact_open_rejects_symlink() {
    let root = root();
    fs::create_dir_all(&root).unwrap();
    let target = root.join("target");
    let link = root.join("link");
    fs::write(&target, b"artifact").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let result = open_artifact_without_following_links(&link);

    assert!(result.is_err());
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
/// Use a subprocess deadline to prove FIFO verification rejects without blocking.
fn fifo_artifact_open_does_not_block() {
    if let Some(path) = std::env::var_os("TAPID_TEST_FIFO_PATH") {
        let expected = digest(b"artifact");
        let result = verify_existing_artifact(Path::new(&path), &expected);
        assert!(
            matches!(result, Err(IngestError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        return;
    }

    let root = root();
    fs::create_dir_all(&root).unwrap();
    let fifo = root.join("artifact-fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::fifo_artifact_open_does_not_block")
        .env("TAPID_TEST_FIFO_PATH", &fifo)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "FIFO verification subprocess failed: {status}"
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("opening a FIFO artifact blocked instead of rejecting it");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn transaction_stages_tree_without_publishing_it_and_rolls_back_on_drop() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"transactional").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<ArtifactDigest>()
        .unwrap();
    let store = Store::new(root.join("store"));
    let mut transaction = StoreTransaction::new(store.clone());

    let staged = transaction.stage_verified_tree(&digest, &source).unwrap();
    let published = store.root().join("trees").join(digest.as_str());
    assert!(staged.is_dir());
    assert!(!published.exists());
    let publication = transaction.publish().unwrap();
    assert!(published.is_dir());
    drop(publication);
    assert!(!published.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn verified_tree_lookup_does_not_deadlock_when_caller_holds_read_guard() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"{\"name\":\"guarded\"}").unwrap();
    let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    let store = Store::new(&root);
    store.activate_verified_tree(&digest, &source).unwrap();
    let _guard = store.read_guard().unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let worker_store = store.clone();
    std::thread::spawn(move || {
        let result = worker_store.verified_tree_path(&digest);
        let _ = sent.send(result.is_ok());
    });

    assert!(
        received
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("verified-tree lookup blocked while a read guard was held")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn store_readers_cannot_observe_trees_that_will_be_rolled_back() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"hidden-until-commit").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<ArtifactDigest>()
        .unwrap();
    let store = Store::new(root.join("store"));
    let mut transaction = store.transaction();
    transaction.stage_verified_tree(&digest, &source).unwrap();
    let publication = transaction.publish().unwrap();
    let reader_store = store.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let visible = reader_store.verified_tree_path(&digest).is_ok();
        result_tx.send(visible).unwrap();
    });

    started_rx.recv().unwrap();
    assert!(
        result_rx
            .recv_timeout(std::time::Duration::from_millis(40))
            .is_err()
    );
    drop(publication);
    assert!(
        !result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
    );
    reader.join().unwrap();
    assert!(!store.root().join("trees").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn transaction_commit_keeps_new_tree_published() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"committed").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<ArtifactDigest>()
        .unwrap();
    let store = Store::new(root.join("store"));
    let mut transaction = StoreTransaction::new(store.clone());
    transaction.stage_verified_tree(&digest, &source).unwrap();

    transaction.publish().unwrap().commit().unwrap();

    assert!(store.verified_tree_path(&digest).is_ok());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn transaction_rollback_never_removes_a_preexisting_tree() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"preexisting").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<ArtifactDigest>()
        .unwrap();
    let store = Store::new(root.join("store"));
    store.activate_verified_tree(&digest, &source).unwrap();
    let mut transaction = StoreTransaction::new(store.clone());
    transaction.stage_verified_tree(&digest, &source).unwrap();

    transaction.publish().unwrap();

    assert!(store.verified_tree_path(&digest).is_ok());
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn sync_tree_flushes_read_only_files() {
    use std::os::unix::fs::PermissionsExt;

    let root = root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let file = root.join("read-only");
    fs::write(&file, b"verified bytes").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();

    assert!(sync_tree(&root).is_ok());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn activation_rejects_source_that_does_not_match_digest() {
    let root = root();
    let source = root.join("source");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"tampered").unwrap();
    let store = Store::new(&root);
    let expected = digest(b"not the canonical tree digest");
    assert!(matches!(
        store.activate_verified_tree(&expected, &source),
        Err(IngestError::TreeDigestMismatch { .. })
    ));
    assert!(!store.artifact_path(&expected).exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn verified_tree_rejects_mutation_after_activation() {
    let root = root();
    let source = root.join("source");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"original").unwrap();
    let expected = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<ArtifactDigest>()
        .unwrap();
    let store = Store::new(&root);
    store.activate_verified_tree(&expected, &source).unwrap();
    fs::write(
        store
            .root()
            .join("trees")
            .join(expected.as_str())
            .join("package.json"),
        b"tampered",
    )
    .unwrap();
    assert!(matches!(
        store.verified_tree_path(&expected),
        Err(IngestError::TreeDigestMismatch { .. })
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn stale_replay_snapshot_cleanup_preserves_live_and_unrelated_entries() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let staging = root.join(".staging");
    let stale = staging.join("replay-tree-4242-1");
    let live = staging.join("replay-tree-4243-2");
    let unrelated = staging.join("tree-4242-3");
    for path in [&stale, &live, &unrelated] {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("data"), b"data").unwrap();
    }
    let store = Store::new(&root);

    store
        .cleanup_stale_replay_snapshots_with(|pid| pid == 4243)
        .unwrap();

    assert!(!stale.exists());
    assert!(live.is_dir());
    assert!(unrelated.is_dir());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn stale_shared_replay_lease_is_recovered_even_when_pid_was_reused() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let staging = root.join(".staging");
    fs::create_dir_all(&staging).unwrap();
    let stale = staging.join("replay-lease-4242-1");
    fs::write(&stale, b"lease-v1\n").unwrap();
    let store = Store::new(&root);

    store
        .cleanup_stale_replay_snapshots_with(|pid| pid == 4242)
        .unwrap();

    assert!(!stale.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unlocked_snapshot_is_stale_even_when_its_pid_was_reused() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let snapshot = root.join(".staging/replay-tree-4242-1");
    fs::create_dir_all(&snapshot).unwrap();
    fs::write(snapshot.join(".tapid-replay-lease"), b"lease-v1\n").unwrap();
    let store = Store::new(&root);

    store
        .cleanup_stale_replay_snapshots_with(|pid| pid == 4242)
        .unwrap();

    assert!(!snapshot.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn created_snapshot_holds_a_live_lease() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"{\"name\":\"leased\"}").unwrap();
    let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    let destination = root.join("trees").join(digest.as_str());
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::rename(&source, &destination).unwrap();
    fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
    let store = Store::new(&root);
    let snapshot = store
        .verified_tree_snapshot_with(&digest, |_, _| Ok(false))
        .unwrap();

    store
        .cleanup_stale_replay_snapshots_with(|_| false)
        .unwrap();

    assert!(snapshot.is_dir());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn deleted_snapshot_tree_releases_its_lease_for_parent_recovery() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"{\"name\":\"released\"}").unwrap();
    let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    let destination = root.join("trees").join(digest.as_str());
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::rename(&source, &destination).unwrap();
    fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
    let store = Store::new(&root);
    let snapshot = store.verified_tree_snapshot(&digest).unwrap();
    let reservation = snapshot.parent().unwrap().to_owned();
    fs::remove_dir_all(snapshot.path()).unwrap();

    store.cleanup_stale_replay_snapshots().unwrap();

    assert!(!reservation.exists());
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn snapshot_clone_target_is_inside_an_atomically_reserved_lease_directory() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"{\"name\":\"reserved\"}").unwrap();
    let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    let destination = root.join("trees").join(digest.as_str());
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::rename(&source, &destination).unwrap();
    fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
    let store = Store::new(&root);

    let result = store.verified_tree_snapshot_with(&digest, |_, target| {
        let reservation = target.parent().unwrap();
        assert!(reservation.join(REPLAY_LEASE).is_file());
        assert!(
            reservation
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("replay-tree-")
        );
        Err(io::Error::other("injected after reservation"))
    });

    assert!(result.is_err());
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn failed_snapshot_does_not_delete_a_substituted_reservation() {
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"{\"name\":\"substitution\"}").unwrap();
    let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    let destination = root.join("trees").join(digest.as_str());
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::rename(&source, &destination).unwrap();
    fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
    let store = Store::new(&root);
    let mut substituted = None;

    let result = store.verified_tree_snapshot_with(&digest, |_, target| {
        let reservation = target.parent().unwrap().to_owned();
        fs::remove_dir_all(&reservation)?;
        fs::create_dir(&reservation)?;
        fs::write(reservation.join("competitor"), b"keep")?;
        substituted = Some(reservation);
        Err(io::Error::other("injected after substitution"))
    });

    assert!(result.is_err());
    let substituted = substituted.unwrap();
    assert_eq!(fs::read(substituted.join("competitor")).unwrap(), b"keep");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn replay_snapshot_forced_clone_fallback_copies_verified_bytes() {
    SNAPSHOT_BYTE_COPY_COUNT.set(0);
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"{\"name\":\"fallback\"}").unwrap();
    let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    let destination = root.join("trees").join(digest.as_str());
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::rename(&source, &destination).unwrap();
    fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
    let store = Store::new(&root);

    let snapshot = store
        .verified_tree_snapshot_with(&digest, |_, _| Ok(false))
        .unwrap();

    assert!(SNAPSHOT_BYTE_COPY_COUNT.get() > 0);
    assert_eq!(
        fs::read(snapshot.join("package.json")).unwrap(),
        b"{\"name\":\"fallback\"}"
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn failed_clone_cleanup_removes_a_partial_symlink_without_following_it() {
    use std::os::unix::fs::symlink;

    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"{\"name\":\"cleanup\"}").unwrap();
    let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    let destination = root.join("trees").join(digest.as_str());
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::rename(&source, &destination).unwrap();
    fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep"), b"keep").unwrap();
    let store = Store::new(&root);

    let result = store.verified_tree_snapshot_with(&digest, |_, snapshot| {
        symlink(&outside, snapshot)?;
        Err(io::Error::other("injected clone failure"))
    });

    assert!(result.is_err());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");
    let staging = root.join(".staging");
    assert!(fs::read_dir(staging).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("replay-tree-")
    }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn verified_tree_rejects_non_exact_marker_contents() {
    let root = root();
    let source = root.join("source");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"original").unwrap();
    let expected = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<ArtifactDigest>()
        .unwrap();
    let store = Store::new(&root);
    let destination = store.activate_verified_tree(&expected, &source).unwrap();

    for marker in [
        format!("{}\n", expected.as_str()),
        format!(" {} ", expected.as_str()),
        format!("{}\nextra", expected.as_str()),
    ] {
        fs::write(destination.join(".tapid-tree"), marker).unwrap();
        assert!(matches!(
            store.verified_tree_path(&expected),
            Err(IngestError::Io(error)) if error.kind() == io::ErrorKind::InvalidData
        ));
    }

    fs::write(destination.join(".tapid-tree"), expected.as_str()).unwrap();
    assert!(store.verified_tree_path(&expected).is_ok());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn replay_snapshot_is_verified_and_detached_from_store_mutations() {
    SNAPSHOT_BYTE_COPY_COUNT.set(0);
    SNAPSHOT_CLONE_COUNT.set(0);
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), b"{\"name\":\"fixture\"}").unwrap();
    let tree_digest = tapid_archive::canonical_tree_digest(&source).unwrap();
    let digest = ArtifactDigest::from_str(&tree_digest).unwrap();
    let destination = root.join("trees").join(digest.as_str());
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::rename(&source, &destination).unwrap();
    fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
    let store = Store::new(&root);

    let snapshot = store.verified_tree_snapshot(&digest).unwrap();
    #[cfg(target_os = "macos")]
    {
        assert!(SNAPSHOT_CLONE_COUNT.get() <= 1);
        if SNAPSHOT_CLONE_COUNT.get() == 1 {
            assert_eq!(SNAPSHOT_BYTE_COPY_COUNT.get(), 0);
        } else {
            assert!(SNAPSHOT_BYTE_COPY_COUNT.get() > 0);
        }
    }
    fs::remove_dir_all(&destination).unwrap();
    fs::create_dir_all(&destination).unwrap();
    fs::write(destination.join("package.json"), b"replacement").unwrap();
    fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
    assert_eq!(
        fs::read(snapshot.join("package.json")).unwrap(),
        b"{\"name\":\"fixture\"}"
    );
    assert_eq!(
        tapid_archive::canonical_tree_digest(&snapshot).unwrap(),
        digest.as_str()
    );
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tree_copy_handles_nested_directories_and_preserves_executable_mode() {
    use std::os::unix::fs::PermissionsExt;
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    let target = root.join("target");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::set_permissions(source.join("nested"), fs::Permissions::from_mode(0o750)).unwrap();
    fs::write(source.join("nested").join("data"), b"nested").unwrap();
    let bin = source.join("bin");
    fs::write(&bin, b"#!/bin/sh\n").unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(&target).unwrap();
    copy_tree_contents(&source, &target).unwrap();
    assert_eq!(
        fs::read(target.join("nested").join("data")).unwrap(),
        b"nested"
    );
    assert_eq!(
        fs::metadata(target.join("nested"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        fs::metadata(target.join("bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn reader_failure_does_not_activate_partial_file() {
    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Interrupted, "stop"))
        }
    }
    let root = root();
    let _ = fs::remove_dir_all(&root);
    let store = Store::new(&root);
    let expected = digest(b"x");
    assert!(store.ingest(&expected, Failing).is_err());
    assert!(!store.artifact_path(&expected).exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn publication_snapshot_survives_rollback_without_reentering_store_lock() {
    let project = tapid_test_support::TempProject::new("publication-snapshot").unwrap();
    let source = project.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("package.json"), b"{}").unwrap();
    let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    let store = Store::new(project.path().join("store"));
    let mut transaction = store.transaction();
    transaction.stage_verified_tree(&digest, &source).unwrap();
    let publication = transaction.publish().unwrap();
    let snapshot = publication.verified_tree_snapshot(&digest).unwrap();
    publication.rollback().unwrap();
    assert_eq!(fs::read(snapshot.join("package.json")).unwrap(), b"{}");
    assert!(store.verified_tree_path(&digest).is_err());
    fs::remove_dir_all(snapshot.path()).unwrap();
}

#[test]
fn last_snapshot_drop_preserves_a_replaced_shared_lease() {
    let fixture = tapid_test_support::TempProject::new("shared-lease-identity").unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"snapshot").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    let store = Store::new(fixture.path().join("store"));
    store.activate_verified_tree(&digest, &source).unwrap();
    let snapshot = store.verified_tree_snapshot(&digest).unwrap();
    let lease = fs::read_dir(store.root.join(".staging"))
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("replay-lease-")
        })
        .unwrap()
        .path();
    fs::rename(&lease, fixture.path().join("moved-lease")).unwrap();
    fs::write(&lease, b"replacement").unwrap();
    drop(snapshot);
    assert_eq!(fs::read(lease).unwrap(), b"replacement");
}

#[test]
#[cfg(unix)]
fn snapshot_drop_does_not_accumulate_file_descriptors() {
    const ROLE: &str = "TAPID_TEST_REPLAY_LEASE_FDS";
    if std::env::var_os(ROLE).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::snapshot_drop_does_not_accumulate_file_descriptors",
            ])
            .env(ROLE, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        return;
    }
    let fixture = tapid_test_support::TempProject::new("snapshot-descriptor-count").unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"snapshot").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    #[cfg(target_os = "macos")]
    let descriptor_directory = "/dev/fd";
    #[cfg(not(target_os = "macos"))]
    let descriptor_directory = "/proc/self/fd";
    let count = || fs::read_dir(descriptor_directory).unwrap().count();
    let before = count();
    for index in 0..16 {
        let store = Store::new(fixture.path().join(format!("store-{index}")));
        store.activate_verified_tree(&digest, &source).unwrap();
        drop(store.verified_tree_snapshot(&digest).unwrap());
    }
    assert_eq!(
        count(),
        before,
        "snapshot leases must release their descriptors"
    );
}

#[test]
fn last_snapshot_drop_releases_shared_lease_and_allows_reuse() {
    let fixture = tapid_test_support::TempProject::new("snapshot-lease-cleanup").unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"snapshot").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    for index in 0..8 {
        let store = Store::new(fixture.path().join(format!("store-{index}")));
        store.activate_verified_tree(&digest, &source).unwrap();
        for _ in 0..2 {
            let first = store.verified_tree_snapshot(&digest).unwrap();
            let second = store.verified_tree_snapshot(&digest).unwrap();
            drop(first);
            store.cleanup_stale_replay_snapshots().unwrap();
            assert_eq!(fs::read(second.join("file")).unwrap(), b"snapshot");
            drop(second);
            assert_eq!(
                fs::read_dir(store.root.join(".staging")).unwrap().count(),
                0,
                "the last snapshot must release its shared lease file"
            );
        }
    }
}

#[test]
fn owned_snapshot_releases_reservation_on_drop() {
    let fixture = tapid_test_support::TempProject::new("owned-snapshot").unwrap();
    let store = Store::new(fixture.path().join("store"));
    let source = fixture.path().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("file"), b"snapshot").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    store.activate_verified_tree(&digest, &source).unwrap();
    let snapshot = store.verified_tree_snapshot(&digest).unwrap();
    let reservation = snapshot.parent().unwrap().to_owned();
    assert!(reservation.is_dir());
    drop(snapshot);
    assert!(!reservation.exists());
}

#[test]
fn snapshot_drop_preserves_a_replaced_reservation() {
    let project = tapid_test_support::TempProject::new("snapshot-identity").unwrap();
    let store = Store::new(project.path().join("store"));
    let source = project.path().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("file"), b"snapshot").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse()
        .unwrap();
    store.activate_verified_tree(&digest, &source).unwrap();
    let snapshot = store.verified_tree_snapshot(&digest).unwrap();
    let reservation = snapshot.parent().unwrap().to_owned();
    fs::rename(&reservation, project.path().join("moved")).unwrap();
    fs::create_dir(&reservation).unwrap();
    fs::write(reservation.join("replacement"), b"keep").unwrap();
    drop(snapshot);
    assert_eq!(fs::read(reservation.join("replacement")).unwrap(), b"keep");
}
