#![cfg(any(unix, windows))]
use tapid_security_provider::ReplayStore;

fn request() -> tapid_protocol::SecurityRequest {
    serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(include_bytes!("fixtures/advisory-v2.json"))
            .unwrap()["claims"]["request"]
            .clone(),
    )
    .unwrap()
}

#[test]
fn replay_floors_survive_reopening_and_reject_rollback_and_equivocation() {
    let home = tapid_test_support::TempHome::new("security-replay").unwrap();
    let root = home.path().join("floors");
    let store = ReplayStore::open(&root).unwrap();
    let request = request();
    assert_eq!(store.load("issuer", &request).unwrap(), None);
    store
        .record("issuer", &request, 3, &format!("sha256-{}", "a".repeat(64)))
        .unwrap();
    drop(store);
    let store = ReplayStore::open(&root).unwrap();
    assert_eq!(
        store.load("issuer", &request).unwrap(),
        Some((3, format!("sha256-{}", "a".repeat(64))))
    );
    assert!(
        store
            .record("issuer", &request, 2, &format!("sha256-{}", "b".repeat(64)))
            .is_err()
    );
    assert!(
        store
            .record("issuer", &request, 3, &format!("sha256-{}", "b".repeat(64)))
            .is_err()
    );
    store
        .record("issuer", &request, 3, &format!("sha256-{}", "a".repeat(64)))
        .unwrap();
    assert_eq!(store.load("other-issuer", &request).unwrap(), None);
}

#[test]
fn concurrent_writers_cannot_lower_the_committed_floor() {
    let home = tapid_test_support::TempHome::new("security-replay-race").unwrap();
    let root = home.path().join("floors");
    ReplayStore::open(&root).unwrap();
    std::thread::scope(|scope| {
        for sequence in 1..=16 {
            let root = &root;
            scope.spawn(move || {
                let store = ReplayStore::open(root).unwrap();
                // Lower writers may lose the race, but the highest must commit.
                let result = store.record(
                    "issuer",
                    &request(),
                    sequence,
                    &format!("sha256-{sequence:064x}"),
                );
                if sequence == 16 {
                    result.unwrap();
                }
            });
        }
    });
    let store = ReplayStore::open(&root).unwrap();
    assert_eq!(
        store.load("issuer", &request()).unwrap(),
        Some((16, format!("sha256-{:064x}", 16)))
    );
}

#[test]
fn interrupted_staging_preserves_old_floor_and_retry_is_durable() {
    let home = tapid_test_support::TempHome::new("security-replay-interrupted").unwrap();
    let root = home.path().join("floors");
    let store = ReplayStore::open(&root).unwrap();
    let digest = format!("sha256-{}", "a".repeat(64));
    store.record("issuer", &request(), 3, &digest).unwrap();
    std::fs::write(root.join(".pending"), b"4\npartial").unwrap();
    assert_eq!(
        store.load("issuer", &request()).unwrap(),
        Some((3, digest.clone()))
    );
    store.record("issuer", &request(), 4, &digest).unwrap();
    assert!(!root.join(".pending").exists());
    assert_eq!(
        ReplayStore::open(&root)
            .unwrap()
            .load("issuer", &request())
            .unwrap(),
        Some((4, digest))
    );
}

