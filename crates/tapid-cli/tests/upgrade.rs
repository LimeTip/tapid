#![cfg(unix)]

use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "tapid-upgrade-cli-{}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir(root.join("release")).unwrap();
        fs::write(root.join("tapid"), b"old executable").unwrap();
        fs::write(root.join(".tapid-managed"), b"tapid-managed-v1\n").unwrap();
        fs::write(root.join("release/tapid"), b"new executable").unwrap();
        assert!(
            Command::new("tar")
                .env("COPYFILE_DISABLE", "1")
                .args(["-czf"])
                .arg(root.join("artifact.tar.gz"))
                .arg("-C")
                .arg(root.join("release"))
                .arg("tapid")
                .status()
                .unwrap()
                .success()
        );
        let bytes = fs::read(root.join("artifact.tar.gz")).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let targets = [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu",
        ];
        let mut record = String::from("tapid-release-v1\t0.0.11\n");
        for target in targets {
            record.push_str(&format!("{target}\ttapid-0.0.11-{target}.tar.gz\t{}\t{digest}\thttps://downloads.example.test/artifact\n", bytes.len()));
        }
        fs::write(root.join("release.tsv"), record).unwrap();
        fs::write(
            root.join("bin/curl"),
            r#"#!/bin/sh
for url do :; done
printf '%s\n' "$url" >> "$FIXTURE/requests"
case "$url" in
  https://tapid.dev/releases/v1/latest.tsv|https://custom.test/latest.tsv) /bin/cat "$FIXTURE/release.tsv" ;;
  */artifact) /bin/cat "$FIXTURE/artifact.tar.gz" ;;
  *) echo "unexpected URL" >&2; exit 99 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(root.join("bin/curl"), fs::Permissions::from_mode(0o755)).unwrap();
        Self { root }
    }

    fn upgrade(&self, extra: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(["upgrade", "--destination"])
            .arg(self.root.join("tapid"))
            .args(extra)
            .env_remove("TAPID_STABLE_ENDPOINTS")
            .env_remove("TAPID_RELEASE_KEYRING")
            // The fixture exercises the record parser explicitly; production
            // upgrades use signed discovery by default.
            .env(
                "TAPID_RELEASE_RECORD_URL",
                "https://tapid.dev/releases/v1/latest.tsv",
            )
            .env("PATH", self.root.join("bin"))
            .env("FIXTURE", &self.root)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn repeated_upgrade_reports_already_current_without_replacing_files() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let first = fixture.upgrade(&[]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains("Upgraded Tapid to 0.0.11"));
    let inode = fs::metadata(fixture.root.join("tapid")).unwrap().ino();
    let state = fs::read(fixture.root.join(".tapid-release-state.json")).unwrap();
    let second = fixture.upgrade(&[]);
    assert!(second.status.success());
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("Tapid 0.0.11 is already up to date"),
        "{stdout}"
    );
    assert!(!stdout.contains("Upgraded"), "{stdout}");
    assert_eq!(
        fs::metadata(fixture.root.join("tapid")).unwrap().ino(),
        inode
    );
    assert_eq!(
        fs::read(fixture.root.join(".tapid-release-state.json")).unwrap(),
        state
    );
}

#[test]
fn explicit_record_discovery_is_not_the_signed_default() {
    let fixture = Fixture::new();
    let output = fixture.upgrade(&[]);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("curl:"), "{stderr}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("independent signature verification was not performed")
    );
}

#[test]
fn already_installed_release_initializes_recovery_without_replacing_executable() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tapid"), b"new executable").unwrap();
    let inode = fs::metadata(fixture.root.join("tapid")).unwrap().ino();
    let output = fixture.upgrade(&[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("already up to date"));
    assert_eq!(
        fs::metadata(fixture.root.join("tapid")).unwrap().ino(),
        inode
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.root.join(".tapid-release-state.json")).unwrap())
            .unwrap();
    assert_eq!(state["verification"], "checksum");
}

