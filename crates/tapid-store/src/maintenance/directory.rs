//! Cache traversal and deletion anchored to opened directories. Unix uses
//! descriptor-relative operations; Windows holds directories without delete
//! sharing and deletes entries through their own no-follow handles.
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::Path,
};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
pub(super) use unix::CacheDirectory;
#[cfg(windows)]
pub(super) use windows::CacheDirectory;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    File,
    Directory,
    Link,
    Special,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Entry {
    pub kind: Kind,
    pub bytes: u64,
    pub identity: (u64, u64),
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "cache entry changed during maintenance",
    )
}

impl CacheDirectory {
    pub(super) fn logical_bytes(&self) -> io::Result<u64> {
        let mut bytes = 0u64;
        for name in self.entries()? {
            let entry = self.metadata(&name)?;
            let size = match entry.kind {
                Kind::Directory => {
                    let child = self.directory(&name)?;
                    if child.identity()? != entry.identity {
                        return Err(changed());
                    }
                    child.logical_bytes()?
                }
                Kind::File => entry.bytes,
                Kind::Link => 0,
                Kind::Special => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "cache contains a special file",
                    ));
                }
            };
            bytes = bytes.checked_add(size).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "cache byte count overflow")
            })?;
        }
        Ok(bytes)
    }

    pub(super) fn remove(&self, name: &OsStr, expected: Entry) -> io::Result<()> {
        if expected.kind == Kind::Directory {
            let child = self.directory(name)?;
            if child.identity()? != expected.identity {
                return Err(changed());
            }
            for name in child.entries()? {
                let entry = child.metadata(&name)?;
                child.remove(&name, entry)?;
            }
            drop(child);
        }
        self.remove_entry(name, expected)
    }
}
