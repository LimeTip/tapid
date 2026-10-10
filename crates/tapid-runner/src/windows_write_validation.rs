//! Read-only preflight for existing, explicitly declared Windows write targets.
use super::{
    ExecutionError, ExecutionErrorCategory, FilesystemAccess, FilesystemGrantKind,
    ResolvedFilesystemGrant,
};
use std::{
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::*;

fn unsupported(message: &str) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCategory::UnsupportedContainment, message)
}
fn inspection_error(error: std::io::Error) -> ExecutionError {
    unsupported(&format!(
        "cannot inspect existing Windows write topology: {error}"
    ))
}

struct Object(HANDLE);
impl Drop for Object {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
impl Object {
    fn open(path: &Path) -> Result<Self, ExecutionError> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: owned terminated path, read-only no-follow open; directories supported.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(inspection_error(std::io::Error::last_os_error()));
        }
        Ok(Self(handle))
    }
    fn validate_acl(&self) -> Result<(), ExecutionError> {
        use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, SE_DACL_PROTECTED,
        };
        let mut dacl = null_mut();
        let mut descriptor = null_mut();
        // SAFETY: held handle, writable outputs; free returned allocation on every path.
        let status = unsafe {
            GetSecurityInfo(
                self.0,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 || descriptor.is_null() {
            if !descriptor.is_null() {
                unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor) };
            }
            return Err(unsupported("cannot read Windows write target DACL"));
        }
        let mut control = 0;
        let mut revision = 0;
        let read = unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) };
        let null_dacl = dacl.is_null();
        unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor) };
        if null_dacl {
            return Err(unsupported("Windows write target has ambiguous NULL DACL"));
        }
        if read == 0 || control & SE_DACL_PROTECTED != 0 {
            return Err(unsupported(
                "Windows write target has unsupported protected DACL",
            ));
        }
        Ok(())
    }

    fn information(&self) -> Result<BY_HANDLE_FILE_INFORMATION, ExecutionError> {
        let mut info = unsafe { std::mem::zeroed() };
        // SAFETY: live owned handle and correctly sized writable output.
        if unsafe { GetFileInformationByHandle(self.0, &mut info) } == 0 {
            return Err(inspection_error(std::io::Error::last_os_error()));
        }
        Ok(info)
    }
}

// This pin is local to grant installation, not a NativeObject receipt or a path-component lock.
pub(super) struct PinnedWriteTarget(Object);
impl PinnedWriteTarget {
    pub(super) fn open(path: &Path) -> Result<Self, ExecutionError> {
        Ok(Self(Object::open(path)?))
    }
    pub(super) fn verify(
        &self,
        handle: HANDLE,
        kind: FilesystemGrantKind,
    ) -> Result<(), ExecutionError> {
        // Borrow only: the ACL transaction owns and closes this handle.
        let actual = std::mem::ManuallyDrop::new(Object(handle));
        let selected = self.0.information()?;
        let opened = actual.information()?;
        let identity = |info: &BY_HANDLE_FILE_INFORMATION| {
            (
                info.dwVolumeSerialNumber,
                info.nFileIndexHigh,
                info.nFileIndexLow,
            )
        };
        let directory = opened.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        let kind_matches = match kind {
            FilesystemGrantKind::ExactFile => !directory,
            FilesystemGrantKind::ExactDirectory | FilesystemGrantKind::DirectorySubtree => {
                directory
            }
            FilesystemGrantKind::CharacterDevice => false,
        };
        if identity(&selected) != identity(&opened)
            || !kind_matches
            || opened.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (!directory && opened.nNumberOfLinks != 1)
        {
            return Err(unsupported(
                "Windows selected write target identity or kind changed",
            ));
        }
        actual.validate_acl()
    }
}