#[cfg(unix)]
#[test]
fn replay_state_rejects_public_permissions_symlinks_and_corrupt_floors() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let home = tapid_test_support::TempHome::new("security-replay-hostile").unwrap();
    let root = home.path().join("floors");
    let store = ReplayStore::open(&root).unwrap();
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    store
        .record(
            "issuer",
            &request(),
            3,
            &format!("sha256-{}", "a".repeat(64)),
        )
        .unwrap();
    let floor = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "floor"))
        .unwrap();
    assert_eq!(
        std::fs::metadata(&floor).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&floor, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load("issuer", &request()).is_err());
    std::fs::set_permissions(&floor, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&floor, b"truncated").unwrap();
    assert!(store.load("issuer", &request()).is_err());
    assert!(
        store
            .record(
                "issuer",
                &request(),
                4,
                &format!("sha256-{}", "a".repeat(64))
            )
            .is_err()
    );
    std::fs::remove_file(&floor).unwrap();
    let target = home.write("unrelated", b"secret").unwrap();
    symlink(&target, &floor).unwrap();
    assert!(store.load("issuer", &request()).is_err());
    assert!(
        store
            .record(
                "issuer",
                &request(),
                4,
                &format!("sha256-{}", "a".repeat(64))
            )
            .is_err()
    );
    assert_eq!(std::fs::read(target).unwrap(), b"secret");
    std::fs::remove_file(&floor).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ReplayStore::open(&root).is_err());
    assert!(store.load("issuer", &request()).is_err());
}

#[test]
fn request_contexts_are_separate_and_linked_floors_fail_closed() {
    let home = tapid_test_support::TempHome::new("security-replay-context").unwrap();
    let root = home.path().join("floors");
    let store = ReplayStore::open(&root).unwrap();
    let request = request();
    let digest = format!("sha256-{}", "a".repeat(64));
    store.record("issuer", &request, 3, &digest).unwrap();
    let mut alias = request.clone();
    alias.subject.requested_name = "another-alias".into();
    assert_eq!(store.load("issuer", &alias).unwrap(), None);
    store.record("issuer", &alias, 1, &digest).unwrap();
    assert_eq!(store.load("issuer", &request).unwrap(), Some((3, digest)));
    let floor = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "floor"))
        .unwrap();
    std::fs::hard_link(&floor, home.path().join("linked-floor")).unwrap();
    // Exactly one context was linked. Neither may silently replace it with a
    // reset baseline or unlink another user's file.
    let outcomes = [store.load("issuer", &request), store.load("issuer", &alias)];
    assert_eq!(outcomes.iter().filter(|r| r.is_err()).count(), 1);
}

#[cfg(unix)]
#[test]
fn replaced_root_and_symlinked_root_are_rejected() {
    use std::os::unix::fs::{DirBuilderExt, symlink};
    let home = tapid_test_support::TempHome::new("security-replay-root").unwrap();
    let root = home.path().join("floors");
    let store = ReplayStore::open(&root).unwrap();
    std::fs::rename(&root, home.path().join("saved")).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    assert!(store.load("issuer", &request()).is_err());
    std::fs::remove_dir(&root).unwrap();
    symlink(home.path().join("saved"), &root).unwrap();
    assert!(ReplayStore::open(&root).is_err());
}

#[test]
fn replay_process_worker() {
    let Some(root) = std::env::var_os("TAPID_REPLAY_TEST_ROOT") else {
        return;
    };
    let sequence: u64 = std::env::var("TAPID_REPLAY_TEST_SEQUENCE")
        .unwrap()
        .parse()
        .unwrap();
    let store = ReplayStore::open(std::path::Path::new(&root)).unwrap();
    let result = store.record(
        "issuer",
        &request(),
        sequence,
        &format!("sha256-{sequence:064x}"),
    );
    if sequence == 8 {
        result.unwrap();
    }
}

#[test]
fn separate_process_writers_cannot_lower_the_committed_floor() {
    let home = tapid_test_support::TempHome::new("security-replay-processes").unwrap();
    let root = home.path().join("floors");
    ReplayStore::open(&root).unwrap();
    let mut children = (1..=8)
        .map(|sequence| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "replay_process_worker"])
                .env("TAPID_REPLAY_TEST_ROOT", &root)
                .env("TAPID_REPLAY_TEST_SEQUENCE", sequence.to_string())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        ReplayStore::open(&root)
            .unwrap()
            .load("issuer", &request())
            .unwrap(),
        Some((8, format!("sha256-{:064x}", 8)))
    );
}

#[cfg(windows)]
#[path = "replay/windows.rs"]
mod windows;
