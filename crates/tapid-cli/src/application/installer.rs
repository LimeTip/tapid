//! Installer rollback policy, called only by the checksum-pinned bootstrap
//! after the script has verified the release record and archive.
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, process::Command};
use tapid_release_client::{ReleaseState, accept_release, read_release_state, write_release_state};

pub(crate) fn prepare(
    version: &str,
    minimum: &str,
    archive: &Path,
    destination: &Path,
) -> Result<(), String> {
    // Use the release client's strict stable-version parser and comparison.
    let baseline =
        ReleaseState::new(minimum, 0, "0".repeat(64)).map_err(|error| error.to_string())?;
    accept_release(&baseline, version, 1, "0".repeat(64))
        .map_err(|error| format!("installer bootstrap floor: {error}"))?;
    let mut bytes = Vec::new();
    fs::File::open(archive)
        .map_err(|error| error.to_string())?
        .take((crate::filesystem::atomic::MAX_ARTIFACT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > crate::filesystem::atomic::MAX_ARTIFACT_BYTES {
        return Err("release archive exceeds the size limit".into());
    }
    let executable = crate::filesystem::atomic::materialize_artifact("release.tar.gz", &bytes)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            crate::filesystem::atomic::validate_upgrade_destination(destination)?;
            let output = Command::new(destination)
                .arg("--version")
                .output()
                .map_err(|error| format!("cannot read installed Tapid version: {error}"))?;
            let text = std::str::from_utf8(&output.stdout)
                .map_err(|_| "installed Tapid version is not UTF-8")?;
            let installed = text
                .trim()
                .strip_prefix("tapid ")
                .ok_or("cannot determine installed Tapid version")?;
            if !output.status.success() {
                return Err("cannot determine installed Tapid version".into());
            }
            let installed_state = ReleaseState::new(installed, 0, "0".repeat(64))
                .map_err(|error| error.to_string())?;
            accept_release(&installed_state, version, 1, "0".repeat(64))
                .map_err(|error| format!("installed version floor: {error}"))?;
            if version == installed
                && fs::read(destination).map_err(|error| error.to_string())? != executable
            {
                return Err("same release version has a different executable digest".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let state_path = destination
        .parent()
        .ok_or("install destination has no parent")?
        .join(".tapid-release-state.json");
    let mut next = match fs::symlink_metadata(&state_path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err("release state must be a regular file".into());
            }
            let previous = read_release_state(&state_path).map_err(|error| error.to_string())?;
            accept_release(
                &previous,
                version,
                previous.release_sequence.saturating_add(1),
                digest.clone(),
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ReleaseState::new(version, 1, digest.clone())
        }
        Err(error) => return Err(error.to_string()),
    }
    .map_err(|error| error.to_string())?;
    next.verification = "signature".into();
    // Persist the verified recovery archive and floor BEFORE the scripts replace
    // the executable. An interruption retains the floor and can retry/recover
    // this exact release; it never leaves new bytes with an older floor.
    super::upgrade::write_cached_artifact(destination, &digest, &bytes)?;
    write_release_state(&state_path, &next).map_err(|error| error.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    use tapid_release_client::{ReleaseState, read_release_state, write_release_state};
    use tapid_test_support::TempProject;

    fn fixture() -> (TempProject, std::path::PathBuf, std::path::PathBuf) {
        let root = TempProject::new("installer-release-policy").unwrap();
        let destination = root
            .write("tapid", b"#!/bin/sh\nprintf 'tapid 1.2.3\\n'\n")
            .unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755)).unwrap();
        root.write(".tapid-managed", b"tapid-managed-v1\n").unwrap();
        root.write("payload/tapid", b"new executable").unwrap();
        let archive = root.path().join("release.tar.gz");
        assert!(
            std::process::Command::new("tar")
                .env("COPYFILE_DISABLE", "1")
                .args(["-czf"])
                .arg(&archive)
                .arg("-C")
                .arg(root.path().join("payload"))
                .arg("tapid")
                .status()
                .unwrap()
                .success()
        );
        (root, archive, destination)
    }

    #[test]
    fn installer_rejects_rollback_without_prior_state() {
        let (root, archive, destination) = fixture();
        let before = fs::read(&destination).unwrap();
        assert!(
            prepare("1.2.2", "1.0.0", &archive, &destination)
                .unwrap_err()
                .contains("ReleaseDowngrade")
        );
        assert_eq!(fs::read(destination).unwrap(), before);
        assert!(!root.path().join(".tapid-release-state.json").exists());
    }

    #[test]
    fn installer_rejects_changed_bytes_at_same_installed_version() {
        let (_root, archive, destination) = fixture();
        assert!(
            prepare("1.2.3", "1.0.0", &archive, &destination)
                .unwrap_err()
                .contains("same release version")
        );
    }

    #[test]
    fn installer_honors_durable_floor_and_digest() {
        let (root, archive, destination) = fixture();
        let path = root.path().join(".tapid-release-state.json");
        let state = ReleaseState::new("1.2.4", 10, "a".repeat(64)).unwrap();
        write_release_state(&path, &state).unwrap();
        assert!(prepare("1.2.3", "1.0.0", &archive, &destination).is_err());
        assert!(prepare("1.2.4", "1.0.0", &archive, &destination).is_err());
        assert_eq!(read_release_state(&path).unwrap(), state);
    }

    #[test]
    fn installer_fresh_install_enforces_bootstrap_floor_and_persists_recovery() {
        let (root, archive, destination) = fixture();
        fs::remove_file(&destination).unwrap();
        assert!(prepare("1.2.2", "1.2.3", &archive, &destination).is_err());
        prepare("1.2.4", "1.2.3", &archive, &destination).unwrap();
        let state = read_release_state(&root.path().join(".tapid-release-state.json")).unwrap();
        assert_eq!(state.release_floor, "1.2.4");
        assert_eq!(state.verification, "signature");
        let cached = root.path().join(format!(
            ".tapid-release-artifact-{}",
            state.last_known_good.artifact_sha256
        ));
        assert_eq!(fs::read(cached).unwrap(), fs::read(archive).unwrap());
        prepare(
            "1.2.4",
            "1.2.3",
            &root.path().join("release.tar.gz"),
            &destination,
        )
        .unwrap();
    }
    #[test]
    fn installer_accepts_identical_reinstall_and_rejects_invalid_state() {
        let (root, archive, destination) = fixture();
        fs::copy(&destination, root.path().join("payload/tapid")).unwrap();
        assert!(
            Command::new("tar")
                .env("COPYFILE_DISABLE", "1")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(root.path().join("payload"))
                .arg("tapid")
                .status()
                .unwrap()
                .success()
        );
        prepare("1.2.3", "1.0.0", &archive, &destination).unwrap();
        prepare("1.2.3", "1.0.0", &archive, &destination).unwrap();
        let state_path = root.path().join(".tapid-release-state.json");
        fs::write(&state_path, b"invalid state").unwrap();
        assert!(prepare("1.2.4", "1.0.0", &archive, &destination).is_err());
        assert_eq!(fs::read(&state_path).unwrap(), b"invalid state");
    }

    #[test]
    fn installer_refuses_unmarked_destination_and_symlinked_state() {
        let (root, archive, destination) = fixture();
        fs::remove_file(root.path().join(".tapid-managed")).unwrap();
        assert!(prepare("1.2.4", "1.0.0", &archive, &destination).is_err());
        root.write(".tapid-managed", b"tapid-managed-v1\n").unwrap();
        let outside = TempProject::new("installer-state-target").unwrap();
        let target = outside.write("state.json", b"outside state").unwrap();
        std::os::unix::fs::symlink(&target, root.path().join(".tapid-release-state.json")).unwrap();
        assert!(prepare("1.2.4", "1.0.0", &archive, &destination).is_err());
        assert_eq!(fs::read(target).unwrap(), b"outside state");
    }
}
