//! Writable grants require low labels; only freshly copied dependency trees are labelled here.
use super::{ExecutionError, ExecutionErrorCategory, FilesystemGrantKind, ResolvedGrant};
use std::{
    ffi::c_void,
    fs::{self, File, OpenOptions},
    mem::MaybeUninit,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        ACL,
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
            SetSecurityInfo,
        },
        CONTAINER_INHERIT_ACE, EqualSid, GetAce, GetSecurityDescriptorSacl,
        LABEL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, SYSTEM_MANDATORY_LABEL_ACE,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle, READ_CONTROL, WRITE_OWNER,
    },
    System::SystemServices::SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
};

fn failure(message: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::UnsupportedContainment,
        format!("Windows writable tree: {message}"),
    )
}
struct Descriptor(*mut c_void);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

struct LowLabel {
    _descriptor: Descriptor,
    sacl: *mut ACL,
}
impl LowLabel {
    fn new(directory: bool) -> Result<Self, ExecutionError> {
        let sddl: Vec<u16> = if directory {
            "S:(ML;OICI;NW;;;LW)"
        } else {
            "S:(ML;;NW;;;LW)"
        }
        .encode_utf16()
        .chain(Some(0))
        .collect();
        let mut descriptor = null_mut();
        // SAFETY: terminated static SDDL; output receives a LocalAlloc-backed descriptor.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(failure(std::io::Error::last_os_error()));
        }
        let descriptor = Descriptor(descriptor);
        let mut present = 0;
        let mut defaulted = 0;
        let mut sacl = null_mut();
        // SAFETY: descriptor is valid; outputs are writable and retained by this owner.
        if unsafe {
            GetSecurityDescriptorSacl(descriptor.0, &mut present, &mut sacl, &mut defaulted)
        } == 0
            || present == 0
            || sacl.is_null()
        {
            return Err(failure("missing generated low-integrity label"));
        }
        Ok(Self {
            _descriptor: descriptor,
            sacl,
        })
    }
    fn sid(&self) -> Result<*mut c_void, ExecutionError> {
        let mut ace = null_mut();
        // SAFETY: this SACL comes from the fixed, validated SDDL above.
        if unsafe { GetAce(self.sacl, 0, &mut ace) } == 0 {
            return Err(failure(std::io::Error::last_os_error()));
        }
        let ace = unsafe { &*ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>() };
        Ok((&ace.SidStart as *const u32).cast_mut().cast())
    }
}

fn open_object(path: &Path, label: bool) -> Result<File, ExecutionError> {
    OpenOptions::new()
        .access_mode(READ_CONTROL | if label { WRITE_OWNER } else { 0 })
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(failure)
}
fn directory(file: &File) -> Result<bool, ExecutionError> {
    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: held handle is valid and output is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as isize, info.as_mut_ptr()) } == 0
    {
        return Err(failure(std::io::Error::last_os_error()));
    }
    let info = unsafe { info.assume_init() };
    let metadata = file.metadata().map_err(failure)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !(metadata.is_file() || metadata.is_dir())
        || (metadata.is_file() && info.nNumberOfLinks != 1)
    {
        return Err(failure(
            "write authority cannot contain reparse points, hard links, or special files",
        ));
    }
    Ok(metadata.is_dir())
}

/// Labels a freshly created private dependency copy for AppContainer writes.
///
/// Trusted callers must never pass a developer's live tree or a shared host tree.
/// This changes native integrity metadata, does not grant DACL access, and does
/// not authorize execution. The contained runner independently validates every
/// writable object before launch. Reparse points and hard links fail closed.
pub fn prepare_private_dependency_write_tree(root: &Path) -> Result<(), ExecutionError> {
    visit(root, true, true, |file, is_directory| {
        let label = LowLabel::new(is_directory)?;
        // SAFETY: held object and SACL stay live. LABEL changes only the mandatory label,
        // never the owner or DACL. Only the caller-owned private copy is modified.
        let status = unsafe {
            SetSecurityInfo(
                file.as_raw_handle() as isize,
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                label.sacl,
            )
        };
        if status != 0 {
            return Err(failure(std::io::Error::from_raw_os_error(status as i32)));
        }
        verify_label(file, is_directory)
    })
}

pub(super) fn verify_write_grant(grant: &ResolvedGrant) -> Result<(), ExecutionError> {
    visit(
        &grant.path,
        grant.kind == FilesystemGrantKind::DirectorySubtree,
        false,
        |file, is_directory| {
            verify_label(
                file,
                is_directory && grant.kind == FilesystemGrantKind::DirectorySubtree,
            )
        },
    )
}
fn verify_label(file: &File, inherits: bool) -> Result<(), ExecutionError> {
    let mut sacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: held object and writable outputs. LABEL query needs READ_CONTROL, not audit privileges.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as isize,
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sacl,
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(failure(std::io::Error::from_raw_os_error(status as i32)));
    }
    let _descriptor = Descriptor(descriptor);
    if sacl.is_null() || unsafe { (*sacl).AceCount } != 1 {
        return Err(failure(
            "write target needs an explicit low-integrity label",
        ));
    }
    let mut pointer = null_mut();
    if unsafe { GetAce(sacl, 0, &mut pointer) } == 0 {
        return Err(failure(std::io::Error::last_os_error()));
    }
    let header = unsafe { &*pointer.cast::<windows_sys::Win32::Security::ACE_HEADER>() };
    if header.AceType != 17
        || usize::from(header.AceSize) < std::mem::size_of::<SYSTEM_MANDATORY_LABEL_ACE>()
    {
        return Err(failure("invalid mandatory-label ACE"));
    }
    let ace = unsafe { &*pointer.cast::<SYSTEM_MANDATORY_LABEL_ACE>() };
    let expected = LowLabel::new(inherits)?;
    let inheritance = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;
    if ace.Mask != SYSTEM_MANDATORY_LABEL_NO_WRITE_UP
        || (inherits && ace.Header.AceFlags & inheritance != inheritance)
        || unsafe {
            EqualSid(
                (&ace.SidStart as *const u32).cast_mut().cast(),
                expected.sid()?,
            )
        } == 0
    {
        return Err(failure(
            "write target label must be low with no-write-up and subtree inheritance",
        ));
    }
    Ok(())
}
fn visit(
    root: &Path,
    subtree: bool,
    label: bool,
    mut action: impl FnMut(&File, bool) -> Result<(), ExecutionError>,
) -> Result<(), ExecutionError> {
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0;
    while let Some(path) = pending.pop() {
        count += 1;
        if count > 250_000 {
            return Err(failure("write tree exceeds 250,000 objects"));
        }
        let file = open_object(&path, label)?;
        let is_directory = directory(&file)?;
        action(&file, is_directory)?;
        if subtree && is_directory {
            for entry in fs::read_dir(&path).map_err(failure)? {
                pending.push(entry.map_err(failure)?.path());
            }
        }
    }
    Ok(())
}
