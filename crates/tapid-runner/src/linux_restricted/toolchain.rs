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
        let mut children = fs::read_dir(path)
            .map_err(|e| unsupported(&format!("cannot list system toolchain: {e}")))?
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
        let mut file = File::open(path)
            .map_err(|e| unsupported(&format!("cannot read system toolchain: {e}")))?;
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
