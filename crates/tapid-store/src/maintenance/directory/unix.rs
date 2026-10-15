use super::*;
use rustix::fs::{self as relative, AtFlags, Dir, FileType, Mode, OFlags};
use std::os::unix::{ffi::OsStringExt, fs::MetadataExt};

pub(in crate::maintenance) struct CacheDirectory {
    file: File,
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

impl CacheDirectory {
    pub(in crate::maintenance) fn open_root(path: &Path) -> io::Result<Self> {
        let before = std::fs::symlink_metadata(path)?;
        if !before.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache root must be a non-symlink directory",
            ));
        }
        let file = File::from(relative::open(path, directory_flags(), Mode::empty())?);
        let after = file.metadata()?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(changed());
        }
        Ok(Self { file })
    }

    pub(in crate::maintenance) fn directory(&self, name: &OsStr) -> io::Result<Self> {
        Ok(Self {
            file: File::from(relative::openat(
                &self.file,
                name,
                directory_flags(),
                Mode::empty(),
            )?),
        })
    }

    pub(in crate::maintenance) fn open_file(&self, name: &OsStr) -> io::Result<File> {
        let file = File::from(relative::openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?);
        if !file.metadata()?.file_type().is_file() {
            return Err(changed());
        }
        Ok(file)
    }

    pub(in crate::maintenance) fn entries(&self) -> io::Result<Vec<OsString>> {
        let mut directory = Dir::read_from(&self.file)?;
        let mut names = Vec::new();
        for entry in &mut directory {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name != b"." && name != b".." {
                names.push(OsString::from_vec(name.to_vec()));
            }
        }
        Ok(names)
    }

    // Stat field widths differ across Unix targets.
    #[allow(clippy::unnecessary_cast)]
    pub(in crate::maintenance) fn metadata(&self, name: &OsStr) -> io::Result<Entry> {
        let stat = relative::statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let kind = match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Kind::File,
            FileType::Directory => Kind::Directory,
            FileType::Symlink => Kind::Link,
            _ => Kind::Special,
        };
        Ok(Entry {
            kind,
            bytes: stat.st_size as u64,
            identity: (stat.st_dev as u64, stat.st_ino as u64),
        })
    }

    pub(in crate::maintenance) fn identity(&self) -> io::Result<(u64, u64)> {
        let meta = self.file.metadata()?;
        Ok((meta.dev(), meta.ino()))
    }

    pub(in crate::maintenance) fn remove_entry(
        &self,
        name: &OsStr,
        expected: Entry,
    ) -> io::Result<()> {
        let current = self.metadata(name)?;
        if current.identity != expected.identity || current.kind != expected.kind {
            return Err(changed());
        }
        let flags = if expected.kind == Kind::Directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        relative::unlinkat(&self.file, name, flags)?;
        Ok(())
    }

    pub(in crate::maintenance) fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}
