//! Caller-owned JSON result delivery, shared by run and read-only audit.
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
};

pub(crate) fn create(path: &Path, project: &Path) -> io::Result<File> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "result file needs a filename")
    })?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let parent = fs::canonicalize(parent.unwrap_or(Path::new(".")))?;
    // Every project write grant is confined to the canonical project root by
    // tapid-runner. Backend additions do not grant writes to caller directories.
    // A missing/invalid project cannot launch a child, but still needs a result.
    if let Ok(project) = fs::canonicalize(project)
        && parent.starts_with(project)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "result file must be outside the project root",
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    // std opens non-inheritable handles / close-on-exec descriptors. Keep this
    // exact file open; never follow the destination pathname again after spawn.
    options.open(parent.join(name))
}

pub(crate) fn write(file: &mut File, value: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    if result.is_err() {
        // Best effort only. Consumers must reject partial records even when
        // truncation fails, and must wait for the CLI to finish before reading.
        let _ = file.set_len(0);
    }
    result
}
