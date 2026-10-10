use super::*;
use std::{
    fs::{self, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Component, PathBuf},
};
use windows_sys::Win32::Storage::FileSystem::*;

pub(in crate::maintenance) struct CacheDirectory {
    file: File,
    path: PathBuf,
    // Excluding FILE_SHARE_DELETE prevents ancestor replacement while paths
    // are used for enumeration and opening children.
    _ancestors: Vec<File>,
}

fn open(path: &Path, delete: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .access_mode(FILE_GENERIC_READ | if delete { DELETE } else { 0 })
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

fn entry(file: &File) -> io::Result<Entry> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the file owns a live handle, and info is a valid output buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Kind::Link
    } else if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Directory
    } else if file.metadata()?.is_file() {
        Kind::File
    } else {
        Kind::Special
    };
    Ok(Entry {
        kind,
        bytes: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        identity: (
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        ),
    })
}

impl CacheDirectory {
    pub(in crate::maintenance) fn open_root(path: &Path) -> io::Result<Self> {
        let original = open(path, false)?;
        let expected = entry(&original)?;
        if expected.kind != Kind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache root must be a non-reparse directory",
            ));
        }
        let path = fs::canonicalize(path)?;
        let mut ancestors = Vec::new();
        let mut current = PathBuf::new();
        for component in path.components() {
            current.push(component.as_os_str());
            if matches!(component, Component::Prefix(_)) {
                continue;
            }
            let file = open(&current, false)?;
            if entry(&file)?.kind != Kind::Directory {
                return Err(changed());
            }
            ancestors.push(file);
        }
        let file = ancestors.pop().ok_or_else(changed)?;
        if entry(&file)?.identity != expected.identity {
            return Err(changed());
        }
        Ok(Self {
            file,
            path,
            _ancestors: ancestors,
        })
    }

    pub(in crate::maintenance) fn directory(&self, name: &OsStr) -> io::Result<Self> {
        let path = self.path.join(name);
        let file = open(&path, false)?;
        if entry(&file)?.kind != Kind::Directory {
            return Err(changed());
        }
        Ok(Self {
            file,
            path,
            _ancestors: Vec::new(),
        })
    }

    pub(in crate::maintenance) fn open_file(&self, name: &OsStr) -> io::Result<File> {
        let file = open(&self.path.join(name), false)?;
        if entry(&file)?.kind != Kind::File {
            return Err(changed());
        }
        Ok(file)
    }

    pub(in crate::maintenance) fn entries(&self) -> io::Result<Vec<OsString>> {
        fs::read_dir(&self.path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect()
    }

    pub(in crate::maintenance) fn metadata(&self, name: &OsStr) -> io::Result<Entry> {
        entry(&open(&self.path.join(name), false)?)
    }
    pub(in crate::maintenance) fn identity(&self) -> io::Result<(u64, u64)> {
        Ok(entry(&self.file)?.identity)
    }

    pub(in crate::maintenance) fn remove_entry(
        &self,
        name: &OsStr,
        expected: Entry,
    ) -> io::Result<()> {
        let file = open(&self.path.join(name), true)?;
        let current = entry(&file)?;
        if current.identity != expected.identity || current.kind != expected.kind {
            return Err(changed());
        }
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: 1 };
        // SAFETY: the no-follow file handle has DELETE access; the disposition
        // buffer remains live and its size matches the information class.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as _,
                FileDispositionInfo,
                &disposition as *const _ as _,
                std::mem::size_of_val(&disposition) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(in crate::maintenance) fn sync(&self) -> io::Result<()> {
        crate::sync_directory(&self.path)
    }
}
