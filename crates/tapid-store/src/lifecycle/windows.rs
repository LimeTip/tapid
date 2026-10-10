//! Owner-only native key files. No inherited grants or reparse-point traversal.
use super::*;
use std::{
    ffi::c_void,
    mem::{MaybeUninit, size_of},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    ptr::{NonNull, null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree, PSID,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT,
        },
        Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom},
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
        GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ALL_ACCESS,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, GetFileInformationByHandle, OPEN_EXISTING,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: these allocations come exclusively from LocalAlloc-backed Win32 APIs.
        unsafe { LocalFree(self.0) };
    }
}

pub(super) fn lifecycle_key(root: &Path, create: bool) -> Result<[u8; 32], IngestError> {
    let user = current_user()?;
    // SAFETY: GetTokenInformation initialized this suitably aligned TOKEN_USER buffer.
    let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let path = root.join(".tapid-lifecycle-key");
    let mut file = match open_key(&path, None) {
        Ok(file) => file,
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            let descriptor = owner_descriptor(sid)?;
            let mut key = [0u8; 32];
            // SAFETY: the system RNG writes exactly the supplied array length.
            if unsafe {
                BCryptGenRandom(
                    null_mut(),
                    key.as_mut_ptr(),
                    32,
                    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                )
            } < 0
            {
                return Err(invalid("Windows system RNG could not create lifecycle key"));
            }
            let mut file = open_key(&path, Some(&descriptor))?;
            if let Err(error) = validate_key(&file, sid)
                .and_then(|()| file.write_all(&key))
                .and_then(|()| file.sync_all())
            {
                drop(file);
                let _ = fs::remove_file(&path);
                return Err(error.into());
            }
            return Ok(key);
        }
        Err(error) => return Err(error.into()),
    };
    validate_key(&file, sid)?;
    let mut key = [0u8; 32];
    file.read_exact(&mut key)?;
    if file.metadata()?.len() != 32 {
        return Err(invalid(
            "lifecycle authentication key must contain exactly 32 bytes",
        ));
    }
    Ok(key)
}

fn open_key(path: &Path, descriptor: Option<&LocalAllocation>) -> io::Result<File> {
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let attributes = descriptor.map(|descriptor| SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    });
    // No write/delete sharing: the held file cannot be changed or replaced during validation/read.
    // OPEN_REPARSE_POINT opens the link object itself so validation can reject it.
    // SAFETY: path and optional descriptor remain live throughout CreateFileW.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ
                | if descriptor.is_some() {
                    GENERIC_WRITE
                } else {
                    0
                },
            FILE_SHARE_READ,
            attributes.as_ref().map_or(null(), |value| value),
            if descriptor.is_some() {
                CREATE_NEW
            } else {
                OPEN_EXISTING
            },
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a new owned, non-inheritable file handle.
    Ok(unsafe { File::from_raw_handle(handle as *mut c_void) })
}

fn current_user() -> io::Result<Vec<usize>> {
    let mut token = 0;
    // SAFETY: output is writable; GetCurrentProcess supplies the current process pseudo-handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    let token = Token(token);
    let mut required = 0;
    // SAFETY: null/zero is the documented size query.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut required) };
    if required < size_of::<TOKEN_USER>() as u32 || required > 64 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Windows user identity size",
        ));
    }
    let mut user = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
    // SAFETY: aligned buffer has at least required writable bytes.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            user.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Windows user SID",
        ));
    }
    Ok(user)
}

fn owner_descriptor(sid: PSID) -> io::Result<LocalAllocation> {
    let mut text = null_mut();
    // SAFETY: sid is validated and output is writable. Win32 allocates a terminated SID string.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let allocation = LocalAllocation(text.cast());
    let mut units = Vec::new();
    for index in 0..256 {
        let unit = unsafe { *text.add(index) };
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    let user = String::from_utf16(&units)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SID string"))?;
    drop(allocation);
    // Explicit user owner and protected owner-only DACL, applied atomically at CREATE_NEW.
    let sddl: Vec<u16> = format!("O:{user}D:P(A;;FA;;;{user})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = null_mut();
    // SAFETY: terminated SDDL and writable output; API owns the allocated descriptor.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalAllocation(descriptor))
}

fn validate_key(file: &File, sid: PSID) -> io::Result<()> {
    let failure = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "lifecycle key must be an owner-only regular file without reparse points or hard links",
        )
    };
    let handle = file.as_raw_handle() as HANDLE;
    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: file owns a valid handle and info is writable.
    if unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let info = unsafe { info.assume_init() };
    if !file.metadata()?.is_file()
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || info.nNumberOfLinks != 1
    {
        return Err(failure());
    }
    let mut owner = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: valid file handle; returned SID/ACL point into the allocated descriptor.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = LocalAllocation(descriptor);
    let mut control = 0;
    let mut revision = 0;
    if owner.is_null()
        || dacl.is_null()
        || unsafe { EqualSid(owner, sid) } == 0
        || unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
        || unsafe { (*dacl).AceCount } != 1
    {
        return Err(failure());
    }
    let mut ace = null_mut();
    if unsafe { GetAce(dacl, 0, &mut ace) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let ace = NonNull::new(ace).ok_or_else(failure)?;
    let header = unsafe { ace.cast::<ACE_HEADER>().as_ref() };
    if header.AceType != 0 || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
        return Err(failure());
    }
    let ace = unsafe { ace.cast::<ACCESS_ALLOWED_ACE>().as_ref() };
    // Only a single explicit FILE_ALL_ACCESS ACE for this user is accepted. Never repair a weak DACL.
    if ace.Header.AceType != 0
        || ace.Header.AceFlags != 0
        || ace.Mask != FILE_ALL_ACCESS
        || unsafe { EqualSid((&ace.SidStart as *const u32).cast_mut().cast(), sid) } == 0
    {
        return Err(failure());
    }
    Ok(())
}
