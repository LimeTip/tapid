#![cfg(all(windows, test))]
use super::*;
use windows_sys::Win32::System::JobObjects::{
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

#[test]
fn assigning_an_invalid_process_handle_fails_closed() {
    let job = WindowsJob::new(
        &ExecutionLimits::new(None, None, None, None).unwrap(),
        false,
    )
    .unwrap();
    assert!(job.assign_suspended_process(0).is_err());
}

#[test]
#[ignore = "one-off Windows 11 ACL diagnosis; not a support acceptance test"]
fn appcontainer_actual_token_file_access_probe() {
    use super::super::{FilesystemBindingMode, FilesystemGrantSource, ResolvedFilesystemGrant};
    use std::io::Read;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::{
        DuplicateTokenEx, ImpersonateLoggedOnUser, RevertToSelf, SecurityImpersonation,
        TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY, TokenImpersonation,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_ALWAYS, CREATE_NEW, CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL,
        FILE_EXECUTE, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_DATA, OPEN_ALWAYS,
        OPEN_EXISTING, WriteFile,
    };
    use windows_sys::Win32::System::Threading::OpenProcessToken;

    struct RevertImpersonation;
    impl Drop for RevertImpersonation {
        fn drop(&mut self) {
            unsafe { RevertToSelf() };
        }
    }

    fn path_wide(path: &std::path::Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn probe_file(
        path: &std::path::Path,
        access: u32,
        disposition: u32,
        label: &str,
    ) -> (bool, bool) {
        let wide = path_wide(path);
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                disposition,
                FILE_ATTRIBUTE_NORMAL,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            eprintln!("{label}: OPEN_DENIED win32={}", unsafe { GetLastError() });
            return (false, false);
        }
        if disposition == CREATE_NEW {
            eprintln!("{label}: CREATE_OPEN_OK");
        }
        let byte = *b"X";
        let mut written = 0;
        let result =
            unsafe { WriteFile(handle, byte.as_ptr(), 1, &mut written, std::ptr::null_mut()) };
        let write_succeeded = result != 0;
        if !write_succeeded {
            eprintln!("{label}: WRITE_DENIED win32={}", unsafe { GetLastError() });
        } else {
            eprintln!("{label}: WRITE_OK bytes={written}");
        }
        unsafe { CloseHandle(handle) };
        (true, write_succeeded)
    }

    fn probe_directory(path: &std::path::Path, label: &str) -> bool {
        let wide = path_wide(path);
        let mut execute_ok = false;
        for (access, access_label) in [
            (FILE_EXECUTE, "FILE_TRAVERSE"),
            (FILE_READ_ATTRIBUTES, "FILE_READ_ATTRIBUTES"),
            (FILE_LIST_DIRECTORY, "FILE_LIST_DIRECTORY"),
            (
                FILE_EXECUTE | FILE_READ_ATTRIBUTES,
                "FILE_TRAVERSE|FILE_READ_ATTRIBUTES",
            ),
        ] {
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    0,
                )
            };
            let succeeded = handle != INVALID_HANDLE_VALUE;
            if succeeded {
                unsafe { CloseHandle(handle) };
            }
            let error = if succeeded {
                0
            } else {
                unsafe { GetLastError() }
            };
            eprintln!(
                "{label}: {access_label} {}{}",
                if succeeded { "OK" } else { "DENIED win32=" },
                if succeeded {
                    String::new()
                } else {
                    error.to_string()
                }
            );
            if access == FILE_EXECUTE {
                execute_ok = succeeded;
            }
        }
        execute_ok
    }

    fn query_acl(icacls: &std::path::Path, path: &std::path::Path) -> Vec<u8> {
        let output = std::process::Command::new(icacls)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "icacls failed for {}: {}{}",
            path.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    let root = std::env::temp_dir().join(format!(
        "tapid-actual-token-acl-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let writable = root.join("writable");
    std::fs::create_dir_all(&writable).unwrap();
    let existing = writable.join("existing.txt");
    let created = writable.join("created.txt");
    std::fs::write(&existing, b"before").unwrap();

    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let icacls = std::path::PathBuf::from(&system_root).join("System32/icacls.exe");
    let label = std::process::Command::new(&icacls)
        .arg(&writable)
        .args(["/setintegritylevel", "(OI)(CI)L", "/c"])
        .output()
        .unwrap();
    assert!(
        label.status.success(),
        "icacls failed: {}",
        String::from_utf8_lossy(&label.stdout)
    );

    let command_target = existing.clone();
    let root = std::fs::canonicalize(&root).unwrap();
    let writable = std::fs::canonicalize(&writable).unwrap();
    let existing = std::fs::canonicalize(&existing).unwrap();
    let system_root_canonical =
        std::fs::canonicalize(std::path::PathBuf::from(&system_root)).unwrap();
    let volume_root = system_root_canonical.parent().unwrap().to_path_buf();
    let mut audit_paths = vec![
        root.clone(),
        writable.clone(),
        existing.clone(),
        root.parent().unwrap().to_path_buf(),
        system_root_canonical.clone(),
        volume_root.clone(),
    ];
    audit_paths.sort();
    audit_paths.dedup();
    let baseline_acls = audit_paths
        .iter()
        .map(|path| query_acl(&icacls, path))
        .collect::<Vec<_>>();

    let mut container = WindowsAppContainer::create().unwrap();
    let payload = format!("echo TAPID_FROM_CMD>\"{}\"", command_target.display());
    let mut pipes = WindowsStdioPipes::new().unwrap();
    let limits =
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap();
    let (job, mut child) = create_appcontainer_child_inner_with_cwd(
        &container,
        &payload,
        &root,
        &limits,
        true,
        true,
        Some(&mut pipes),
    );
    let (mut stdout_pipe, mut stderr_pipe) = pipes.into_parent_readers().unwrap();
    let system32 = std::fs::canonicalize(
        std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"),
    )
    .unwrap();
    let resolved_grants = vec![
        ResolvedFilesystemGrant {
            path: root.clone(),
            access: FilesystemAccess::Read,
            kind: FilesystemGrantKind::DirectorySubtree,
            source: FilesystemGrantSource::ProjectPolicy,
            binding: FilesystemBindingMode::CanonicalPath,
        },
        ResolvedFilesystemGrant {
            path: system32,
            access: FilesystemAccess::Read,
            kind: FilesystemGrantKind::DirectorySubtree,
            source: FilesystemGrantSource::BackendRuntime,
            binding: FilesystemBindingMode::CanonicalPath,
        },
        ResolvedFilesystemGrant {
            path: writable.clone(),
            access: FilesystemAccess::Write,
            kind: FilesystemGrantKind::DirectorySubtree,
            source: FilesystemGrantSource::ProjectPolicy,
            binding: FilesystemBindingMode::CanonicalPath,
        },
    ];
    let mut grants = WindowsFilesystemGrants::apply(&container, &resolved_grants).unwrap();
    let active_acls = audit_paths
        .iter()
        .map(|path| query_acl(&icacls, path))
        .collect::<Vec<_>>();
    for ((path, before), active) in audit_paths.iter().zip(&baseline_acls).zip(&active_acls) {
        eprintln!(
            "active DACL path={} changed={}\n{}",
            path.display(),
            before != active,
            String::from_utf8_lossy(active)
        );
    }
    let system_temp = root.parent().unwrap().to_path_buf();
    let system_temp_index = audit_paths
        .iter()
        .position(|path| path == &system_temp)
        .unwrap();
    assert_eq!(
        active_acls[system_temp_index], baseline_acls[system_temp_index],
        "project grant must not modify shared SystemTemp"
    );
    eprintln!("SystemTemp DACL unchanged; testing declared descendant access");

    let dacl = std::process::Command::new(&icacls)
        .arg(&existing)
        .output()
        .unwrap();
    eprintln!(
        "actual grant target DACL: {}",
        String::from_utf8_lossy(&dacl.stdout)
    );

    let mut primary_token = 0;
    assert_ne!(
        unsafe {
            OpenProcessToken(
                child.process_handle(),
                TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
                &mut primary_token,
            )
        },
        0,
        "OpenProcessToken win32={}",
        unsafe { GetLastError() }
    );
    let mut impersonation_token = 0;
    assert_ne!(
        unsafe {
            DuplicateTokenEx(
                primary_token,
                TOKEN_QUERY | TOKEN_IMPERSONATE,
                std::ptr::null(),
                SecurityImpersonation,
                TokenImpersonation,
                &mut impersonation_token,
            )
        },
        0,
        "DuplicateTokenEx win32={}",
        unsafe { GetLastError() }
    );
    assert_ne!(
        unsafe { ImpersonateLoggedOnUser(impersonation_token) },
        0,
        "ImpersonateLoggedOnUser win32={}",
        unsafe { GetLastError() }
    );
    let revert = RevertImpersonation;

    assert!(
        probe_directory(&root, "project root"),
        "AppContainer cannot traverse project root"
    );
    assert!(
        probe_directory(&writable, "write directory"),
        "AppContainer cannot traverse write directory"
    );
    for (path, label) in [
        (root.parent().unwrap(), "SystemTemp ancestor"),
        (system_root_canonical.as_path(), "Windows ancestor"),
        (volume_root.as_path(), "volume-root ancestor"),
    ] {
        eprintln!(
            "ancestor access label={label} path={} traverse={}",
            path.display(),
            probe_directory(path, label)
        );
    }
    for (path, access, disposition, label) in [
        (
            &existing,
            FILE_WRITE_DATA,
            OPEN_EXISTING,
            "existing FILE_WRITE_DATA",
        ),
        (
            &existing,
            FILE_APPEND_DATA,
            OPEN_EXISTING,
            "existing FILE_APPEND_DATA",
        ),
        (
            &existing,
            GENERIC_WRITE,
            OPEN_EXISTING,
            "existing GENERIC_WRITE",
        ),
        (
            &existing,
            GENERIC_WRITE,
            CREATE_ALWAYS,
            "existing CREATE_ALWAYS + GENERIC_WRITE",
        ),
        (
            &existing,
            GENERIC_WRITE,
            OPEN_ALWAYS,
            "existing OPEN_ALWAYS + GENERIC_WRITE",
        ),
        (
            &created,
            FILE_WRITE_DATA,
            CREATE_NEW,
            "new CREATE_NEW + FILE_WRITE_DATA",
        ),
        (
            &created,
            FILE_WRITE_DATA,
            OPEN_EXISTING,
            "new reopen FILE_WRITE_DATA",
        ),
        (
            &created,
            FILE_APPEND_DATA,
            OPEN_EXISTING,
            "new reopen FILE_APPEND_DATA",
        ),
    ] {
        assert_eq!(
            probe_file(path, access, disposition, label),
            (true, true),
            "{label} failed"
        );
    }
    drop(revert);
    unsafe {
        CloseHandle(impersonation_token);
        CloseHandle(primary_token);
    }

    let termination = child.resume_and_wait_for_exit(&job, 5_000).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    stdout_pipe.read_to_end(&mut stdout).unwrap();
    stderr_pipe.read_to_end(&mut stderr).unwrap();
    eprintln!(
        "actual cmd child termination={termination:?}; stdout={:?}; stderr={:?}; file={:?}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
        std::fs::read_to_string(&existing).unwrap_or_else(|error| format!("READ_ERROR: {error}"))
    );
    assert_eq!(
        termination,
        0,
        "cmd.exe write failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap().trim(),
        "TAPID_FROM_CMD"
    );
    drop(child);
    drop(job);
    grants.restore().unwrap();
    let restored_acls = audit_paths
        .iter()
        .map(|path| query_acl(&icacls, path))
        .collect::<Vec<_>>();
    for ((path, before), restored) in audit_paths.iter().zip(&baseline_acls).zip(&restored_acls) {
        assert_eq!(
            before,
            restored,
            "DACL was not restored for {}",
            path.display()
        );
    }
    container.cleanup().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn appcontainer_node_ordinary_project_write_mutation_contract() {
    ordinary_project_write_contract(false);
}

#[test]
fn appcontainer_node_write_only_grant_denies_read() {
    ordinary_project_write_contract(true);
}

fn ordinary_project_write_contract(write_only: bool) {
    use super::super::windows_environment_block_units;
    use super::filesystem::windows_acl::{initialize_inheritance, read_acl};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, TOKEN_MANDATORY_LABEL, TokenIntegrityLevel,
    };

    fn run_node(
        container: &WindowsAppContainer,
        node: &std::path::Path,
        root: &std::path::Path,
        script: &str,
    ) -> (WindowsChildTermination, Vec<u8>, Vec<u8>) {
        let program: Vec<u16> = node.as_os_str().encode_wide().collect();
        let arguments: Vec<Vec<u16>> = [
            "--preserve-symlinks",
            "--preserve-symlinks-main",
            "-e",
            script,
        ]
        .iter()
        .map(|arg| arg.encode_utf16().collect())
        .collect();
        let command =
            super::super::serialize_windows_command_line_units(&program, &arguments, false)
                .unwrap();
        let environment =
            windows_environment_block_units(&std::collections::BTreeMap::new()).unwrap();
        let application: Vec<u16> = program.into_iter().chain(Some(0)).collect();
        let cwd: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
        let limits =
            ExecutionLimits::new(Some(10), Some(16384), Some(8), Some(512 * 1024 * 1024)).unwrap();
        let job = WindowsJob::new(&limits, false).unwrap();
        let mut pipes = WindowsStdioPipes::new().unwrap();
        let mut child = WindowsSuspendedChild::create_with_stdio(
            container,
            &application,
            &command,
            &environment,
            &cwd,
            &mut pipes,
        )
        .unwrap();
        job.assign_suspended_process(child.process_handle())
            .unwrap();
        let mut token = 0;
        assert_ne!(
            unsafe { OpenProcessToken(child.process_handle(), TOKEN_QUERY, &mut token) },
            0
        );
        let mut required = 0;
        unsafe { GetTokenInformation(token, TokenIntegrityLevel, null_mut(), 0, &mut required) };
        // usize storage gives TOKEN_MANDATORY_LABEL proper native alignment.
        let mut storage = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
        assert_ne!(
            unsafe {
                GetTokenInformation(
                    token,
                    TokenIntegrityLevel,
                    storage.as_mut_ptr().cast(),
                    required,
                    &mut required,
                )
            },
            0
        );
        let label = unsafe { &*storage.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
        let sid = label.Label.Sid;
        let rid = unsafe { *GetSidSubAuthority(sid, u32::from(*GetSidSubAuthorityCount(sid)) - 1) };
        unsafe { CloseHandle(token) };
        eprintln!("actual verified AppContainer token integrity RID={rid}");
        assert_eq!(rid, 4096, "expected low-integrity AppContainer token");
        let capture =
            WindowsOutputCapture::start(pipes.into_parent_readers().unwrap(), Some(16384));
        let termination = child
            .resume_and_wait_for_status(&job, 10000, capture.output_limit_exceeded(), None)
            .unwrap();
        let (stdout, stderr) = capture.finish().unwrap();
        job.wait_for_active_process_zero_notification(5000).unwrap();
        drop(child);
        drop(job);
        (termination, stdout, stderr)
    }

    let node_source = std::env::var_os("TAPID_TEST_NODE")
        .expect("native write acceptance requires TAPID_TEST_NODE");
    let project = tapid_test_support::TempProject::new("ordinary-write-contract").unwrap();
    let runtime = tapid_test_support::TempProject::new("ordinary-write-runtime").unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    let runtime_root = std::fs::canonicalize(runtime.path()).unwrap();
    let node = runtime_root.join("node.exe");
    std::fs::copy(node_source, &node).unwrap();
    let writable = root.join("writable");
    let sibling = root.join("sibling");
    std::fs::create_dir(&writable).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    let existing = writable.join("existing.txt");
    std::fs::write(&existing, b"preexisting content").unwrap();
    let exact = sibling.join("exact.txt");
    std::fs::write(&exact, b"exact before").unwrap();
    // Only disposable inheritance bookkeeping is initialized; mandatory labels stay untouched.
    initialize_inheritance(&root);
    initialize_inheritance(&runtime_root);
    let paths = [
        root.clone(),
        writable.clone(),
        existing.clone(),
        sibling.clone(),
        root.parent().unwrap().to_path_buf(),
        runtime_root.clone(),
        node.clone(),
        exact.clone(),
    ];
    let before: Vec<_> = paths.iter().map(|p| read_acl(p)).collect();
    let mut container = WindowsAppContainer::create().unwrap();
    let mut runtime_grant = WindowsFilesystemGrants::apply(
        &container,
        &[ResolvedFilesystemGrant {
            path: runtime_root.clone(),
            access: FilesystemAccess::Read,
            kind: FilesystemGrantKind::DirectorySubtree,
            source: super::super::FilesystemGrantSource::BackendRuntime,
            binding: super::super::FilesystemBindingMode::CanonicalPath,
        }],
    )
    .unwrap();
    // Explicit metadata/traversal grant is independent of file-content authority.
    let mut read_grant = WindowsPathAcl::grant(
        &root,
        container.sid(),
        if write_only {
            FilesystemAccess::ReadMetadata
        } else {
            FilesystemAccess::Read
        },
        FilesystemGrantKind::DirectorySubtree,
    )
    .unwrap();
    let sibling_read_acl = read_acl(&sibling);
    let mut write_grant = WindowsPathAcl::grant(
        &writable,
        container.sid(),
        FilesystemAccess::Write,
        FilesystemGrantKind::DirectorySubtree,
    )
    .unwrap();
    assert_eq!(
        read_acl(&sibling),
        sibling_read_acl,
        "write grant changed sibling"
    );
    assert_eq!(
        read_acl(root.parent().unwrap()),
        before[4],
        "shared ancestor changed"
    );
    let icacls = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("icacls.exe");
    for path in [&writable, &existing] {
        let listing = std::process::Command::new(&icacls)
            .arg(path)
            .output()
            .unwrap();
        assert!(listing.status.success());
        eprintln!(
            "ordinary target (no label changes) {}: {}",
            path.display(),
            String::from_utf8_lossy(&listing.stdout)
        );
    }
    let mut exact_grant = WindowsPathAcl::grant(
        &exact,
        container.sid(),
        FilesystemAccess::Write,
        FilesystemGrantKind::ExactFile,
    )
    .unwrap();
    // Inspect only this execution's allow ACEs, not the owner's preexisting full-control ACE.
    let sid_length =
        unsafe { windows_sys::Win32::Security::GetLengthSid(container.sid()) } as usize;
    let sid_bytes = unsafe { std::slice::from_raw_parts(container.sid().cast::<u8>(), sid_length) };
    for (path, expect_delete) in [(&writable, false), (&existing, true), (&exact, false)] {
        let (_, acl) = read_acl(path);
        let count = u16::from_le_bytes([acl[4], acl[5]]);
        let mut offset = 8;
        let mut effective = 0u32;
        for _ in 0..count {
            let size = u16::from_le_bytes([acl[offset + 2], acl[offset + 3]]) as usize;
            let ace = &acl[offset..offset + size];
            if ace[0] == 0 && ace.len() == 8 + sid_length && &ace[8..] == sid_bytes {
                let mask = u32::from_le_bytes(ace[4..8].try_into().unwrap());
                assert_eq!(
                    mask & (0x00040000 | 0x00080000 | 0x40),
                    0,
                    "grant must not add WRITE_DAC, WRITE_OWNER or FILE_DELETE_CHILD"
                );
                if u32::from(ace[1]) & windows_sys::Win32::Security::INHERIT_ONLY_ACE == 0 {
                    effective |= mask;
                }
            }
            offset += size;
        }
        assert_eq!(
            effective & windows_sys::Win32::Storage::FileSystem::DELETE != 0,
            expect_delete,
            "DELETE scope: {}",
            path.display()
        );
        if write_only && path != &writable {
            assert_eq!(
                effective & windows_sys::Win32::Storage::FileSystem::FILE_READ_DATA,
                0
            );
        }
    }
    eprintln!(
        "native ACE masks verified: descendant DELETE only; ExactFile/root/authority bits unchanged"
    );
    let mutation_script = r#"
const fs = require('node:fs');
function denied(f) {try {f();} catch(e) {if(['EPERM','EACCES'].includes(e.code)) return; throw e;} throw Error('outside authority succeeded');}
denied(()=>fs.writeFileSync('sibling/outside.txt','forbidden'));
denied(()=>fs.renameSync('writable','moved-root'));
fs.writeFileSync('sibling/exact.txt', 'exact overwrite');
denied(()=>fs.renameSync('sibling/exact.txt','writable/exact-moved.txt'));
fs.writeFileSync('writable/exact-replacement.txt','forbidden replacement');
denied(()=>fs.renameSync('writable/exact-replacement.txt','sibling/exact.txt'));
fs.unlinkSync('writable/exact-replacement.txt');
console.log('ROOT_AND_SIBLING_DENIED');
console.log('BEFORE_EXISTING_OVERWRITE');
fs.writeFileSync('writable/existing.txt', 'overwrite');
console.log('AFTER_EXISTING_OVERWRITE');
fs.appendFileSync('writable/existing.txt', '+append');
fs.truncateSync('writable/existing.txt', 4);
fs.mkdirSync('writable/nested');
fs.writeFileSync('writable/nested/new.txt', 'created');
if (fs.readFileSync('writable/nested/new.txt', 'utf8') !== 'created') throw Error('reopen');
fs.renameSync('writable/nested/new.txt', 'writable/nested/renamed.txt');
fs.writeFileSync('writable/replacement.txt', 'replacement');
fs.renameSync('writable/replacement.txt', 'writable/existing.txt');
fs.unlinkSync('writable/nested/renamed.txt');
fs.rmdirSync('writable/nested');
console.log('MUTATION_CONTRACT_COMPLETE');
"#;
    let write_only_script = r#"
const fs = require('node:fs');
fs.writeFileSync('writable/existing.txt', 'write-only');
fs.appendFileSync('writable/existing.txt', '+append');
try {fs.readFileSync('writable/existing.txt'); throw Error('write-only read succeeded');}
catch(e) {if(!['EPERM','EACCES'].includes(e.code)) throw e; console.log('WRITE_ONLY_READ_DENIED');}
"#;
    let script = if write_only {
        write_only_script
    } else {
        mutation_script
    };
    // Prove the identical nontruncating, write-only open succeeds before revocation.
    let script = format!(
        "const probeFs=require('node:fs'); probeFs.closeSync(probeFs.openSync('writable/existing.txt',probeFs.constants.O_WRONLY)); console.log('WRITE_OPEN_BEFORE_REVOCATION');\n{script}"
    );
    let (termination, stdout, stderr) = run_node(&container, &node, &root, &script);
    eprintln!(
        "ordinary Node result={termination:?}; stdout={}; stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    write_grant.restore().unwrap();
    exact_grant.restore().unwrap();
    // Revoke only writes: retain the independently declared metadata/traversal (or read)
    // grant so denial of O_WRONLY cannot be mistaken for denial of read access.
    // The same SID's runtime authority also remains alive until the Job is empty.
    // The write-only payload has already proved content reads are denied. Node's stat
    // also needs more than our ReadMetadata grant on this target, so supply independent
    // read authority only for the revocation control; never reinstate write authority.
    let mut revocation_read = if write_only {
        Some(
            WindowsPathAcl::grant(
                &root,
                container.sid(),
                FilesystemAccess::Read,
                FilesystemGrantKind::DirectorySubtree,
            )
            .unwrap(),
        )
    } else {
        None
    };
    let revoked = r#"const fs=require('node:fs'); fs.statSync('writable/existing.txt'); fs.readFileSync('writable/existing.txt'); console.log('SAME_SID_METADATA_RETAINED'); try {fs.closeSync(fs.openSync('writable/existing.txt',fs.constants.O_WRONLY)); throw Error('revoked write succeeded');} catch(e) {if(!['EPERM','EACCES'].includes(e.code)) throw e; console.log('SAME_SID_REVOKED');}"#;
    let (revoked_status, revoked_out, revoked_err) = run_node(&container, &node, &root, revoked);
    if let Some(grant) = &mut revocation_read {
        grant.restore().unwrap();
    }
    read_grant.restore().unwrap();
    runtime_grant.restore().unwrap();
    for (path, baseline) in paths.iter().zip(&before) {
        let restored = read_acl(path);
        eprintln!(
            "DACL_RECEIPT path={} before_control={:#06x} before_acl={:02x?} after_control={:#06x} after_acl={:02x?}",
            path.display(),
            baseline.0,
            baseline.1,
            restored.0,
            restored.1
        );
        assert_eq!(
            &restored,
            baseline,
            "exact DACL/control restoration: {}",
            path.display()
        );
    }
    container.cleanup().unwrap();
    eprintln!(
        "exact DACL/control restoration verified; same-SID result={revoked_status:?}; stdout={}; stderr={}",
        String::from_utf8_lossy(&revoked_out),
        String::from_utf8_lossy(&revoked_err)
    );
    assert_eq!(revoked_status, WindowsChildTermination::Exited(0));
    assert!(String::from_utf8_lossy(&stdout).contains("WRITE_OPEN_BEFORE_REVOCATION"));
    assert!(String::from_utf8_lossy(&revoked_out).contains("SAME_SID_METADATA_RETAINED"));
    assert!(String::from_utf8_lossy(&revoked_out).contains("SAME_SID_REVOKED"));
    assert_eq!(
        termination,
        WindowsChildTermination::Exited(0),
        "ordinary write/mutation contract failed; do not lower integrity labels to satisfy acceptance"
    );
    if write_only {
        assert_eq!(std::fs::read(&existing).unwrap(), b"write-only+append");
        assert!(String::from_utf8_lossy(&stdout).contains("WRITE_ONLY_READ_DENIED"));
    } else {
        assert_eq!(std::fs::read(&existing).unwrap(), b"replacement");
        assert!(String::from_utf8_lossy(&stdout).contains("MUTATION_CONTRACT_COMPLETE"));
        assert!(String::from_utf8_lossy(&stdout).contains("ROOT_AND_SIBLING_DENIED"));
        assert!(!sibling.join("outside.txt").exists());
        assert!(!root.join("moved-root").exists());
        assert!(!writable.join("nested").exists());
    }
}

#[test]
fn appcontainer_child_is_verified_and_assigned_before_resume() {
    let (mut container, job, mut child) = create_appcontainer_child("exit 0");
    assert_eq!(child.resume_and_wait_for_exit(&job, 5_000).unwrap(), 0);
    drop(child);
    drop(job);
    container.cleanup().unwrap();
}

#[test]
fn appcontainer_child_captures_stdout_and_stderr_separately() {
    use std::io::Read;

    let (mut container, job, mut child, pipes) =
        create_appcontainer_child_with_stdio("echo TAPID_OUT & echo TAPID_ERR 1>&2");
    let (mut stdout_pipe, mut stderr_pipe) = pipes.into_parent_readers().unwrap();
    assert_eq!(child.resume_and_wait_for_exit(&job, 5_000).unwrap(), 0);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    stdout_pipe.read_to_end(&mut stdout).unwrap();
    stderr_pipe.read_to_end(&mut stderr).unwrap();
    drop(child);
    drop(job);
    container.cleanup().unwrap();
    assert_eq!(String::from_utf8_lossy(&stdout).trim(), "TAPID_OUT");
    assert_eq!(String::from_utf8_lossy(&stderr).trim(), "TAPID_ERR");
}

#[test]
fn appcontainer_output_limit_terminates_and_bounds_combined_capture() {
    let (mut container, job, mut child, pipes) = create_appcontainer_child_with_stdio(
        "for /L %i in (1,1,1000000) do @echo TAPID_OUTPUT_CHUNK",
    );
    let capture = WindowsOutputCapture::start(pipes.into_parent_readers().unwrap(), Some(128));
    let termination = child
        .resume_and_wait_for_status(&job, 5_000, capture.output_limit_exceeded(), None)
        .unwrap();
    let (stdout, stderr) = capture.finish().unwrap();
    drop(child);
    drop(job);
    container.cleanup().unwrap();
    assert_eq!(termination, WindowsChildTermination::OutputLimitExceeded);
    assert!(stdout.len() + stderr.len() <= 128);
}

#[test]
fn appcontainer_timeout_terminates_and_reaps_the_job() {
    let (mut container, job, mut child) =
        create_appcontainer_child("for /L %i in (1,1,10000000) do @set /a x=%i >nul");
    let error = child.resume_and_wait_for_exit(&job, 100).unwrap_err();
    assert_eq!(error.category(), ExecutionErrorCategory::Timeout);
    assert!(child.wait_for_signal(5_000).unwrap());
    drop(child);
    drop(job);
    container.cleanup().unwrap();
}

#[test]
fn explicit_job_termination_signals_a_suspended_assigned_child() {
    let (mut container, job, child) = create_appcontainer_child("exit 0");
    job.terminate_all().unwrap();
    assert!(child.wait_for_signal(5_000).unwrap());
    drop(child);
    drop(job);
    container.cleanup().unwrap();
}

#[test]
fn job_completion_port_reports_when_the_last_member_exits() {
    let (mut container, job, child) = create_appcontainer_child("exit 0");
    job.terminate_all().unwrap();
    assert!(child.wait_for_signal(5_000).unwrap());
    job.wait_for_active_process_zero_notification(5_000)
        .unwrap();
    drop(child);
    drop(job);
    container.cleanup().unwrap();
}

#[test]
fn restoring_appcontainer_grant_preserves_concurrent_dacl_changes() {
    let root = std::env::temp_dir().join(format!(
        "tapid-appcontainer-concurrent-dacl-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let mut container = WindowsAppContainer::create().unwrap();
    let mut grant = WindowsPathAcl::grant(
        &root,
        container.sid(),
        FilesystemAccess::Write,
        FilesystemGrantKind::DirectorySubtree,
    )
    .unwrap();

    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let icacls =
        std::fs::canonicalize(std::path::PathBuf::from(system_root).join("System32/icacls.exe"))
            .unwrap();
    // BUILTIN\\Remote Desktop Users is a valid, distinct trustee that is not
    // inherited by this temporary directory. The numeric form avoids localization.
    let concurrent_sid = "*S-1-5-32-555";
    let concurrent_ace = format!("{concurrent_sid}:(OI)(CI)(R)");
    let change = std::process::Command::new(&icacls)
        .arg(&root)
        .arg("/grant")
        .arg(&concurrent_ace)
        .output()
        .unwrap();
    assert!(
        change.status.success(),
        "icacls concurrent DACL update failed: {}{}",
        String::from_utf8_lossy(&change.stdout),
        String::from_utf8_lossy(&change.stderr)
    );

    grant.restore().unwrap();
    let saved_acl = std::env::temp_dir().join(format!(
        "tapid-appcontainer-concurrent-dacl-{}.acl",
        std::process::id()
    ));
    let saved = std::process::Command::new(&icacls)
        .arg(&root)
        .arg("/save")
        .arg(&saved_acl)
        .arg("/c")
        .output()
        .unwrap();
    assert!(saved.status.success());
    let bytes = std::fs::read(&saved_acl).unwrap();
    let utf16 = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect::<Vec<_>>();
    let acl = String::from_utf16_lossy(&utf16);
    assert!(
        acl.contains(";;;RD)"),
        "restoring Tapid's ACE discarded the concurrent ACE: {acl}"
    );
    std::fs::remove_file(saved_acl).unwrap();
    container.cleanup().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn appcontainer_child_can_write_a_granted_tree_and_loses_access_on_restore() {
    let root = std::env::temp_dir().join(format!(
        "tapid-appcontainer-grant-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let marker = root.join("allowed.txt");
    let mut container = WindowsAppContainer::create().unwrap();
    let mut grant = WindowsPathAcl::grant(
        &root,
        container.sid(),
        FilesystemAccess::Write,
        FilesystemGrantKind::DirectorySubtree,
    )
    .unwrap();

    let payload = format!("echo authorized>\"{}\"", marker.display());
    let (job, mut child) = create_appcontainer_child_in(&container, &payload);
    assert_eq!(child.resume_and_wait_for_exit(&job, 5_000).unwrap(), 0);
    drop(child);
    drop(job);
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "authorized"
    );

    let payload = format!("type \"{}\" >nul", marker.display());
    let (job, mut child) = create_appcontainer_child_in(&container, &payload);
    assert_ne!(
        child.resume_and_wait_for_exit(&job, 5_000).unwrap(),
        0,
        "write-only grants must not permit reading the granted subtree"
    );
    drop(child);
    drop(job);

    grant.restore().unwrap();
    let payload = format!("echo denied>\"{}\"", marker.display());
    let (job, mut child) = create_appcontainer_child_in(&container, &payload);
    assert_ne!(child.resume_and_wait_for_exit(&job, 5_000).unwrap(), 0);
    drop(child);
    drop(job);
    container.cleanup().unwrap();
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "authorized"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn appcontainer_policy_only_allows_declared_write_subtree() {
    use super::super::{FilesystemBindingMode, FilesystemGrantSource, ResolvedFilesystemGrant};

    let root = std::env::temp_dir().join(format!(
        "tapid-appcontainer-policy-split-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let writable = root.join("writable");
    std::fs::create_dir_all(&writable).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let writable = std::fs::canonicalize(&writable).unwrap();
    let readonly_file = root.join("read-only.txt");
    let allowed_file = writable.join("allowed.txt");
    std::fs::write(&readonly_file, b"read-only baseline").unwrap();
    let mut container = WindowsAppContainer::create().unwrap();
    let resolved_grants = [
        ResolvedFilesystemGrant {
            path: root.clone(),
            access: FilesystemAccess::Read,
            kind: FilesystemGrantKind::DirectorySubtree,
            source: FilesystemGrantSource::ProjectPolicy,
            binding: FilesystemBindingMode::CanonicalPath,
        },
        ResolvedFilesystemGrant {
            path: writable,
            access: FilesystemAccess::Write,
            kind: FilesystemGrantKind::DirectorySubtree,
            source: FilesystemGrantSource::ProjectPolicy,
            binding: FilesystemBindingMode::CanonicalPath,
        },
    ];
    let mut grants = WindowsFilesystemGrants::apply(&container, &resolved_grants).unwrap();

    let payload = format!("echo authorized>\"{}\"", allowed_file.display());
    let (job, mut child) = create_appcontainer_child_in(&container, &payload);
    let allowed_exit = child.resume_and_wait_for_exit(&job, 5_000).unwrap();
    drop(child);
    drop(job);

    let payload = format!("echo unauthorized>\"{}\"", readonly_file.display());
    let (job, mut child) = create_appcontainer_child_in(&container, &payload);
    let denied_exit = child.resume_and_wait_for_exit(&job, 5_000).unwrap();
    drop(child);
    drop(job);
    let allowed_contents = std::fs::read_to_string(&allowed_file).unwrap();
    let readonly_contents = std::fs::read(&readonly_file).unwrap();

    grants.restore().unwrap();
    container.cleanup().unwrap();
    std::fs::remove_dir_all(root).unwrap();

    assert_eq!(allowed_exit, 0, "declared write subtree must be writable");
    assert_eq!(allowed_contents.trim(), "authorized");
    assert_eq!(
        readonly_contents, b"read-only baseline",
        "write to read-only project sibling changed the file (cmd exit {denied_exit})"
    );
}

#[test]
fn appcontainer_child_cannot_write_an_ungranted_temp_file() {
    let marker =
        std::env::temp_dir().join(format!("tapid-appcontainer-{}.txt", std::process::id()));
    std::fs::write(&marker, b"parent-owned").unwrap();
    let payload = format!("echo child>\"{}\" & exit 42", marker.display());
    let (mut container, job, mut child) = create_appcontainer_child(&payload);
    let exit_code = child.resume_and_wait_for_exit(&job, 5_000).unwrap();
    let contents = std::fs::read(&marker).unwrap();
    drop(child);
    drop(job);
    container.cleanup().unwrap();
    std::fs::remove_file(marker).unwrap();
    assert_ne!(
        exit_code, 0,
        "redirection to the parent-owned marker must be denied"
    );
    assert_eq!(contents, b"parent-owned");
}

fn create_appcontainer_child(
    payload: &str,
) -> (WindowsAppContainer, WindowsJob, WindowsSuspendedChild) {
    let container = WindowsAppContainer::create().unwrap();
    let (job, child) = create_appcontainer_child_in(&container, payload);
    (container, job, child)
}

fn create_appcontainer_child_with_stdio(
    payload: &str,
) -> (
    WindowsAppContainer,
    WindowsJob,
    WindowsSuspendedChild,
    WindowsStdioPipes,
) {
    let container = WindowsAppContainer::create().unwrap();
    let mut pipes = WindowsStdioPipes::new().unwrap();
    let (job, child) = create_appcontainer_child_inner(&container, payload, Some(&mut pipes));
    (container, job, child, pipes)
}

fn create_appcontainer_child_in(
    container: &WindowsAppContainer,
    payload: &str,
) -> (WindowsJob, WindowsSuspendedChild) {
    create_appcontainer_child_inner(container, payload, None)
}

fn create_appcontainer_child_inner(
    container: &WindowsAppContainer,
    payload: &str,
    stdio: Option<&mut WindowsStdioPipes>,
) -> (WindowsJob, WindowsSuspendedChild) {
    let current_directory = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let limits = ExecutionLimits::new(None, None, None, None).unwrap();
    create_appcontainer_child_inner_with_cwd(
        container,
        payload,
        &current_directory,
        &limits,
        false,
        false,
        stdio,
    )
}

fn create_appcontainer_child_inner_with_cwd(
    container: &WindowsAppContainer,
    payload: &str,
    current_directory: &std::path::Path,
    limits: &ExecutionLimits,
    restrict_subprocesses: bool,
    canonicalize_program: bool,
    stdio: Option<&mut WindowsStdioPipes>,
) -> (WindowsJob, WindowsSuspendedChild) {
    use std::os::windows::ffi::OsStrExt;

    let system_root = std::env::var_os("SystemRoot").unwrap();
    let system_directory = std::path::PathBuf::from(&system_root);
    let program_path = system_directory.join("System32").join("cmd.exe");
    let program_path = if canonicalize_program {
        std::fs::canonicalize(program_path).unwrap()
    } else {
        program_path
    };
    let current_directory = current_directory.to_path_buf();
    let program = program_path.as_os_str().encode_wide().collect::<Vec<_>>();
    let arguments = ["/D", "/S", "/C", payload]
        .into_iter()
        .map(|argument| argument.encode_utf16().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let command_line =
        super::super::serialize_windows_command_line_units(&program, &arguments, true).unwrap();
    let environment =
        super::super::windows_environment_block_units(&std::collections::BTreeMap::from([(
            std::ffi::OsString::from("SystemRoot"),
            system_root.clone(),
        )]))
        .unwrap();
    let mut application = program;
    application.push(0);
    let mut working_directory = current_directory
        .as_os_str()
        .encode_wide()
        .collect::<Vec<_>>();
    working_directory.push(0);

    let job = WindowsJob::new(limits, restrict_subprocesses).unwrap();
    let child = match stdio {
        Some(pipes) => WindowsSuspendedChild::create_with_stdio(
            container,
            &application,
            &command_line,
            &environment,
            &working_directory,
            pipes,
        ),
        None => WindowsSuspendedChild::create(
            container,
            &application,
            &command_line,
            &environment,
            &working_directory,
        ),
    }
    .unwrap();
    job.assign_suspended_process(child.process_handle())
        .unwrap();
    (job, child)
}

#[test]
fn disabled_subprocess_policy_caps_the_job_to_its_root_process() {
    let job = WindowsJob::new(&ExecutionLimits::default(), true).unwrap();
    let information = job.query_extended_limits().unwrap();
    assert_eq!(information.BasicLimitInformation.ActiveProcessLimit, 1);
    assert_eq!(
        information.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS
    );
}

#[test]
fn job_limits_are_job_wide_and_kill_members_when_owner_closes() {
    const PROCESS_LIMIT: u32 = 4;
    const MEMORY_LIMIT: u64 = 64 * 1024 * 1024;
    let limits = ExecutionLimits::new(None, None, Some(PROCESS_LIMIT), Some(MEMORY_LIMIT)).unwrap();
    let job = WindowsJob::new(&limits, false).unwrap();
    let information = job.query_extended_limits().unwrap();

    assert_eq!(
        information.BasicLimitInformation.ActiveProcessLimit,
        PROCESS_LIMIT
    );
    assert_eq!(
        information.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS
    );
    assert_eq!(
        information.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_MEMORY,
        JOB_OBJECT_LIMIT_JOB_MEMORY
    );
    assert_eq!(
        information.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    );
    assert_eq!(information.JobMemoryLimit, MEMORY_LIMIT as usize);
    assert_eq!(information.ProcessMemoryLimit, 0);
}
