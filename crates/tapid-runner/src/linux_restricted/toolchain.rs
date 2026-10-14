use super::*;
use sha2::{Digest, Sha256};
use std::os::unix::{ffi::OsStrExt, fs::FileTypeExt};
use std::{collections::BTreeSet, path::PathBuf};

pub(super) fn identity(runtime: &[PathBuf]) -> Result<String, ExecutionError> {
    let mut hash = Sha256::new();
    frame(&mut hash, b"tapid-linux-managed-system-toolchain-v1");
    let mut kernel: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut kernel) } != 0 {
        return Err(unsupported("cannot identify execution kernel"));
    }
    for value in [&kernel.release, &kernel.version, &kernel.machine] {
        frame(
            &mut hash,
            unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) }.to_bytes(),
        );
    }
    let executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| unsupported(&format!("cannot identify Tapid executable: {e}")))?;
    let mut paths = runtime.to_vec();
    paths.push(executable);
    paths.sort();
    paths.dedup();
    let mut visited = BTreeSet::new();
    let mut budget = Budget {
        entries: 0,
        bytes: 0,
    };
    for path in &paths {
        hash_path(path, &paths, &mut visited, &mut budget, &mut hash)?;
    }
    Ok(format!("sha256-{:x}", hash.finalize()))
}
struct Budget {
    entries: usize,
    bytes: u64,
}
fn frame(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}
fn frame_unreadable(hash: &mut Sha256, metadata: &fs::Metadata) {
    frame(hash, b"permission-denied");
    hash.update(metadata.mode().to_le_bytes());
    hash.update(metadata.uid().to_le_bytes());
    hash.update(metadata.gid().to_le_bytes());
    hash.update(metadata.len().to_le_bytes());
    hash.update(metadata.mtime().to_le_bytes());
    hash.update(metadata.mtime_nsec().to_le_bytes());
    hash.update(metadata.ctime().to_le_bytes());
    hash.update(metadata.ctime_nsec().to_le_bytes());
}
fn hash_path(
    path: &Path,
    roots: &[PathBuf],
    visited: &mut BTreeSet<PathBuf>,
    budget: &mut Budget,
    hash: &mut Sha256,
) -> Result<(), ExecutionError> {
    budget.entries += 1;
    if budget.entries > 500_000 {
        return Err(unsupported("system toolchain exceeds 500,000 entries"));
    }
    frame(hash, path.as_os_str().as_bytes());
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| unsupported(&format!("cannot inspect system toolchain: {e}")))?;
    if metadata.file_type().is_symlink() {
        frame(hash, b"symlink");
        frame(
            hash,
            fs::read_link(path)
                .map_err(|e| unsupported(&format!("cannot read toolchain link: {e}")))?
                .as_os_str()
                .as_bytes(),
        );
        let canonical = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Missing optional system-link targets grant no readable bytes.
                // Record their absence so installing the target changes identity.
                frame(hash, b"missing-link-target");
                return Ok(());
            }
            Err(error) => {
                return Err(unsupported(&format!(
                    "cannot resolve toolchain link: {error}"
                )));
            }
        };
        if roots.iter().any(|root| canonical.starts_with(root)) && !visited.contains(&canonical) {
            hash_path(&canonical, roots, visited, budget, hash)?;
        }
        return Ok(());
    }
    if !visited.insert(path.to_path_buf()) {
        frame(hash, b"already-framed");
        return Ok(());
    }
    if metadata.is_dir() {
        frame(hash, b"directory");
        // Reading names alone does not establish search permission for children.
        match fs::metadata(path.join(".")) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                frame_unreadable(hash, &metadata);
                return Ok(());
            }
            Err(error) => {
                return Err(unsupported(&format!(
                    "cannot list system toolchain: {error}"
                )));
            }
        }
        // A searchable directory can expose known children even when listing
        // is denied. Refuse to omit those potentially readable contents.
        let mut children = fs::read_dir(path)
            .map_err(|error| unsupported(&format!("cannot list system toolchain: {error}")))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()
            .map_err(|e| unsupported(&format!("cannot list system toolchain: {e}")))?;
        children.sort();
        for child in children {
            hash_path(&child, roots, visited, budget, hash)?;
        }
    } else if metadata.is_file() {
        frame(hash, b"file");
        hash.update((metadata.mode() & 0o111).to_le_bytes());
        hash.update(metadata.len().to_le_bytes());
        budget.bytes = budget
            .bytes
            .checked_add(metadata.len())
            .ok_or_else(|| unsupported("system toolchain size overflow"))?;
        if budget.bytes > 32 * 1024 * 1024 * 1024 {
            return Err(unsupported("system toolchain exceeds 32 GiB"));
        }
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                // The hook retains this UID and cannot read these bytes either.
                frame_unreadable(hash, &metadata);
                return Ok(());
            }
            Err(error) => {
                return Err(unsupported(&format!(
                    "cannot read system toolchain: {error}"
                )));
            }
        };
        let mut remaining = metadata.len();
        let mut buffer = [0u8; 65536];
        while remaining > 0 {
            let count = file
                .read(&mut buffer[..remaining.min(65536) as usize])
                .map_err(|e| unsupported(&format!("cannot hash system toolchain: {e}")))?;
            if count == 0 {
                return Err(unsupported("system toolchain changed while hashing"));
            }
            hash.update(&buffer[..count]);
            remaining -= count as u64;
        }
        if file
            .read(&mut buffer[..1])
            .map_err(|e| unsupported(&e.to_string()))?
            != 0
            || file
                .metadata()
                .map_err(|e| unsupported(&e.to_string()))?
                .len()
                != metadata.len()
        {
            return Err(unsupported("system toolchain changed while hashing"));
        }
    } else if metadata.file_type().is_char_device() {
        frame(hash, b"device");
        hash.update(metadata.rdev().to_le_bytes());
    } else {
        return Err(unsupported("unsupported system toolchain file type"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    fn fingerprint(root: &Path) -> Result<String, ExecutionError> {
        let mut hash = Sha256::new();
        hash_path(
            root,
            &[root.to_owned()],
            &mut BTreeSet::new(),
            &mut Budget {
                entries: 0,
                bytes: 0,
            },
            &mut hash,
        )?;
        Ok(format!("sha256-{:x}", hash.finalize()))
    }

    #[test]
    fn toolchain_identity_records_unreadable_entries_and_permission_changes() {
        const CHILD_ROOT: &str = "TAPID_TEST_UNREADABLE_TOOLCHAIN_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            if std::env::var_os("TAPID_TEST_UNLISTABLE_TOOLCHAIN").is_some() {
                let root = Path::new(&root);
                assert_eq!(fs::read(root.join("known-child")).unwrap(), b"accessible");
                assert!(
                    fingerprint(root).is_err(),
                    "searchable directories expose known children even when listing is denied"
                );
                return;
            }
            println!(
                "toolchain-test-identity:{}",
                fingerprint(Path::new(&root)).unwrap()
            );
            return;
        }
        let project = tapid_test_support::TempProject::new("toolchain-unreadable").unwrap();
        fs::set_permissions(project.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let file = project
            .write("private-file", b"not readable by the hook")
            .unwrap();
        let directory = project.path().join("private-directory");
        project
            .write("private-directory/hidden", b"hidden contents")
            .unwrap();
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            }
        }
        let _restore = Restore(directory.clone());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o000)).unwrap();
        let identity = || {
            if unsafe { libc::geteuid() } != 0 {
                assert_eq!(
                    File::open(&file).unwrap_err().kind(),
                    io::ErrorKind::PermissionDenied
                );
                return fingerprint(project.path()).unwrap();
            }
            // Root bypasses DAC. A fresh unprivileged process exercises actual
            // permission-denied opens without changing the test runner's UID.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "execution::platform_backend::toolchain::tests::toolchain_identity_records_unreadable_entries_and_permission_changes", "--nocapture"])
                .env(CHILD_ROOT, project.path())
                .uid(65534).gid(65534)
                .output().unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.strip_prefix("toolchain-test-identity:")
                        .map(str::to_owned)
                })
                .expect("unprivileged fingerprint helper must run")
        };
        let denied = identity();
        assert_eq!(denied, identity());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o444)).unwrap();
        let no_search = identity();
        assert_ne!(denied, no_search);
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        let readable_directory = identity();
        assert_ne!(no_search, readable_directory);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        // The closure's DAC assertion no longer applies once this file is readable.
        let readable = fingerprint(project.path()).unwrap();
        assert_ne!(readable_directory, readable);
        project
            .write("private-directory/hidden", b"changed readable contents")
            .unwrap();
        assert_ne!(readable, fingerprint(project.path()).unwrap());
    }

    #[test]
    fn toolchain_identity_keeps_other_inspection_errors_fatal() {
        let project = tapid_test_support::TempProject::new("toolchain-errors").unwrap();
        assert!(fingerprint(&project.path().join("missing")).is_err());
        let path =
            std::ffi::CString::new(project.path().join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(fingerprint(project.path()).is_err());
        fs::remove_file(project.path().join("fifo")).unwrap();
        project.write("known-child", b"accessible").unwrap();
        fs::set_permissions(project.path(), fs::Permissions::from_mode(0o111)).unwrap();
        let result = if unsafe { libc::geteuid() } == 0 {
            Some(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "execution::platform_backend::toolchain::tests::toolchain_identity_records_unreadable_entries_and_permission_changes", "--nocapture"])
                .env("TAPID_TEST_UNREADABLE_TOOLCHAIN_ROOT", project.path())
                .env("TAPID_TEST_UNLISTABLE_TOOLCHAIN", "1")
                .uid(65534).gid(65534).output().unwrap())
        } else {
            assert_eq!(
                fs::read(project.path().join("known-child")).unwrap(),
                b"accessible"
            );
            let result = fingerprint(project.path());
            fs::set_permissions(project.path(), fs::Permissions::from_mode(0o755)).unwrap();
            assert!(
                result.is_err(),
                "searchable directories must still be fingerprinted completely"
            );
            None
        };
        fs::set_permissions(project.path(), fs::Permissions::from_mode(0o755)).unwrap();
        if let Some(output) = result {
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