pub(super) fn validate_existing_write_grants(
    grants: &[ResolvedFilesystemGrant],
) -> Result<(), ExecutionError> {
    for grant in grants
        .iter()
        .filter(|g| g.access() == FilesystemAccess::Write)
    {
        if grant.source() != super::FilesystemGrantSource::ProjectPolicy {
            return Err(unsupported(
                "Windows write grants require explicit project policy",
            ));
        }
        if grant.binding() != super::FilesystemBindingMode::CanonicalPath {
            return Err(unsupported(
                "Windows write grants require CanonicalPath binding",
            ));
        }
        let home = std::env::var_os("USERPROFILE")
            .ok_or_else(|| unsupported("cannot locate shared host boundary"))?;
        let system_root = std::env::var_os("SystemRoot")
            .ok_or_else(|| unsupported("cannot locate shared host boundary"))?;
        for boundary in [
            std::path::PathBuf::from(home),
            std::path::PathBuf::from(system_root),
            std::env::temp_dir(),
        ] {
            let boundary = std::fs::canonicalize(boundary).map_err(inspection_error)?;
            if boundary
                .ancestors()
                .any(|ancestor| super::platform_paths_semantically_equal(ancestor, grant.path()))
            {
                return Err(unsupported(
                    "Windows write target is a shared host boundary",
                ));
            }
        }
        // Inspect ancestors without changing their ACLs. CanonicalPath still assumes a trusted
        // host does not replace components after this read-only walk.
        for ancestor in grant.path().ancestors().skip(1) {
            use std::os::windows::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(ancestor).map_err(inspection_error)?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(unsupported(
                    "Windows write target has unsafe ancestor topology",
                ));
            }
        }
        let mut pending = vec![grant.path().to_owned()];
        while let Some(path) = pending.pop() {
            let metadata = std::fs::symlink_metadata(&path).map_err(inspection_error)?;
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(unsupported(
                    "Windows write topology contains a reparse point",
                ));
            }
            let object = Object::open(&path)?;
            object.validate_acl()?;
            let info = object.information()?;
            let is_directory = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
            if metadata.is_dir() != is_directory
                || (path == grant.path()
                    && !match grant.kind() {
                        FilesystemGrantKind::ExactFile => !is_directory && metadata.is_file(),
                        FilesystemGrantKind::ExactDirectory
                        | FilesystemGrantKind::DirectorySubtree => is_directory,
                        FilesystemGrantKind::CharacterDevice => false,
                    })
            {
                return Err(unsupported(
                    "Windows write target kind is unsupported or changed",
                ));
            }
            if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(unsupported(
                    "Windows write topology changed to a reparse point",
                ));
            }
            if metadata.is_file() && info.nNumberOfLinks != 1 {
                return Err(unsupported("Windows writable file has multiple hard links"));
            }
            if metadata.is_dir() && grant.kind() == FilesystemGrantKind::DirectorySubtree {
                for entry in std::fs::read_dir(&path).map_err(inspection_error)? {
                    pending.push(entry.map_err(inspection_error)?.path());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
use super::windows_acl;

#[cfg(test)]
mod tests {
    use super::windows_acl::read_acl;
    use super::*;
    use crate::execution::{
        FilesystemAccess, FilesystemBindingMode, FilesystemGrantKind, FilesystemGrantSource,
    };
    use std::path::{Path, PathBuf};

    fn write_grant(path: &Path, kind: FilesystemGrantKind) -> ResolvedFilesystemGrant {
        ResolvedFilesystemGrant {
            path: path.to_owned(),
            access: FilesystemAccess::Write,
            kind,
            source: FilesystemGrantSource::ProjectPolicy,
            binding: FilesystemBindingMode::CanonicalPath,
        }
    }

    struct Fixture {
        _owner: tapid_test_support::TempProject,
        root: PathBuf,
        target: PathBuf,
        outside: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let owner = tapid_test_support::TempProject::new("write-validation").unwrap();
            let root = std::fs::canonicalize(owner.path()).unwrap();
            let target = root.join("declared spaced ü");
            std::fs::create_dir(&target).unwrap();
            let outside = root.join("outside sentinel.txt");
            std::fs::write(&outside, b"host-positive-control").unwrap();
            println!("HOST_POSITIVE_CONTROL path={}", outside.display());
            super::windows_acl::initialize_inheritance(&root);
            Self {
                _owner: owner,
                root,
                target,
                outside,
            }
        }
        fn rejects(&self, path: &Path, kind: FilesystemGrantKind) {
            let mut paths: Vec<_> = self.root.ancestors().map(Path::to_path_buf).collect();
            let mut pending = vec![self.root.clone()];
            while let Some(path) = pending.pop() {
                use std::os::windows::fs::MetadataExt;
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    continue;
                }
                paths.push(path.clone());
                if metadata.is_dir() {
                    pending.extend(std::fs::read_dir(path).unwrap().map(|e| e.unwrap().path()));
                }
            }
            paths.sort();
            paths.dedup();
            let before: Vec<_> = paths.iter().map(|p| read_acl(p)).collect();
            let result = validate_existing_write_grants(&[write_grant(path, kind)]);
            assert!(
                result.is_err(),
                "unsupported write topology accepted: {path:?}"
            );
            assert!(!self.target.join("child-marker").exists());
            assert_eq!(
                std::fs::read(&self.outside).unwrap(),
                b"host-positive-control"
            );
            for (p, before) in paths.iter().zip(before) {
                let after = read_acl(p);
                println!(
                    "DACL_RECEIPT path={} before_control={} before_acl={:?} after_control={} after_acl={:?}",
                    p.display(),
                    before.0,
                    before.1,
                    after.0,
                    after.1
                );
                assert_eq!(before, after);
            }
        }
    }

    fn set_protected(path: &Path) {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        };
        let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut dacl = null_mut();
        let mut descriptor = null_mut();
        // SAFETY: live test-owned path and writable outputs; retain allocated descriptor through update.
        assert_eq!(
            unsafe {
                windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW(
                    wide.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    &mut dacl,
                    null_mut(),
                    &mut descriptor,
                )
            },
            0
        );
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            )
        };
        unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor) };
        assert_eq!(status, 0);
    }

    #[test]
    fn null_dacl_is_rejected_as_ambiguous() {
        use windows_sys::Win32::Security::Authorization::{
            GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
        };
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, UNPROTECTED_DACL_SECURITY_INFORMATION,
        };
        let fixture = Fixture::new();
        let file = fixture.target.join("null-dacl.txt");
        std::fs::write(&file, b"existing").unwrap();
        let before = read_acl(&file);
        let object = Object::open(&file).unwrap();
        let mut dacl = null_mut();
        let mut descriptor = null_mut();
        // SAFETY: test owns file, handle and descriptor through restoration.
        unsafe {
            assert_eq!(
                GetSecurityInfo(
                    object.0,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    &mut dacl,
                    null_mut(),
                    &mut descriptor
                ),
                0
            );
            assert_eq!(
                SetSecurityInfo(
                    object.0,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    null(),
                    null()
                ),
                0
            );
        }
        let result =
            validate_existing_write_grants(&[write_grant(&file, FilesystemGrantKind::ExactFile)]);
        unsafe {
            assert_eq!(
                SetSecurityInfo(
                    object.0,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    dacl,
                    null(),
                ),
                0
            );
            windows_sys::Win32::Foundation::LocalFree(descriptor);
        }
        assert_eq!(
            read_acl(&file),
            before,
            "NULL DACL fixture restoration must be exact"
        );
        assert!(result.is_err(), "NULL DACL accepted");
        assert!(result.unwrap_err().to_string().contains("NULL DACL"));
        assert!(!fixture.target.join("child-marker").exists());
    }

    #[test]
    fn host_write_dac_denial_is_fatal() {
        let fixture = Fixture::new();
        let file = fixture.target.join("denied.txt");
        std::fs::write(&file, b"existing").unwrap();
        let icacls = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("icacls.exe");
        let output = std::process::Command::new(icacls)
            .arg(&file)
            .args(["/grant", "*S-1-3-4:R", "/deny", "*S-1-1-0:(WDAC)"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "WRITE_DAC fixture setup: {output:?}"
        );
        let before = read_acl(&file);
        fixture.rejects(&file, FilesystemGrantKind::ExactFile);
        assert_eq!(read_acl(&file), before);
    }

    #[test]
    fn denied_descendant_authority_is_rejected() {
        let fixture = Fixture::new();
        let file = fixture.target.join("denied.txt");
        std::fs::write(&file, b"existing").unwrap();
        let icacls = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("icacls.exe");
        let output = std::process::Command::new(icacls)
            .arg(&file)
            .args(["/grant", "*S-1-3-4:R", "/deny", "*S-1-1-0:(WDAC)"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "WRITE_DAC fixture setup: {output:?}"
        );
        fixture.rejects(&fixture.target, FilesystemGrantKind::DirectorySubtree);
    }

    #[test]
    fn protected_descendant_is_rejected_before_mutation() {
        let fixture = Fixture::new();
        let file = fixture.target.join("protected.txt");
        std::fs::write(&file, b"existing").unwrap();
        set_protected(&file);
        let before = read_acl(&file);
        fixture.rejects(&fixture.target, FilesystemGrantKind::DirectorySubtree);
        assert_eq!(read_acl(&file), before);
    }

    fn junction(link: &Path, destination: &Path) {
        use std::os::windows::process::CommandExt;
        let cmd = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("cmd.exe");
        let output = std::process::Command::new(cmd)
            .raw_arg(format!(
                "/D /S /C \"mklink /J \"{}\" \"{}\"\"",
                link.display(),
                destination.display()
            ))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "junction prerequisite failed: {output:?}"
        );
    }

    #[test]
    fn reparse_ancestor_is_rejected() {
        let fixture = Fixture::new();
        let outside = fixture.root.join("outside tree");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("file.txt"), b"outside").unwrap();
        let link = fixture.target.join("junction");
        junction(&link, &outside);
        fixture.rejects(&link.join("file.txt"), FilesystemGrantKind::ExactFile);
        std::fs::remove_dir(&link).unwrap();
    }

    #[test]
    fn outside_junction_is_rejected_without_traversal() {
        let fixture = Fixture::new();
        let outside = fixture.root.join("outside tree");
        std::fs::create_dir(&outside).unwrap();
        let link = fixture.target.join("junction");
        junction(&link, &outside);
        fixture.rejects(&fixture.target, FilesystemGrantKind::DirectorySubtree);
        std::fs::remove_dir(&link).unwrap();
    }

    #[test]
    fn outside_hard_link_is_rejected_before_acl_changes() {
        let fixture = Fixture::new();
        std::fs::hard_link(&fixture.outside, fixture.target.join("alias.txt")).unwrap();
        fixture.rejects(&fixture.target, FilesystemGrantKind::DirectorySubtree);
    }

    #[test]
    fn native_acl_write_handle_rejects_multi_link_file() {
        let fixture = Fixture::new();
        let alias = fixture.target.join("alias.txt");
        std::fs::hard_link(&fixture.outside, &alias).unwrap();
        let mut container = crate::execution::windows_job::WindowsAppContainer::create().unwrap();
        let result = crate::execution::windows_job::WindowsPathAcl::grant(
            &alias,
            container.sid(),
            FilesystemAccess::Write,
            FilesystemGrantKind::ExactFile,
        );
        let rejected = result.is_err();
        drop(result);
        container.cleanup().unwrap();
        assert!(rejected, "unsafe native write handle accepted");
    }

    #[test]
    fn apply_preflights_all_writes_before_any_acl_mutation() {
        let fixture = Fixture::new();
        std::fs::hard_link(&fixture.outside, fixture.target.join("alias.txt")).unwrap();
        let before = read_acl(&fixture.outside);
        let mut read = write_grant(&fixture.root, FilesystemGrantKind::DirectorySubtree);
        read.access = FilesystemAccess::Read;
        let mut container = crate::execution::windows_job::WindowsAppContainer::create().unwrap();
        let result = crate::execution::windows_job::WindowsFilesystemGrants::apply(
            &container,
            &[
                read,
                write_grant(&fixture.target, FilesystemGrantKind::DirectorySubtree),
            ],
        );
        let rejected = result.is_err();
        drop(result);
        container.cleanup().unwrap();
        assert!(
            rejected,
            "apply accepted unsafe subtree instead of read-only preflight rejection"
        );
        assert_eq!(read_acl(&fixture.outside), before);
        assert!(!fixture.target.join("child-marker").exists());
    }

    #[test]
    fn replaced_target_identity_is_rejected_at_acl_handle() {
        let fixture = Fixture::new();
        let file = fixture.target.join("selected.txt");
        std::fs::write(&file, b"selected").unwrap();
        let selected = PinnedWriteTarget::open(&file).unwrap();
        std::fs::rename(&file, fixture.target.join("old.txt")).unwrap();
        std::fs::write(&file, b"replacement").unwrap();
        let replacement = Object::open(&file).unwrap();
        assert!(
            selected
                .verify(replacement.0, FilesystemGrantKind::ExactFile)
                .is_err(),
            "changed identity accepted"
        );
    }

    #[test]
    fn write_validation_never_claims_native_object_binding() {
        let fixture = Fixture::new();
        let mut grant = write_grant(&fixture.target, FilesystemGrantKind::DirectorySubtree);
        grant.binding = FilesystemBindingMode::NativeObject;
        assert!(
            validate_existing_write_grants(&[grant]).is_err(),
            "unproved NativeObject binding accepted"
        );
    }

    #[test]
    fn runtime_origin_does_not_authorize_project_writes() {
        let fixture = Fixture::new();
        let mut grant = write_grant(&fixture.target, FilesystemGrantKind::DirectorySubtree);
        grant.source = FilesystemGrantSource::BackendRuntime;
        assert!(
            validate_existing_write_grants(&[grant]).is_err(),
            "backend runtime write accepted"
        );
    }

    #[test]
    fn shared_host_boundaries_are_not_write_targets() {
        let home = std::fs::canonicalize(std::env::var_os("USERPROFILE").unwrap()).unwrap();
        let system_root = std::fs::canonicalize(std::env::var_os("SystemRoot").unwrap()).unwrap();
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let volume = home.ancestors().last().unwrap().to_owned();
        for path in [home, system_root, temp, volume] {
            let before = read_acl(&path);
            let result = validate_existing_write_grants(&[write_grant(
                &path,
                FilesystemGrantKind::ExactDirectory,
            )]);
            assert!(result.is_err(), "shared boundary accepted: {path:?}");
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("shared host boundary")
            );
            assert_eq!(read_acl(&path), before);
        }
    }

    #[test]
    fn resolved_target_kind_change_is_rejected() {
        let fixture = Fixture::new();
        fixture.rejects(&fixture.target, FilesystemGrantKind::ExactFile);
    }

    #[test]
    fn existing_exact_file_preserves_canonical_receipt_fields() {
        let fixture = Fixture::new();
        let file = fixture.target.join("existing spaced 雪.txt");
        std::fs::write(&file, b"existing").unwrap();
        let grant = write_grant(&file, FilesystemGrantKind::ExactFile);
        let original = grant.clone();
        let before = read_acl(&file);
        validate_existing_write_grants(std::slice::from_ref(&grant)).unwrap();
        assert_eq!(grant, original);
        assert_eq!(grant.binding(), FilesystemBindingMode::CanonicalPath);
        let mut container = crate::execution::windows_job::WindowsAppContainer::create().unwrap();
        let mut applied =
            crate::execution::windows_job::WindowsFilesystemGrants::apply(&container, &[grant])
                .unwrap();
        applied.restore().unwrap();
        container.cleanup().unwrap();
        assert_eq!(read_acl(&file), before);
    }

    #[test]
    fn ordinary_unicode_directory_subtree_is_supported() {
        let fixture = Fixture::new();
        let nested = fixture.target.join("nested 雪");
        std::fs::create_dir(&nested).unwrap();
        let file = nested.join("existing spaced.txt");
        std::fs::write(&file, b"ordinary").unwrap();
        let grant = write_grant(&fixture.target, FilesystemGrantKind::DirectorySubtree);
        let paths = [
            &fixture.root,
            &fixture.target,
            &nested,
            &file,
            &fixture.outside,
        ];
        let before: Vec<_> = paths.iter().map(|p| read_acl(p)).collect();
        validate_existing_write_grants(std::slice::from_ref(&grant)).unwrap();
        let mut container = crate::execution::windows_job::WindowsAppContainer::create().unwrap();
        let mut applied =
            crate::execution::windows_job::WindowsFilesystemGrants::apply(&container, &[grant])
                .unwrap();
        applied.restore().unwrap();
        container.cleanup().unwrap();
        for (path, snapshot) in paths.iter().zip(before) {
            assert_eq!(read_acl(path), snapshot);
        }
    }

    #[test]
    fn missing_components_are_not_materialized() {
        let fixture = Fixture::new();
        let missing = fixture.target.join("missing").join("nested");
        fixture.rejects(&missing, FilesystemGrantKind::DirectorySubtree);
        assert!(!fixture.target.join("missing").exists());
    }

    #[test]
    fn nondirectory_ancestor_is_rejected() {
        let fixture = Fixture::new();
        let file = fixture.target.join("file.txt");
        std::fs::write(&file, b"not a directory").unwrap();
        fixture.rejects(&file.join("child"), FilesystemGrantKind::DirectorySubtree);
    }

    #[test]
    fn protected_root_is_rejected() {
        let fixture = Fixture::new();
        set_protected(&fixture.target);
        fixture.rejects(&fixture.target, FilesystemGrantKind::DirectorySubtree);
    }

    #[test]
    fn dangling_nested_junction_is_rejected_without_traversal() {
        let fixture = Fixture::new();
        let nested = fixture.target.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let link = nested.join("dangling");
        junction(&link, &fixture.root.join("absent destination"));
        fixture.rejects(&fixture.target, FilesystemGrantKind::DirectorySubtree);
        std::fs::remove_dir(&link).unwrap();
        assert!(!fixture.root.join("absent destination").exists());
    }

    #[test]
    fn missing_leaf_is_rejected_without_materialization() {
        let fixture = Fixture::new();
        let missing = fixture.target.join("missing");
        fixture.rejects(&missing, FilesystemGrantKind::DirectorySubtree);
        assert!(!missing.exists());
    }
}
