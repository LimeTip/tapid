use super::super::*;
use rustix::fs::{self as relative, AtFlags, Dir, FileType, Mode, OFlags};
pub(super) use std::fs::{File, Metadata};
use std::os::unix::{ffi::OsStringExt, fs::MetadataExt};

pub(super) fn unchanged(before: &Metadata, after: &Metadata) -> Result<(), ExtractError> {
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.mode() != after.mode()
        || before.nlink() != after.nlink()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(invalid("directory source changed during capture"));
    }
    Ok(())
}

// Native stat field widths differ between supported Unix targets.
#[allow(clippy::unnecessary_cast)]
pub(super) fn open_child(parent: &File, name: &str) -> Result<File, ExtractError> {
    let stat =
        relative::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
    let kind = FileType::from_raw_mode(stat.st_mode);
    if !matches!(kind, FileType::RegularFile | FileType::Directory) {
        return Err(invalid("directory source contains a link or special file"));
    }
    let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    if kind == FileType::Directory {
        flags |= OFlags::DIRECTORY;
    }
    let file =
        File::from(relative::openat(parent, name, flags, Mode::empty()).map_err(io::Error::from)?);
    let metadata = file.metadata()?;
    if metadata.dev() != stat.st_dev as u64 || metadata.ino() != stat.st_ino as u64 {
        return Err(invalid("directory entry replaced during capture"));
    }
    if !metadata.is_dir() && (!metadata.is_file() || metadata.nlink() != 1) {
        return Err(invalid(
            "directory source contains a special or multiply linked file",
        ));
    }
    Ok(file)
}

pub(super) fn open_root(root: &Path) -> Result<File, ExtractError> {
    Ok(File::from(
        relative::open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    ))
}
pub(super) fn names(
    directory: &File,
) -> io::Result<impl Iterator<Item = io::Result<std::ffi::OsString>>> {
    Ok(Dir::read_from(directory)
        .map_err(io::Error::from)?
        .map(|entry| {
            entry
                .map(|entry| std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec()))
                .map_err(io::Error::from)
        }))
}
pub(super) fn executable(metadata: &Metadata) -> bool {
    metadata.mode() & 0o111 != 0
}
fn invalid(message: &str) -> ExtractError {
    ExtractError::InvalidArchive(message.into())
}

pub(super) fn same_path(left: &str, right: &str) -> Result<bool, ExtractError> {
    Ok(left == right)
}
