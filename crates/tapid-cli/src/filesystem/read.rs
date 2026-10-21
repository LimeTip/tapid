//! Bounded regular-file reads, with no-follow open protection on Unix.
use std::{
    fs,
    io::{self, Read},
    path::Path,
};

#[derive(Debug)]
pub(crate) enum ReadError {
    CapacityExceeded,
    Io(io::Error),
}

pub(crate) fn read_bounded_file(path: &Path, max_bytes: usize) -> Result<Vec<u8>, ReadError> {
    let max_bytes = u64::try_from(max_bytes).map_err(|_| ReadError::CapacityExceeded)?;
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or(ReadError::CapacityExceeded)?;
    let metadata = fs::symlink_metadata(path).map_err(ReadError::Io)?;
    if !metadata.file_type().is_file() {
        return Err(ReadError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file is not a regular file",
        )));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A replaced FIFO must not block the open, nor may a replaced symlink
        // redirect the read after the initial metadata check.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(ReadError::Io)?;
    let metadata = file.metadata().map_err(ReadError::Io)?;
    if !metadata.is_file() {
        return Err(ReadError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file is not a regular file",
        )));
    }
    if metadata.len() > max_bytes {
        return Err(ReadError::CapacityExceeded);
    }
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(ReadError::Io)?;
    if bytes.len() as u64 > max_bytes {
        return Err(ReadError::CapacityExceeded);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regular_file_reads_enforce_exact_limits_and_reject_directories() {
        let project = tapid_test_support::TempProject::new("bounded-file").unwrap();
        let path = project.write("data", b"1234").unwrap();
        assert_eq!(read_bounded_file(&path, 4).unwrap(), b"1234");
        assert!(matches!(
            read_bounded_file(&path, 3),
            Err(ReadError::CapacityExceeded)
        ));
        assert!(matches!(
            read_bounded_file(project.path(), 4),
            Err(ReadError::Io(_))
        ));
        let empty = project.write("empty", b"").unwrap();
        assert!(read_bounded_file(&empty, 0).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_reads_reject_symlinks() {
        let project = tapid_test_support::TempProject::new("bounded-file-link").unwrap();
        let target = project.write("target", b"secret").unwrap();
        let link = project.path().join("link");
        std::os::unix::fs::symlink(target, &link).unwrap();
        assert!(matches!(
            read_bounded_file(&link, 32),
            Err(ReadError::Io(_))
        ));
    }
}
