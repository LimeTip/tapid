//! Windows traversal holds each ancestor without write or delete sharing. An active
//! writer fails closed, including handles that could retarget a reparse point.
use super::super::*;
use std::fs::{File as NativeFile, OpenOptions};
use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
use std::path::{Component, PathBuf, Prefix};
use std::sync::Arc;
use windows_sys::Win32::Storage::FileSystem::*;

pub(super) struct File {
    file: Arc<NativeFile>,
    path: PathBuf,
    // Every path-based operation is protected by the complete held chain.
    ancestors: Vec<Arc<NativeFile>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Metadata {
    identity: (u32, u64),
    attributes: u32,
    links: u32,
    size: u64,
    written: i64,
    changed: i64,
}
impl Metadata {
    pub(super) fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    pub(super) fn is_file(&self) -> bool {
        !self.is_dir()
    }
    pub(super) fn len(&self) -> u64 {
        self.size
    }
}

fn invalid(message: &str) -> ExtractError {
    ExtractError::InvalidArchive(message.into())
}

fn inspect(file: &NativeFile) -> io::Result<Metadata> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: file owns a live handle; both output buffers have the native
    // structure layout and remain writable for the duration of these calls.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0
        || unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as _,
                FileBasicInfo,
                &mut basic as *mut _ as _,
                std::mem::size_of_val(&basic) as u32,
            )
        } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory source contains a reparse point",
        ));
    }
    // SAFETY: file owns the queried live handle.
    if unsafe { GetFileType(file.as_raw_handle() as _) } != FILE_TYPE_DISK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory source contains a special file",
        ));
    }
    Ok(Metadata {
        identity: (
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        ),
        attributes: info.dwFileAttributes,
        links: info.nNumberOfLinks,
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        written: basic.LastWriteTime,
        changed: basic.ChangeTime,
    })
}

fn open(path: PathBuf, ancestors: Vec<Arc<NativeFile>>) -> Result<File, ExtractError> {
    // The held parent chain prevents ancestor substitution. Denying write
    // sharing also prevents converting a held directory into a reparse point;
    // denying delete sharing alone would not prevent in-place retargeting.
    let native = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&path)?;
    let metadata = inspect(&native)?;
    if metadata.is_file() && metadata.links != 1 {
        return Err(invalid("directory source contains a multiply linked file"));
    }
    Ok(File {
        file: Arc::new(native),
        path,
        ancestors,
    })
}

impl File {
    pub(super) fn metadata(&self) -> io::Result<Metadata> {
        inspect(&self.file)
    }
    pub(super) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.clone(),
            path: self.path.clone(),
            ancestors: self.ancestors.clone(),
        })
    }
}
impl Read for &File {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        (&*self.file).read(bytes)
    }
}

pub(super) fn unchanged(before: &Metadata, after: &Metadata) -> Result<(), ExtractError> {
    if before != after {
        return Err(invalid("directory source changed during capture"));
    }
    Ok(())
}
pub(super) fn executable(_: &Metadata) -> bool {
    false
}

pub(super) fn open_root(root: &Path) -> Result<File, ExtractError> {
    let path = std::path::absolute(root)?;
    let mut current = PathBuf::new();
    let mut ancestors = Vec::new();
    let mut held = None;
    for component in path.components() {
        current.push(component.as_os_str());
        match component {
            Component::Prefix(prefix)
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
            {
                continue;
            }
            Component::RootDir | Component::Normal(_) => {}
            _ => return Err(invalid("directory capture requires a local absolute root")),
        }
        let file = open(current.clone(), ancestors.clone())?;
        if !file.metadata()?.is_dir() {
            return Err(invalid("capture root component is not a directory"));
        }
        ancestors.push(file.file.clone());
        held = Some(file);
    }
    held.ok_or_else(|| invalid("directory capture requires a root"))
}
pub(super) fn open_child(parent: &File, name: &str) -> Result<File, ExtractError> {
    if !parent.metadata()?.is_dir() {
        return Err(invalid("source ancestor is not a directory"));
    }
    let mut ancestors = parent.ancestors.clone();
    ancestors.push(parent.file.clone());
    let file = open(parent.path.join(name), ancestors)?;
    // With the object and its ancestors held against write/delete, resolving
    // its display name cannot substitute the object used for subsequent reads.
    // Reject DOS short-name aliases so managed-root policy cannot be bypassed
    // by naming node_modules (or another managed root) through an 8.3 alias.
    let canonical = fs::canonicalize(&file.path)?;
    let actual = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("directory entry is not UTF-8"))?;
    if !same_path(name, actual)? {
        return Err(invalid("directory capture rejects filesystem name aliases"));
    }
    Ok(file)
}
pub(super) fn names(
    directory: &File,
) -> io::Result<impl Iterator<Item = io::Result<std::ffi::OsString>>> {
    Ok(fs::read_dir(&directory.path)?.map(|entry| entry.map(|entry| entry.file_name())))
}

// Use Windows ordinal case folding rather than ASCII folding or Rust Unicode
// lowercasing, which differ from Windows path comparisons.
pub(super) fn same_path(left: &str, right: &str) -> Result<bool, ExtractError> {
    use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    let left_len =
        i32::try_from(left.len()).map_err(|_| invalid("path comparison exceeds native limit"))?;
    let right_len =
        i32::try_from(right.len()).map_err(|_| invalid("path comparison exceeds native limit"))?;
    // SAFETY: the UTF-16 buffers remain live and their explicit lengths match.
    let result =
        unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) };
    if result == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(result == CSTR_EQUAL)
}
