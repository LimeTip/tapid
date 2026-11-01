use super::invalid;
use fs4::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
pub(super) struct Directory {
    root: PathBuf,
    owner: u32,
    device: u64,
    inode: u64,
}
impl Directory {
    pub(super) fn open(root: &Path) -> io::Result<Self> {
        let parent = root.parent().ok_or_else(invalid)?;
        let parent_meta = fs::metadata(parent)?;
        if parent_meta.mode() & 0o022 != 0 {
            return Err(invalid());
        }
        match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => File::open(parent)?.sync_all()?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = fs::symlink_metadata(root)?;
        // The caller supplies an owned parent; keep all state under that owner.
        // No HOME or credential lookup participates in this decision.
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.mode() & 0o077 != 0
            || meta.uid() != parent_meta.uid()
        {
            return Err(invalid());
        }
        let store = Self {
            root: root.to_path_buf(),
            owner: meta.uid(),
            device: meta.dev(),
            inode: meta.ino(),
        };
        Ok(store)
    }

    pub(super) fn lock(&self) -> io::Result<File> {
        let root = fs::symlink_metadata(&self.root)?;
        if !root.is_dir()
            || root.file_type().is_symlink()
            || root.mode() & 0o077 != 0
            || root.uid() != self.owner
            || root.dev() != self.device
            || root.ino() != self.inode
        {
            return Err(invalid());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.root.join(".lock"))?;
        private_file(&file)?;
        if file.metadata()?.uid() != self.owner {
            return Err(invalid());
        }
        FileExt::lock(&file)?;
        Ok(file)
    }

    pub(super) fn read(&self, path: &Path) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        private_file(&file)?;
        if file.metadata()?.uid() != self.owner {
            return Err(invalid());
        }
        Ok(file)
    }
    pub(super) fn stage(&self, path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    pub(super) fn publish(&self, stage: &Path, path: &Path) -> io::Result<()> {
        fs::rename(stage, path)?;
        File::open(&self.root)?.sync_all()
    }
}
fn private_file(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.mode() & 0o077 != 0 {
        return Err(invalid());
    }
    Ok(())
}