#[test]
fn dry_run_leaves_executable_and_recovery_state_untouched() {
    let fixture = Fixture::new();
    let output = fixture.upgrade(&["--dry-run"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("dry-run did not replace"));
    assert_eq!(
        fs::read(fixture.root.join("tapid")).unwrap(),
        b"old executable"
    );
    assert!(!fixture.root.join(".tapid-release-state.json").exists());
}

#[test]
fn cached_recovery_does_not_claim_latest_or_upgrade() {
    let fixture = Fixture::new();
    assert!(fixture.upgrade(&[]).status.success());
    fs::remove_file(fixture.root.join("release.tsv")).unwrap();
    let unchanged = fixture.upgrade(&[]);
    assert!(unchanged.status.success());
    let stdout = String::from_utf8_lossy(&unchanged.stdout);
    assert!(
        stdout.contains("could not check the latest stable release"),
        "{stdout}"
    );
    assert!(
        stdout.contains("already matches the cached release"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("up to date") && !stdout.contains("Upgraded"),
        "{stdout}"
    );

    fs::write(fixture.root.join("tapid"), b"damaged executable").unwrap();
    let restored = fixture.upgrade(&[]);
    assert!(restored.status.success());
    let stdout = String::from_utf8_lossy(&restored.stdout);
    assert!(
        stdout.contains("Restored Tapid 0.0.11 from the cached release"),
        "{stdout}"
    );
    assert!(!stdout.contains("Upgraded"), "{stdout}");
    assert_eq!(
        fs::read(fixture.root.join("tapid")).unwrap(),
        b"new executable"
    );
}

#[test]
fn unavailable_release_without_cache_reports_failure() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("release.tsv")).unwrap();
    let output = fixture.upgrade(&[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("release discovery unavailable"));
    assert_eq!(
        fs::read(fixture.root.join("tapid")).unwrap(),
        b"old executable"
    );
}

#[test]
fn identical_executable_still_requires_valid_release_checksum() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tapid"), b"new executable").unwrap();
    let record = fs::read_to_string(fixture.root.join("release.tsv")).unwrap();
    let digest = record.lines().nth(1).unwrap().split('\t').nth(3).unwrap();
    fs::write(
        fixture.root.join("release.tsv"),
        record.replace(digest, &"0".repeat(64)),
    )
    .unwrap();
    let output = fixture.upgrade(&[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ArtifactDigestMismatch"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("up to date"));
    assert!(!fixture.root.join(".tapid-release-state.json").exists());
}

#[test]
fn provider_migration_and_major_version_upgrade_need_no_client_changes() {
    let fixture = Fixture::new();
    assert!(fixture.upgrade(&[]).status.success());
    let record = fs::read_to_string(fixture.root.join("release.tsv")).unwrap();
    fs::write(
        fixture.root.join("release.tsv"),
        record
            .replace("0.0.11", "1.0.0")
            .replace("downloads.example.test", "gitlab.example.org"),
    )
    .unwrap();
    fs::write(fixture.root.join("tapid"), b"previous version executable").unwrap();
    let output = fixture.upgrade(&[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Upgraded Tapid to 1.0.0"));
    let requests = fs::read_to_string(fixture.root.join("requests")).unwrap();
    assert!(requests.contains("https://gitlab.example.org/artifact"));
    assert!(!requests.contains("api.github.com") && !requests.contains("stable.json"));
}

#[test]
fn explicit_release_record_url_overrides_default_discovery() {
    let fixture = Fixture::new();
    let output = fixture.upgrade(&["--release-url", "https://custom.test/latest.tsv"]);
    assert!(output.status.success());
    let requests = fs::read_to_string(fixture.root.join("requests")).unwrap();
    assert!(requests.starts_with("https://custom.test/latest.tsv\n"));
    assert!(!requests.contains("tapid.dev"));
}

#[test]
fn rejected_record_or_artifact_never_uses_existing_recovery_cache() {
    let fixture = Fixture::new();
    assert!(fixture.upgrade(&[]).status.success());
    let record = fs::read(fixture.root.join("release.tsv")).unwrap();
    let executable = fs::read(fixture.root.join("tapid")).unwrap();
    let state = fs::read(fixture.root.join(".tapid-release-state.json")).unwrap();
    fs::write(fixture.root.join("release.tsv"), b"invalid record\n").unwrap();
    assert!(!fixture.upgrade(&[]).status.success());
    fs::write(fixture.root.join("release.tsv"), record).unwrap();
    fs::write(fixture.root.join("artifact.tar.gz"), b"corrupted").unwrap();
    assert!(!fixture.upgrade(&[]).status.success());
    assert_eq!(fs::read(fixture.root.join("tapid")).unwrap(), executable);
    assert_eq!(
        fs::read(fixture.root.join(".tapid-release-state.json")).unwrap(),
        state
    );
}

#[test]
fn earlier_release_is_rejected_after_successful_upgrade() {
    let fixture = Fixture::new();
    assert!(fixture.upgrade(&[]).status.success());
    let record = fs::read_to_string(fixture.root.join("release.tsv")).unwrap();
    fs::write(
        fixture.root.join("release.tsv"),
        record.replace("0.0.11", "0.0.10"),
    )
    .unwrap();
    let output = fixture.upgrade(&[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ReleaseDowngrade"));
}

#[test]
fn matching_new_install_refreshes_old_recovery_state() {
    let fixture = Fixture::new();
    assert!(fixture.upgrade(&[]).status.success());
    let record = fs::read_to_string(fixture.root.join("release.tsv")).unwrap();
    fs::write(
        fixture.root.join("release.tsv"),
        record.replace("0.0.11", "1.0.0"),
    )
    .unwrap();
    let output = fixture.upgrade(&[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.root.join(".tapid-release-state.json")).unwrap())
            .unwrap();
    assert_eq!(state["release_floor"], "1.0.0");
    fs::remove_file(fixture.root.join("release.tsv")).unwrap();
    let cached = fixture.upgrade(&[]);
    assert!(cached.status.success());
    assert!(
        String::from_utf8_lossy(&cached.stdout)
            .contains("Tapid 1.0.0 already matches the cached release")
    );
}

#[test]
fn fresh_self_upgrade_rejects_an_older_release_without_state() {
    let fixture = Fixture::new();
    let install_dir = fixture.root.join("fresh-self");
    fs::create_dir(&install_dir).unwrap();
    let executable = install_dir.join("tapid");
    fs::copy(env!("CARGO_BIN_EXE_tapid"), &executable).unwrap();
    fs::write(install_dir.join(".tapid-managed"), b"tapid-managed-v1\n").unwrap();
    let record = fs::read_to_string(fixture.root.join("release.tsv")).unwrap();
    fs::write(
        fixture.root.join("release.tsv"),
        record.replace("0.0.11", "0.0.10"),
    )
    .unwrap();
    let output = Command::new(&executable)
        .arg("upgrade")
        .env(
            "TAPID_RELEASE_RECORD_URL",
            "https://tapid.dev/releases/v1/latest.tsv",
        )
        .env_remove("TAPID_STABLE_ENDPOINTS")
        .env_remove("TAPID_RELEASE_KEYRING")
        .env("PATH", fixture.root.join("bin"))
        .env("FIXTURE", &fixture.root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("refusing to downgrade running Tapid")
    );
    assert!(!install_dir.join(".tapid-release-state.json").exists());
}
