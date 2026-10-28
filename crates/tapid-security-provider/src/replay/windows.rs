//! Windows handle/ACL boundary; shared replay policy stays in the parent module.
use super::invalid;
use fs4::FileExt;
use std::{
    fs::File,
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
            SE_FILE_OBJECT,
        },
        DACL_SECURITY_INFORMATION, GetTokenInformation, OWNER_SECURITY_INFORMATION,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::*,
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub(super) struct Directory {
    root: PathBuf,
    // No FILE_SHARE_DELETE: the directory cannot be renamed/replaced while held.
    handle: File,
    security: Security,
}

impl Directory {
    pub(super) fn open(root: &Path) -> io::Result<Self> {
        let security = Security::current()?;
        let path = wide(root)?;
        let attributes = security.attributes();
        // SAFETY: path and descriptor remain valid for the synchronous call.
        if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
                return Err(error);
            }
        }
        let handle = security.open(root, OPEN_EXISTING, true, false)?;
        let directory = Self {
            root: root.to_owned(),
            handle,
            security,
        };
        directory.validate()?;
        Ok(directory)
    }

    fn validate(&self) -> io::Result<()> {
        self.security.validate(&self.handle)?;
        let info = information(&self.handle)?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) fn lock(&self) -> io::Result<File> {
        self.validate()?;
        let file = self
            .security
            .open(&self.root.join(".lock"), OPEN_ALWAYS, false, true)?;
        self.private_file(&file)?;
        FileExt::lock(&file)?;
        // Permissions may have changed while waiting for another writer.
        self.validate()?;
        self.private_file(&file)?;
        Ok(file)
    }

    fn private_file(&self, file: &File) -> io::Result<()> {
        self.security.validate(file)?;
        let info = information(file)?;
        if info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0
            || info.nNumberOfLinks != 1
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) fn read(&self, path: &Path) -> io::Result<File> {
        let file = self.security.open(path, OPEN_EXISTING, false, false)?;
        self.private_file(&file)?;
        Ok(file)
    }

    pub(super) fn stage(&self, path: &Path) -> io::Result<File> {
        let file = self.security.open(path, CREATE_NEW, false, true)?;
        self.private_file(&file)?;
        Ok(file)
    }

    pub(super) fn publish(&self, stage: &Path, path: &Path) -> io::Result<()> {
        self.validate()?;
        let stage = wide(stage)?;
        let path = wide(path)?;
        // SAFETY: both names are terminated, same-directory paths beneath the
        // pinned private root. Never fall back to cross-volume copy/delete.
        if unsafe {
            MoveFileExW(
                stage.as_ptr(),
                path.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    // SAFETY: File owns a live handle and info is writable for its full size.
    unsafe {
        let mut info = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info)
    }
}

struct Local(*mut std::ffi::c_void);
impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: every Local is the unique owner of a LocalAlloc API result.
        unsafe {
            LocalFree(self.0);
        }
    }
}

struct Security {
    descriptor: Local,
    canonical: Vec<u16>,
}
impl Security {
    fn current() -> io::Result<Self> {
        // SAFETY: native token and LocalAlloc buffers are retained until all
        // borrowed SID/descriptor pointers have been consumed; handles use RAII.
        unsafe {
            let mut token = 0;
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let token = OwnedHandle::from_raw_handle(token as _);
            let mut length = 0;
            GetTokenInformation(
                token.as_raw_handle() as HANDLE,
                TokenUser,
                null_mut(),
                0,
                &mut length,
            );
            if length == 0 {
                return Err(io::Error::last_os_error());
            }
            // usize storage supplies TOKEN_USER alignment.
            let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
            if GetTokenInformation(
                token.as_raw_handle() as HANDLE,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut sid = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
                return Err(io::Error::last_os_error());
            }
            let sid_owner = Local(sid.cast());
            let mut count = 0;
            while *sid.add(count) != 0 {
                count += 1;
            }
            let sid_text = String::from_utf16(std::slice::from_raw_parts(sid, count))
                .map_err(|_| invalid())?;
            drop(sid_owner);
            let sddl = format!("O:{sid_text}D:P(A;;FA;;;{sid_text})")
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>();
            let mut descriptor = null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let descriptor = Local(descriptor);
            let canonical = canonical(descriptor.0)?;
            Ok(Self {
                descriptor,
                canonical,
            })
        }
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: 0,
        }
    }

    fn open(
        &self,
        path: &Path,
        disposition: u32,
        directory: bool,
        write: bool,
    ) -> io::Result<File> {
        let path = wide(path)?;
        let attributes = self.attributes();
        let access = FILE_GENERIC_READ | if write { FILE_GENERIC_WRITE } else { 0 };
        let flags = FILE_FLAG_OPEN_REPARSE_POINT
            | if directory {
                FILE_FLAG_BACKUP_SEMANTICS
            } else {
                0
            };
        // SAFETY: attributes and path remain live; handle is transferred exactly
        // once to File and cannot be inherited by children.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &attributes,
                disposition,
                flags,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_handle(handle as _) })
    }

    fn validate(&self, file: &File) -> io::Result<()> {
        let mut descriptor = null_mut();
        // SAFETY: live handle; returned descriptor has a single Local owner.
        let error = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        let descriptor = Local(descriptor);
        // Compare canonical owner + DACL, including protected inheritance flags.
        // This rejects NULL/empty/broadened ACLs rather than repairing them.
        if canonical(descriptor.0)? != self.canonical {
            return Err(invalid());
        }
        Ok(())
    }
}

fn canonical(descriptor: *mut std::ffi::c_void) -> io::Result<Vec<u16>> {
    let mut text = null_mut();
    let mut length = 0;
    // SAFETY: caller keeps a valid security descriptor alive; API returns the
    // buffer length including terminator and allocates with LocalAlloc.
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut text,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let owner = Local(text.cast());
    let result = unsafe { std::slice::from_raw_parts(text, length as usize) }.to_vec();
    drop(owner);
    Ok(result)
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if units.contains(&0) {
        return Err(invalid());
    }
    Ok(units.into_iter().chain(Some(0)).collect())
}
