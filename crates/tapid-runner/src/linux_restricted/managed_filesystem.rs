use super::*;
use std::os::unix::{ffi::OsStrExt, fs::DirBuilderExt};

pub(super) struct View {
    pub(super) path: PathBuf,
    directory: File,
}
impl View {
    pub(super) fn reserve(policy: &ResolvedSandboxPolicy) -> Result<Self, ExecutionError> {
        for grant in &policy.write {
            reject_control_filesystem(&open_path(&grant.path)?)
                .map_err(|error| unsupported(&error.to_string()))?;
        }
        for base in [
            std::env::temp_dir(),
            PathBuf::from("/tmp"),
            PathBuf::from("/var/tmp"),
        ] {
            let Ok(base) = fs::canonicalize(base) else {
                continue;
            };
            if policy
                .write
                .iter()
                .any(|grant| base.starts_with(&grant.path))
            {
                continue;
            }
            let mut random = [0u8; 16];
            File::open("/dev/urandom")
                .and_then(|mut file| file.read_exact(&mut random))
                .map_err(|e| unsupported(&e.to_string()))?;
            let name = random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = base.join(format!("tapid-managed-view-{name}"));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    return Ok(Self {
                        directory: open_path(&path)?,
                        path,
                    });
                }
                Err(_) => continue,
            }
        }
        Err(unsupported(
            "cannot reserve a private filesystem view outside write authority",
        ))
    }
    pub(super) fn identity(&self) -> Result<(u64, u64), ExecutionError> {
        let held = self
            .directory
            .metadata()
            .map_err(|e| unsupported(&e.to_string()))?;
        let current = fs::symlink_metadata(&self.path).map_err(|e| unsupported(&e.to_string()))?;
        if !current.is_dir()
            || current.file_type().is_symlink()
            || (held.dev(), held.ino()) != (current.dev(), current.ino())
        {
            return Err(unsupported(
                "private filesystem view identity changed before launch",
            ));
        }
        Ok((held.dev(), held.ino()))
    }
}

fn reject_control_filesystem(file: &File) -> io::Result<()> {
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if [
        libc::CGROUP_SUPER_MAGIC as libc::c_long,
        libc::CGROUP2_SUPER_MAGIC as libc::c_long,
        libc::PROC_SUPER_MAGIC as libc::c_long,
        libc::SYSFS_MAGIC as libc::c_long,
    ]
    .contains(&stat.f_type)
    {
        return Err(io::Error::other(
            "ManagedTree cannot grant writes to kernel control filesystems",
        ));
    }
    Ok(())
}
impl Drop for View {
    fn drop(&mut self) {
        if self.identity().is_ok() {
            let _ = fs::remove_dir(&self.path);
        }
    }
}

fn cpath(path: &Path) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("NUL in filesystem view path"))
}
#[repr(C)]
struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}

/// A read-only root view closes Landlock's metadata-modification gaps. Only
/// explicitly held write objects receive writable bind mounts in the new root.
pub(super) fn readonly_view(
    writes: &[(PathBuf, u64, u64)],
    view: &(PathBuf, u64, u64),
) -> io::Result<()> {
    let cwd = std::env::current_dir()?;
    let view_meta = fs::symlink_metadata(&view.0)?;
    if !view_meta.is_dir()
        || view_meta.file_type().is_symlink()
        || (view_meta.dev(), view_meta.ino()) != (view.1, view.2)
    {
        return Err(io::Error::other("private filesystem view binding changed"));
    }
    let mut held_writes = Vec::new();
    for (path, device, inode) in writes {
        let file = open_path(path).map_err(|error| io::Error::other(error.to_string()))?;
        reject_control_filesystem(&file)?;
        let metadata = file.metadata()?;
        if !path.is_absolute()
            || !(metadata.is_dir() || metadata.is_file())
            || (metadata.dev(), metadata.ino()) != (*device, *inode)
        {
            return Err(io::Error::other("managed write binding changed"));
        }
        let mut flags: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(file.as_raw_fd(), &mut flags) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if flags.f_flag & libc::ST_RDONLY != 0 {
            return Err(io::Error::other("write grant resides on a read-only mount"));
        }
        held_writes.push((path, file, flags.f_flag));
    }
    let root = cpath(&view.0)?;
    if unsafe {
        libc::mount(
            c"/".as_ptr(),
            root.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let readonly = MountAttr {
        attr_set: 1,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    if unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            libc::AT_FDCWD,
            root.as_ptr(),
            0x8000u32,
            &readonly,
            std::mem::size_of::<MountAttr>(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    for (path, file, flags) in &held_writes {
        let target = view.0.join(
            path.strip_prefix("/")
                .map_err(|_| io::Error::other("write path is not absolute"))?,
        );
        let target = cpath(&target)?;
        let source = cpath(Path::new(&format!("/proc/self/fd/{}", file.as_raw_fd())))?;
        if unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            libc::mount(
                std::ptr::null(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_REMOUNT
                    | libc::MS_BIND
                    | libc::MS_NOSUID
                    | libc::MS_NODEV
                    | if flags & libc::ST_NOEXEC != 0 {
                        libc::MS_NOEXEC
                    } else {
                        0
                    },
                std::ptr::null(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    if unsafe { libc::chroot(root.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    std::env::set_current_dir(cwd)?;
    Ok(())
}
