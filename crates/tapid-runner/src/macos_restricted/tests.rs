use super::*;
use crate::config::{ExecutionLimits, FilesystemPolicy, SandboxMode, SandboxPolicy};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

fn temp_project(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "tapid-seatbelt-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&path).unwrap();
    fs::canonicalize(path).unwrap()
}

fn temp_node_runtime(label: &str, bytes: &[u8]) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let directory = temp_project(label);
    let runtime = directory.join("node");
    fs::write(&runtime, bytes).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    (directory, runtime)
}

fn policy(network: bool, subprocess: bool, environment: Vec<String>) -> SandboxPolicy {
    SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
        network,
        environment,
        subprocess,
        ExecutionLimits::default(),
    )
    .unwrap()
}

fn run_ruby(root: &Path, script: &str, network: bool, subprocess: bool) -> ExecutionOutcome {
    let request = ExecutionRequest::builder("/usr/bin/ruby")
        .args(["--disable-gems", "-e", script])
        .project_root(root)
        .policy(policy(network, subprocess, Vec::new()))
        .executable_search_path("/usr/bin")
        .build()
        .unwrap();
    super::super::execute(&request).unwrap()
}

#[test]
fn reserved_node_rejects_relative_and_aliased_project_temp_write_authority() {
    const CHILD: &str = "TAPID_TEST_PRIVATE_OVERLAP";
    if let Ok(mode) = std::env::var(CHILD) {
        let root = fs::canonicalize(".").unwrap();
        let project = if mode == "relative" {
            PathBuf::from(".")
        } else {
            PathBuf::from(root.to_string_lossy().replacen("/private/var/", "/var/", 1))
        };
        let request = ExecutionRequest::builder("/bin/sh")
            .args(["-c", ": > marker"])
            .project_root(project)
            .trusted_node_runtime(root.join("node"))
            .executable_search_path(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let result = super::super::execute(&request);
        assert!(
            result.is_err(),
            "overlapping private directory issued a receipt"
        );
        assert!(!root.join("marker").exists());
        assert_eq!(
            fs::read_dir(root.join("tmp")).unwrap().count(),
            0,
            "failed creation left a reserved directory"
        );
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let root = temp_project("private-overlap");
    fs::create_dir(root.join("tmp")).unwrap();
    fs::write(root.join("node"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(root.join("node"), fs::Permissions::from_mode(0o700)).unwrap();
    let mut failures = Vec::new();
    for mode in ["relative", "alias"] {
        let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "execution::platform_backend::tests::reserved_node_rejects_relative_and_aliased_project_temp_write_authority", "--nocapture"])
                .current_dir(&root).env(CHILD, mode).env("TMPDIR", root.join("tmp"))
                .output().unwrap();
        if !output.status.success() {
            failures.push(format!(
                "{mode}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let _ = fs::remove_file(root.join("marker"));
    }
    fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn surviving_descendant_keeps_reserved_node_after_root_receipt() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = temp_project("surviving-node");
    let (runtime_dir, runtime) = temp_node_runtime(
        "surviving-node-runtime",
        b"#!/bin/sh\nprintf verified > verified\n",
    );
    let local = root.join("node_modules/.bin");
    fs::create_dir_all(&local).unwrap();
    let hostile = local.join("node");
    fs::write(&hostile, "#!/bin/sh\nprintf hijacked > hijacked\n").unwrap();
    fs::set_permissions(&hostile, fs::Permissions::from_mode(0o700)).unwrap();
    // setsid escapes best-effort group cleanup. Redirect every stream so root
    // completion does not depend on the descendant closing an inherited pipe.
    let script = r#"
            directory = ENV.fetch('PATH').split(':').first
            Process.fork do
                Process.setsid
                STDIN.reopen('/dev/null')
                STDOUT.reopen('descendant.stdout', 'w')
                STDERR.reopen('descendant.stderr', 'w')
                File.write('ready', 'ready')
                deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 5
                sleep 0.01 until File.exist?('receipt-returned') || Process.clock_gettime(Process::CLOCK_MONOTONIC) > deadline
                deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 1
                sleep 0.01 while File.directory?(directory) && Process.clock_gettime(Process::CLOCK_MONOTONIC) < deadline
                File.write('binding-state', File.directory?(directory) ? 'retained' : 'removed')
                system('/bin/sh', '-c', 'node')
                File.write('done', 'done')
                exit! 0
            end
            deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 3
            sleep 0.01 until File.exist?('ready') || Process.clock_gettime(Process::CLOCK_MONOTONIC) > deadline
            abort 'descendant never ready' unless File.exist?('ready')
        "#;
    let request = ExecutionRequest::builder("/usr/bin/ruby")
        .args(["--disable-gems", "-e", script])
        .project_root(&root)
        .trusted_node_runtime(&runtime)
        .executable_search_paths([local, runtime_dir.clone(), PathBuf::from("/usr/bin")])
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let outcome = super::super::execute(&request).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    fs::write(root.join("receipt-returned"), "receipt").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !root.join("done").exists() && Instant::now() < deadline {
        thread::sleep(POLL_INTERVAL);
    }
    assert!(root.join("done").exists(), "descendant did not finish");
    let node = outcome
        .enforcement()
        .executable_resolution()
        .unwrap()
        .reserved_node
        .as_ref()
        .unwrap();
    let directory = Path::new(std::ffi::OsStr::from_bytes(&node.private_path))
        .parent()
        .unwrap();
    let hijacked = root.join("hijacked").exists();
    let verified = root.join("verified").exists();
    let state = fs::read_to_string(root.join("binding-state")).unwrap();
    let metadata = fs::metadata(directory).ok();
    // This test knows the descendant has finished using PATH. Production does not.
    let _ = fs::remove_dir_all(directory);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(runtime_dir).unwrap();
    assert!(
        !hijacked,
        "hostile node created hijacked marker despite receipt; binding={state}; cleanup_observed={}",
        node.cleanup_observed
    );
    assert!(verified);
    assert_eq!(state, "retained");
    assert!(!node.cleanup_observed);
    assert!(node.limitations.contains("retained"));
    let metadata = metadata.unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o700);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
}

#[test]
fn reserved_node_cleans_after_subprocess_denied_root_exit() {
    let root = temp_project("node-no-descendants");
    let (runtime_dir, runtime) = temp_node_runtime(
        "node-no-descendants-runtime",
        b"#!/bin/sh\nprintf verified\n",
    );
    let request = ExecutionRequest::builder("/bin/sh")
        .args(["-c", "exec node"])
        .project_root(&root)
        .trusted_node_runtime(&runtime)
        .executable_search_path(&runtime_dir)
        .policy(policy(false, false, Vec::new()))
        .build()
        .unwrap();
    let outcome = super::super::execute(&request).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert_eq!(outcome.stdout(), b"verified");
    let node = outcome
        .enforcement()
        .executable_resolution()
        .unwrap()
        .reserved_node
        .as_ref()
        .unwrap();
    assert!(node.cleanup_observed);
    assert!(node.limitations.contains("subprocess=false"));
    let directory = Path::new(std::ffi::OsStr::from_bytes(&node.private_path))
        .parent()
        .unwrap();
    assert!(!directory.exists());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(runtime_dir).unwrap();
}

#[test]
fn reserved_node_preserves_native_bytes_and_exact_empty_caller_path() {
    use std::os::unix::ffi::OsStringExt;
    let root = temp_project("node-bytes");
    let (runtime_dir, runtime) =
        temp_node_runtime("node-bytes-runtime", b"#!/bin/sh\nprintf '%s' \"$1\"");
    let local = root.join("local");
    fs::create_dir(&local).unwrap();
    let value = OsString::from_vec(b"f\x80o".to_vec());
    let request = ExecutionRequest::builder("/bin/sh")
        .args([
            OsString::from("-c"),
            OsString::from("node \"$1\""),
            OsString::from("fixture"),
            value.clone(),
        ])
        .trusted_node_runtime(&runtime)
        .executable_search_paths([local.clone(), runtime_dir.clone()])
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let outcome = super::super::execute(&request).unwrap();
    assert_eq!(outcome.stdout(), value.as_bytes());
    let resolution = outcome.enforcement().executable_resolution().unwrap();
    assert_eq!(resolution.path_order.len(), 3);
    assert_eq!(resolution.path_order[1], local.as_os_str().as_bytes());
    assert_eq!(resolution.path_order[2], runtime_dir.as_os_str().as_bytes());
    assert_eq!(resolution.path, resolution.path_order.join(&b':'));
    assert!(!resolution.caller_path_inherited);
    assert!(!resolution.reserved_node.as_ref().unwrap().cleanup_observed);
    fs::remove_dir_all(Path::new(std::ffi::OsStr::from_bytes(
        &resolution.path_order[0],
    )))
    .unwrap();
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(runtime_dir).unwrap();
}

#[test]
fn reserved_node_executes_snapshot_after_source_rename() {
    let root = temp_project("node-source-rename");
    let (runtime_dir, runtime) = temp_node_runtime(
        "node-source-rename-runtime",
        b"#!/bin/sh\nprintf captured-inode",
    );
    let mut request = ExecutionRequest::builder("/bin/sh")
        .args(["-c", "node"])
        .trusted_node_runtime(&runtime)
        .executable_search_path(&runtime_dir)
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let binding =
        ReservedNode::create(request.trusted_node_runtime.as_ref().unwrap(), &root).unwrap();
    request
        .executable_search_paths
        .insert(0, binding.evidence.directory.clone());
    request.reserved_node = Some(binding.evidence.clone());
    fs::rename(&runtime, runtime_dir.join("renamed-node")).unwrap();
    let outcome = super::super::execute_with_backend(&request, &PlatformBackend).unwrap();
    assert_eq!(outcome.stdout(), b"captured-inode");
    assert!(
        outcome
            .enforcement()
            .resolved_filesystem()
            .grants()
            .iter()
            .any(|g| g.path() == binding.evidence.directory.join("node")
                && g.source() == super::super::FilesystemGrantSource::BackendRuntime)
    );
    drop(binding);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(runtime_dir).unwrap();
}

#[test]
fn reserved_node_is_a_verified_snapshot_and_detects_tampering() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = temp_project("node-identity");
    let runtime = root.join("node");
    fs::write(&runtime, b"#!/bin/sh\nprintf trusted").unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let trusted = super::super::TrustedNodeRuntime::checked(&runtime).unwrap();
    let mut binding = ReservedNode::create(&trusted, &root).unwrap();
    let node = binding.evidence.directory.join("node");
    assert!(
        !fs::symlink_metadata(&node)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_ne!(
        fs::metadata(&node).unwrap().ino(),
        fs::metadata(&runtime).unwrap().ino()
    );
    assert_eq!(fs::read(&node).unwrap(), fs::read(&runtime).unwrap());
    assert_eq!(
        fs::metadata(&binding.evidence.directory).unwrap().mode() & 0o777,
        0o700
    );
    fs::rename(&runtime, root.join("old-node")).unwrap();
    fs::write(&runtime, "replacement").unwrap();
    binding.evidence.validate().unwrap();
    fs::remove_file(&node).unwrap();
    fs::write(&node, "tamper").unwrap();
    assert!(binding.evidence.validate().is_err());
    let directory = binding.evidence.directory.clone();
    assert!(binding.cleanup());
    assert!(!directory.exists());
    assert!(ReservedNode::create(&trusted, &root).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reserved_node_rejects_a_runtime_under_project_write_authority() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_project("node-runtime-write-authority");
    let runtime = root.join("node");
    fs::write(&runtime, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let request = ExecutionRequest::builder("/bin/sh")
        .args(["-c", ": > marker"])
        .trusted_node_runtime(&runtime)
        .executable_search_path(&root)
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let error = super::super::execute(&request)
        .expect_err("project-writable trusted runtime issued a receipt");
    assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
    assert!(
        error
            .to_string()
            .contains("trusted Node runtime overlaps project write authority")
    );
    assert!(!root.join("marker").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_hardlink_cannot_mutate_reserved_node_or_trusted_runtime() {
    use std::os::unix::{
        ffi::OsStringExt,
        fs::{MetadataExt, PermissionsExt},
    };
    let project = temp_project("node-hardlink-project");
    let trusted = temp_project("node-hardlink-trusted");
    let runtime = trusted.join("node");
    fs::write(
            &runtime,
            b"#!/bin/sh\ncase \"$*\" in *second-node*) printf verified > second-node ;; *detached-node*) printf verified > detached-node ;; esac\n",
        )
        .unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let original = fs::read(&runtime).unwrap();
    let script = r##"
            private_node = File.join(ENV.fetch('PATH').split(':').first, 'node')
            begin
              File.link(private_node, 'node-alias')
              File.write('node-alias', "#!/bin/sh\nprintf mutated > \"$TAPID_NODE_RESULT\"\n")
              File.chmod(0700, 'node-alias')
            rescue SystemCallError => error
              File.write('mutation-denied', error.class.name)
            end
            abort 'second node failed' unless system('node', '-e', "require('fs').writeFileSync('second-node', 'verified')")
            Process.fork do
              Process.setsid
              STDIN.reopen('/dev/null')
              STDOUT.reopen('descendant.stdout', 'w')
              STDERR.reopen('descendant.stderr', 'w')
              sleep 0.01 until File.exist?('receipt-returned')
              system('node', '-e', "require('fs').writeFileSync('detached-node', 'verified')")
              exit! 0
            end
            File.write('descendant-ready', 'ready')
        "##;
    let request = ExecutionRequest::builder("/usr/bin/ruby")
        .args(["--disable-gems", "-e", script])
        .trusted_node_runtime(&runtime)
        .executable_search_paths([
            trusted.clone(),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .project_root(&project)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let outcome = super::super::execute(&request).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert!(project.join("descendant-ready").exists());
    fs::write(project.join("receipt-returned"), b"receipt").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !project.join("detached-node").exists() && Instant::now() < deadline {
        thread::sleep(POLL_INTERVAL);
    }
    let resolution = outcome.enforcement().executable_resolution().unwrap();
    let private = PathBuf::from(OsString::from_vec(
        resolution
            .reserved_node
            .as_ref()
            .unwrap()
            .private_path
            .clone(),
    ));
    assert_eq!(
        fs::read(&runtime).unwrap(),
        original,
        "selected runtime mutated"
    );
    assert_eq!(fs::read(project.join("second-node")).unwrap(), b"verified");
    assert_eq!(
        fs::read(project.join("detached-node")).unwrap(),
        b"verified"
    );
    assert_eq!(
        fs::read(&private).unwrap(),
        original,
        "reserved binding mutated"
    );
    let private_metadata = fs::metadata(&private).unwrap();
    let runtime_metadata = fs::metadata(&runtime).unwrap();
    assert_ne!(
        (private_metadata.dev(), private_metadata.ino()),
        (runtime_metadata.dev(), runtime_metadata.ino()),
        "private snapshot shares the selected runtime inode"
    );
    assert_eq!(private_metadata.nlink(), 1);
    assert!(!project.join("node-alias").exists());
    assert_eq!(
        resolution.reserved_node.as_ref().unwrap().mechanism,
        "byte-verified private snapshot"
    );
    assert!(project.join("mutation-denied").exists());
    fs::remove_dir_all(private.parent().unwrap()).unwrap();
    fs::remove_dir_all(project).unwrap();
    fs::remove_dir_all(trusted).unwrap();
}

#[test]
fn private_helper_closes_unexpected_descriptors_before_target_exec() {
    let file = fs::File::open("/dev/null").unwrap();
    let raw = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
    assert!(raw >= 64);
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut command = Command::new(std::env::current_exe().unwrap());
    let confirmation = ExecConfirmation::prepare(&mut command).unwrap();
    command
        .arg("/bin/sh")
        .args(["-c", &format!("test ! -e /dev/fd/{raw}")])
        .env_clear()
        .env("PATH", "")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Simulate a launcher leaking a descriptor after the parent's initial sanitation.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(raw, libc::F_SETFD, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(command);
    confirmation.confirm(child.id(), None).unwrap();
    let status = child.wait().unwrap();
    drop(fd);
    assert!(
        status.success(),
        "private helper leaked an unexpected descriptor"
    );
}

#[test]
fn lowered_nofile_never_leaks_authority_through_helper_exec() {
    let root = temp_project("lowered-nofile");
    let file = fs::File::create(root.join("authority")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let connected = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    for source in [
        file.as_raw_fd(),
        listener.as_raw_fd(),
        connected.as_raw_fd(),
    ] {
        for helper_leak in [false, true] {
            let raw = 512;
            let mut command = Command::new(std::env::current_exe().unwrap());
            // Lower only the child limit, before production pre_exec sanitation.
            unsafe {
                command.pre_exec(move || {
                    let mut limit = std::mem::zeroed::<libc::rlimit>();
                    if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    limit.rlim_cur = limit.rlim_max.min(1024);
                    if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0
                        || libc::dup2(source, raw) != raw
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    limit.rlim_cur = 256;
                    if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0
                        || libc::fcntl(raw, libc::F_SETFD, 0) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let confirmation = ExecConfirmation::prepare(&mut command).unwrap();
            command
                .arg("/usr/bin/ruby")
                .args([
                    "--disable-gems",
                    "-e",
                    &format!("begin; IO.for_fd({raw}); exit 91; rescue Errno::EBADF; exit 0; end"),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if helper_leak {
                unsafe {
                    command.pre_exec(move || {
                        if libc::fcntl(raw, libc::F_SETFD, 0) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
            let mut child = command.spawn().unwrap();
            drop(command);
            confirmation.confirm(child.id(), None).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("descriptor target stalled");
                }
                thread::sleep(POLL_INTERVAL);
            };
            assert!(
                status.success(),
                "FD {raw} survived lowered limit; helper leak={helper_leak}, status={status}"
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn helper_dead_after_ready_before_go_cannot_confirm_exec() {
    let mut command = Command::new("/usr/bin/ruby");
    command.args([
        "--disable-gems",
        "-e",
        "IO.for_fd(3).syswrite([1,1].pack('NN') + [ARGV[1]].pack('H*') + [0,0].pack('NN'))",
        "--",
    ]);
    let confirmation = ExecConfirmation::prepare(&mut command).unwrap();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().unwrap();
    drop(command);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "READY writer failed");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("READY writer stalled");
        }
        thread::sleep(POLL_INTERVAL);
    }
    // The full READY is queued, but the child is already reaped before the
    // parent can register NOTE_EXEC or send GO. EOF cannot count as exec.
    assert!(confirmation.confirm(child.id(), None).is_err());
    assert!(Instant::now() < deadline);
}

#[test]
fn real_pipe_protocol_failures_have_no_receipt_and_reap_the_helper() {
    let root = temp_project("protocol-failures");
    let request = ExecutionRequest::builder("/bin/sh")
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let additions = PlatformBackend
        .runtime_filesystem_additions(&request)
        .unwrap();
    let policy = super::super::resolve_policy(&request, additions).unwrap();
    let preflight = ValidatedPreflight {
        support: containment_support(&request),
        bindings: FilesystemBindings::canonical_path(&policy).unwrap(),
        policy,
        child_environment: request.child_environment(),
    };
    for fault in [
        "valid",
        "nonce",
        "version",
        "short",
        "duplicate",
        "eof",
        "before-ready",
        "after-go",
        "death",
        "no-status-eof",
    ] {
        // A separate Ruby process speaks the actual pipe protocol. The retained
        // status FD case performs a real exec, so NOTE_EXEC alone cannot pass.
        let script = format!(
            r#"
                status = IO.for_fd(3); gate = IO.for_fd(4)
                status.close_on_exec = false
                nonce = [ARGV[1]].pack('H*')
                ready = [1, 1].pack('NN') + nonce + [0, 0].pack('NN')
                fault = {fault:?}
                sleep 30 if fault == 'before-ready'
                exit! 0 if fault == 'eof'
                ready.setbyte(8, ready.getbyte(8) ^ 1) if fault == 'nonce'
                ready.setbyte(3, 2) if fault == 'version'
                ready = ready[0, 31] if fault == 'short'
                status.syswrite(ready)
                exit! 0 if fault == 'short' || fault == 'death'
                status.syswrite(ready) if fault == 'duplicate'
                gate.read(32)
                sleep 30 if fault == 'after-go'
                status.close_on_exec = fault != 'no-status-eof'
                exec('/bin/sleep', fault == 'no-status-eof' ? '30' : '0.01')
            "#
        );
        let mut command = Command::new("/usr/bin/ruby");
        command.args(["--disable-gems", "-e", &script, "--"]);
        let confirmation = ExecConfirmation::prepare(&mut command).unwrap();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut lifecycle = MacosLifecycle {
            request: request.clone(),
            preflight: &preflight,
            launch: Some((command, confirmation)),
            child: None,
            process_group: None,
            cleanup_attempted: false,
            cleanup_observed: false,
            process_started: false,
        };
        let start = Instant::now();
        let result = lifecycle.execute();
        let pid = lifecycle.process_group.map(|group| group as u32);
        let cleanup = lifecycle.cleanup();
        if fault == "valid" {
            assert!(result.is_ok(), "valid real-pipe control failed: {result:?}");
        } else {
            assert!(result.is_err(), "{fault} issued a receipt");
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "{fault} was unbounded"
        );
        if fault != "valid" {
            let completion = cleanup
                .expect("fallback cleanup should succeed")
                .expect("a started child should yield completion evidence");
            assert_eq!(
                completion.cleanup_confidence(),
                CleanupConfidence::BestEffortObserved
            );
        }
        assert!(lifecycle.child.is_none());
        if let Some(pid) = pid {
            assert_eq!(
                unsafe { libc::kill(pid as i32, 0) },
                -1,
                "{fault} child survived"
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reserved_execution_failures_leave_no_receipt_or_private_directory() {
    const CHILD: &str = "TAPID_TEST_RESERVED_FAILURE";
    if std::env::var_os(CHILD).is_some() {
        let root = fs::canonicalize(".").unwrap();
        for fault in ["preparation", "exec", "protocol"] {
            let program = if fault == "protocol" {
                OsString::from("--test-death-after-ready")
            } else {
                root.join("missing-target").into_os_string()
            };
            let runtime = PathBuf::from(std::env::var_os("TAPID_TEST_RUNTIME").unwrap());
            let runtime_dir = runtime.parent().unwrap();
            let mut request = ExecutionRequest::builder(program)
                .project_root(&root)
                .trusted_node_runtime(&runtime)
                .executable_search_path(runtime_dir)
                .policy(policy(false, true, Vec::new()))
                .build()
                .unwrap();
            if fault == "preparation" {
                request.launcher = None;
            }
            let start = Instant::now();
            let result = super::super::execute(&request);
            assert!(result.is_err(), "{fault} issued a receipt");
            assert!(start.elapsed() < Duration::from_secs(8));
            if fault != "preparation" {
                assert_eq!(
                    result
                        .unwrap_err()
                        .completion()
                        .unwrap()
                        .cleanup_confidence(),
                    CleanupConfidence::BestEffortObserved
                );
            }
            assert_eq!(
                fs::read_dir(std::env::temp_dir()).unwrap().count(),
                0,
                "{fault} leaked reserved directory"
            );
        }
        return;
    }
    let root = temp_project("reserved-failure");
    let temporary = temp_project("reserved-failure-tmp");
    let (runtime_dir, runtime) =
        temp_node_runtime("reserved-failure-runtime", b"#!/bin/sh\nexit 0\n");
    let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "execution::platform_backend::tests::reserved_execution_failures_leave_no_receipt_or_private_directory", "--nocapture"])
            .current_dir(&root).env(CHILD, "1").env("TAPID_TEST_RUNTIME", &runtime).env("TMPDIR", &temporary).output().unwrap();
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(temporary).unwrap();
    fs::remove_dir_all(runtime_dir).unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reserved_snapshot_creation_failure_removes_private_directory() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_project("link-failure");
    let path = root.join("node");
    fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let runtime = super::super::TrustedNodeRuntime::checked(&path).unwrap();
    let mut created = None;
    let result = ReservedNode::create_with_copy(&runtime, &root, |_, target| {
        let directory = target.parent().unwrap().to_path_buf();
        assert!(directory.is_dir());
        created = Some(directory);
        Err(std::io::Error::from_raw_os_error(libc::EXDEV))
    });
    let error = result.err().expect("injected hard-link failure succeeded");
    assert!(
        error
            .to_string()
            .contains("cannot create verified reserved Node snapshot")
    );
    assert!(
        !created.unwrap().exists(),
        "creation failure leaked directory"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prepared_launch_revalidates_identities_and_cleans_failed_attempts() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = temp_project("prepared-tamper");
    let (runtime_dir, runtime) =
        temp_node_runtime("prepared-tamper-runtime", b"#!/bin/sh\nexit 0\n");
    for fault in ["helper", "binding", "spawn", "preparation"] {
        let mut request = ExecutionRequest::builder("/bin/sh")
            .args(["-c", ": > marker"])
            .project_root(&root)
            .trusted_node_runtime(&runtime)
            .executable_search_path(&runtime_dir)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let binding =
            ReservedNode::create(request.trusted_node_runtime.as_ref().unwrap(), &root).unwrap();
        let directory = binding.evidence.directory.clone();
        request.reserved_node = Some(binding.evidence.clone());
        request.executable_search_paths.insert(0, directory.clone());
        let helper_path = root.join("helper");
        fs::copy(std::env::current_exe().unwrap(), &helper_path).unwrap();
        let metadata = fs::metadata(&helper_path).unwrap();
        let helper = request.launcher.as_mut().unwrap();
        helper.executable = Some(helper_path.clone());
        helper.identity = Some((metadata.dev(), metadata.ino()));
        let additions = PlatformBackend
            .runtime_filesystem_additions(&request)
            .unwrap();
        let policy = super::super::resolve_policy(&request, additions).unwrap();
        let preflight = ValidatedPreflight {
            support: containment_support(&request),
            bindings: PlatformBackend.bind_filesystem(&request, &policy).unwrap(),
            policy,
            child_environment: request.child_environment(),
        };
        let profile = compile_profile(&request, &preflight).unwrap();
        if fault == "preparation" {
            fs::remove_file(&helper_path).unwrap();
            assert!(prepare_launch(&request, &preflight, &profile).is_err());
        } else {
            let mut launch = prepare_launch(&request, &preflight, &profile).unwrap();
            match fault {
                "helper" => {
                    fs::rename(&helper_path, root.join("old-helper")).unwrap();
                    fs::write(&helper_path, "#!/bin/sh\n: > marker\n").unwrap();
                    fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o700)).unwrap();
                }
                "binding" => {
                    fs::remove_file(directory.join("node")).unwrap();
                    fs::write(directory.join("node"), "replacement").unwrap();
                }
                "spawn" => {
                    launch.0.current_dir(root.join("absent"));
                }
                _ => unreachable!(),
            }
            let result = OwnedExecutionAttempt::new(
                &preflight,
                Box::new(MacosLifecycle {
                    request,
                    preflight: &preflight,
                    launch: Some(launch),
                    child: None,
                    process_group: None,
                    cleanup_attempted: false,
                    cleanup_observed: false,
                    process_started: false,
                }),
            )
            .finish();
            let error = result.expect_err("failed launch issued receipt");
            assert!(
                error.completion().is_none(),
                "pre-spawn failure fabricated completion evidence"
            );
        }
        assert!(!root.join("marker").exists());
        drop(binding);
        assert!(!directory.exists(), "{fault} left reserved directory");
        let _ = fs::remove_file(&helper_path);
        let _ = fs::remove_file(root.join("old-helper"));
    }
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(runtime_dir).unwrap();
}

#[test]
fn private_protocol_rejects_version_nonce_kind_and_short_frames() {
    let nonce = std::array::from_fn(|index| 7_u8.wrapping_add(index as u8));
    let ready = protocol_frame(1, nonce, 0);
    assert!(validate_frame(&ready, 1, nonce).is_ok());
    for index in [0, 4, 8, 24] {
        let mut malformed = ready;
        malformed[index] ^= 1;
        assert!(validate_frame(&malformed, 1, nonce).is_err());
    }
    assert!(validate_frame(&ready[..31], 1, nonce).is_err());
    assert!(validate_frame(&[ready.as_slice(), &[0]].concat(), 1, nonce).is_err());
}

#[test]
fn missing_early_initializer_fails_before_spawn() {
    let root = temp_project("missing-init");
    let mut request = ExecutionRequest::builder("/bin/sh")
        .args(["-c", "touch marker"])
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    request.launcher = None;
    let error = super::super::execute(&request).unwrap_err();
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert!(!root.join("marker").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn support_evidence_identifies_native_generated_policy_controls() {
    let root = temp_project("native-probes");
    let request = ExecutionRequest::builder("/bin/sh")
        .project_root(&root)
        .policy(policy(false, false, Vec::new()))
        .build()
        .unwrap();
    let support = containment_support(&request);
    assert!(support.is_supported(), "{support:?}");
    for evidence in support.observed_evidence() {
        assert!(
            evidence
                .mechanism()
                .contains("native generated deny-default"),
            "{evidence:?}"
        );
        assert!(
            evidence
                .limitations()
                .iter()
                .any(|text| text.contains("sampled"))
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_native_profile_fails_before_target_marker() {
    let root = temp_project("native-malformed");
    let marker = root.join("marker");
    let mut profile = probe_profile(&root, true).unwrap();
    profile.text = "(version 1)(not-a-seatbelt-operation)".into();
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "printf untrusted > \"$1\"", "target"])
        .arg(&marker);
    assert_eq!(
        profile.configure(&mut command).unwrap_err().category(),
        ExecutionErrorCategory::Spawn
    );
    let request = ExecutionRequest::builder("/bin/sh")
        .args([
            OsString::from("-c"),
            OsString::from("printf untrusted > \"$1\""),
            OsString::from("target"),
            marker.clone().into_os_string(),
        ])
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let additions = PlatformBackend
        .runtime_filesystem_additions(&request)
        .unwrap();
    let policy = super::super::resolve_policy(&request, additions).unwrap();
    let preflight = ValidatedPreflight {
        support: containment_support(&request),
        bindings: FilesystemBindings::canonical_path(&policy).unwrap(),
        policy,
        child_environment: request.child_environment(),
    };
    let launch = prepare_launch(&request, &preflight, &profile).unwrap();
    let result = OwnedExecutionAttempt::new(
        &preflight,
        Box::new(MacosLifecycle {
            request,
            preflight: &preflight,
            launch: Some(launch),
            child: None,
            process_group: None,
            cleanup_attempted: false,
            cleanup_observed: false,
            process_started: false,
        }),
    )
    .finish();
    assert_eq!(
        result.unwrap_err().category(),
        ExecutionErrorCategory::Spawn
    );
    assert!(!marker.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_execution_preserves_empty_path_and_non_utf8_arguments() {
    use std::os::unix::ffi::OsStringExt;
    let root = temp_project("native-bytes");
    let value = OsString::from_vec(vec![b'a', 0xff, b' ', b'\'']);
    let request = ExecutionRequest::builder("/bin/sh")
        .args([
            OsString::from("-c"),
            OsString::from("test -z \"$PATH\" && printf '%s' \"$1\""),
            OsString::from("target"),
            value.clone(),
        ])
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let outcome = super::super::execute(&request).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert_eq!(outcome.stdout(), value.as_bytes());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn death_after_profile_application_without_exec_never_issues_a_receipt() {
    let root = temp_project("preexec-death");
    let request = ExecutionRequest::builder("/bin/sh")
        .args(["-c", "exit 0"])
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let additions = PlatformBackend
        .runtime_filesystem_additions(&request)
        .unwrap();
    let policy = super::super::resolve_policy(&request, additions).unwrap();
    let bindings = FilesystemBindings::canonical_path(&policy).unwrap();
    let preflight = ValidatedPreflight {
        support: containment_support(&request),
        policy,
        bindings,
        child_environment: request.child_environment(),
    };
    let mut profile = compile_profile(&request, &preflight).unwrap();
    profile.abort_before_exec = true;
    let launch = prepare_launch(&request, &preflight, &profile).unwrap();
    let mut lifecycle = MacosLifecycle {
        request,
        preflight: &preflight,
        launch: Some(launch),
        child: None,
        process_group: None,
        cleanup_attempted: false,
        cleanup_observed: false,
        process_started: false,
    };
    let result = lifecycle.execute();
    lifecycle.cleanup().unwrap();
    assert!(result.is_err(), "death before exec produced a receipt");
    assert_eq!(
        result.unwrap_err().category(),
        ExecutionErrorCategory::Spawn
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn launch_confirmation_distinguishes_exec_failure_from_target_exit_71() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_project("exec-confirmation");
    let noexec = root.join("noexec");
    fs::write(&noexec, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&noexec, fs::Permissions::from_mode(0o600)).unwrap();
    for (target, errno) in [(root.join("missing"), libc::ENOENT), (noexec, libc::EACCES)] {
        let request = ExecutionRequest::builder(target)
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let result = super::super::execute(&request);
        assert!(result.is_err(), "exec failure issued receipt: {result:?}");
        let error = result.unwrap_err();
        assert_eq!(error.category(), ExecutionErrorCategory::Spawn);
        assert!(
            error.to_string().contains(&format!("errno {errno}:")),
            "{error}"
        );
    }
    for code in [0, 71] {
        let request = ExecutionRequest::builder("/bin/sh")
            .args([
                "-c",
                &format!("test ! -e /dev/fd/3 && test ! -e /dev/fd/4 || exit 99; exit {code}"),
            ])
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        assert_eq!(
            super::super::execute(&request).unwrap().termination(),
            &Termination::Exited(code)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn compiled_filesystem_rules_match_typed_effective_authority() {
    let root = temp_project("typed-authority");
    let request = ExecutionRequest::builder("/bin/sh")
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let additions = PlatformBackend
        .runtime_filesystem_additions(&request)
        .unwrap();
    let resolved = super::super::resolve_policy(&request, additions).unwrap();
    let bindings = FilesystemBindings::canonical_path(&resolved).unwrap();
    let receipt = bindings.receipt();
    let roots: Vec<_> = receipt
        .grants()
        .iter()
        .filter(|g| g.path() == Path::new("/"))
        .collect();
    assert_eq!(
        roots.len(),
        2,
        "root data and global metadata need distinct evidence"
    );
    assert_eq!(
        receipt
            .read()
            .filter(|grant| grant.path() == Path::new("/"))
            .count(),
        2,
        "read authority iterator must include data and metadata grants"
    );
    assert_eq!(
        (roots[0].access(), roots[0].kind()),
        (
            FilesystemAccess::ReadData,
            FilesystemGrantKind::ExactDirectory
        )
    );
    assert_eq!(
        (roots[1].access(), roots[1].kind()),
        (
            FilesystemAccess::ReadMetadata,
            FilesystemGrantKind::DirectorySubtree
        )
    );
    let preflight = ValidatedPreflight {
        support: containment_support(&request),
        policy: resolved,
        bindings,
        child_environment: request.child_environment(),
    };
    let compiled = compile_profile(&request, &preflight).unwrap();
    assert_eq!(
        compiled
            .text
            .lines()
            .filter(|line| line.starts_with("(allow file-"))
            .count(),
        receipt.grants().len() + 1
    );
    for (index, grant) in receipt.grants().iter().enumerate() {
        let operation = match grant.access() {
            FilesystemAccess::ReadData => "file-read-data",
            FilesystemAccess::ReadMetadata => "file-read-metadata",
            FilesystemAccess::Read => "file-read*",
            FilesystemAccess::Write => "file-write*",
        };
        let filter = if grant.kind() == FilesystemGrantKind::DirectorySubtree {
            "subpath"
        } else {
            "literal"
        };
        assert!(compiled.text.contains(&format!(
            "(allow {operation} ({filter} (param \"G{index}\")))"
        )));
        assert_eq!(compiled.parameters[index].1, grant.path().as_os_str());
    }
    assert!(
        receipt.grants().iter().any(|g| g.path() == root
            && g.source() == super::super::FilesystemGrantSource::ProjectPolicy)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn descriptor_sanitation_is_bounded_for_huge_limits_and_fails_closed_when_full() {
    const CHILD: &str = "TAPID_TEST_DESCRIPTOR_LIMIT_CHILD";
    if let Ok(mode) = std::env::var(CHILD) {
        let mut original: libc::rlimit = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) },
            0
        );
        let requested = match mode.as_str() {
            "infinity" => libc::RLIM_INFINITY,
            "full" => 1024,
            "huge" => 1_000_000_000,
            "lowered" => 1024,
            _ => unreachable!(),
        };
        let raised = libc::rlimit {
            // A process cannot raise its inherited hard limit. Exercise the
            // largest permitted value on constrained CI runners too.
            rlim_cur: requested.min(original.rlim_max),
            rlim_max: original.rlim_max,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) }, 0);
        let code = match mode.as_str() {
            "infinity" | "huge" => usize::from(sanitize_descriptors(false).is_err()),
            "full" => {
                // Exercise the same real enumeration/truncation path with
                // a smaller fixed buffer: macOS may cap open descriptors
                // below the production capacity even at RLIM_INFINITY.
                let files: Vec<_> = (0..64)
                    .map(|_| fs::File::open("/dev/null").unwrap())
                    .collect();
                let rejected = sanitize_descriptors_with_capacity::<32>(false)
                    .is_err_and(|error| error.raw_os_error() == Some(libc::EIO));
                drop(files);
                usize::from(!rejected)
            }
            "lowered" => {
                let file = fs::File::open("/etc/hosts").unwrap();
                assert_eq!(unsafe { libc::dup2(file.as_raw_fd(), 900) }, 900);
                assert_eq!(unsafe { libc::fcntl(900, libc::F_SETFD, 0) }, 0);
                let lowered = libc::rlimit {
                    rlim_cur: 256,
                    rlim_max: original.rlim_max,
                };
                assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) }, 0);
                let root = temp_project("lowered-fd-limit");
                let outcome = run_ruby(
                    &root,
                    "begin; IO.for_fd(900, autoclose: false); exit 9; rescue Errno::EBADF; exit 0; end",
                    false,
                    true,
                );
                let success = outcome.termination() == &Termination::Exited(0);
                unsafe { libc::close(900) };
                fs::remove_dir_all(root).unwrap();
                usize::from(!success)
            }
            _ => unreachable!(),
        };
        unsafe { libc::_exit(code as i32) }
    }

    for mode in ["infinity", "huge", "full", "lowered"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "execution::platform_backend::tests::descriptor_sanitation_is_bounded_for_huge_limits_and_fails_closed_when_full",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{mode} child failed with {status}");
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{mode} descriptor sanitation exceeded deadline");
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

#[test]
fn first_support_probe_preserves_occupied_fd_100_during_concurrent_churn() {
    const CHILD: &str = "TAPID_TEST_PROBE_FD_100_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let file = fs::File::open("/etc/hosts").unwrap();
        assert_eq!(unsafe { libc::dup2(file.as_raw_fd(), 100) }, 100);
        assert_eq!(unsafe { libc::fcntl(100, libc::F_SETFD, 0) }, 0);
        let mut before: libc::stat = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::fstat(100, &mut before) }, 0);
        let running = Arc::new(AtomicBool::new(true));
        let churn_running = Arc::clone(&running);
        let churn = thread::spawn(move || {
            while churn_running.load(Ordering::Acquire) {
                if let Ok(file) = fs::File::open("/dev/null") {
                    drop(file);
                }
            }
        });
        let probe = run_support_probes();
        running.store(false, Ordering::Release);
        churn.join().unwrap();
        let mut after: libc::stat = unsafe { std::mem::zeroed() };
        let intact = unsafe { libc::fstat(100, &mut after) } == 0
            && (before.st_dev, before.st_ino) == (after.st_dev, after.st_ino);
        let mut byte = [0_u8; 1];
        let readable = unsafe { libc::pread(100, byte.as_mut_ptr().cast(), 1, 0) } == 1;
        unsafe { libc::close(100) };
        let code =
            usize::from(probe.is_err()) + usize::from(!intact) * 2 + usize::from(!readable) * 4;
        unsafe { libc::_exit(code as i32) }
    }

    let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "execution::platform_backend::tests::first_support_probe_preserves_occupied_fd_100_during_concurrent_churn",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "probe child failed with {status}");
            break;
        }
        assert!(Instant::now() < deadline, "support probe exceeded deadline");
        thread::sleep(POLL_INTERVAL);
    }
}

#[test]
fn unread_parent_output_pipes_fail_bounded_and_clean_the_target() {
    const CHILD: &str = "TAPID_TEST_UNREAD_OUTPUT_CHILD";
    if let Ok(root) = std::env::var(CHILD) {
        let root = PathBuf::from(root);
        let request = ExecutionRequest::builder("/bin/sh")
                .args([
                    "-c",
                    "printf '%s' $$ > child-pid; chunk=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; while :; do printf '%s' \"$chunk\"; printf '%s' \"$chunk\" >&2; done; : > marker",
                ])
                .project_root(&root)
                .policy(policy(false, true, Vec::new()))
                .build()
                .unwrap();
        let code = match super::super::execute(&request) {
            Err(error) if error.category() == ExecutionErrorCategory::Internal => 0,
            _ => 2,
        };
        // SAFETY: this isolated regression child must not let libtest write a completion line
        // into the deliberately full inherited output pipes.
        unsafe { libc::_exit(code) }
    }

    let root = temp_project("unread-output");
    let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "execution::platform_backend::tests::unread_parent_output_pipes_fail_bounded_and_clean_the_target",
                "--nocapture",
            ])
            .env(CHILD, &root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        thread::sleep(POLL_INTERVAL);
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let target_pid = fs::read_to_string(root.join("child-pid"))
        .ok()
        .and_then(|value| value.parse::<i32>().ok());
    let target_survived = target_pid.is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0);
    if let Some(pid) = target_pid {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let marker = root.join("marker").exists();
    fs::remove_dir_all(root).unwrap();
    assert!(status.is_some(), "supervisor hung on unread output pipes");
    assert!(
        status.unwrap().success(),
        "bounded sink path failed its assertions"
    );
    assert!(!marker, "target reached its post-output marker");
    assert!(
        !target_survived,
        "target survived sink-backpressure cleanup"
    );
}

#[test]
fn group_signal_is_never_attempted_after_leader_is_reaped() {
    let root = temp_project("post-reap-group");
    let request = ExecutionRequest::builder("/bin/sh")
        .project_root(&root)
        .policy(policy(false, true, Vec::new()))
        .build()
        .unwrap();
    let additions = PlatformBackend
        .runtime_filesystem_additions(&request)
        .unwrap();
    let resolved = super::super::resolve_policy(&request, additions).unwrap();
    let preflight = ValidatedPreflight {
        support: containment_support(&request),
        bindings: FilesystemBindings::canonical_path(&resolved).unwrap(),
        policy: resolved,
        child_environment: request.child_environment(),
    };
    let mut lifecycle = MacosLifecycle {
        request,
        preflight: &preflight,
        launch: None,
        child: None,
        process_group: Some(i32::MAX),
        cleanup_attempted: false,
        cleanup_observed: false,
        process_started: false,
    };
    lifecycle.kill_group();
    assert!(!lifecycle.cleanup_attempted);
    assert_eq!(lifecycle.process_group, None);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn output_reader_sets_overflow_without_growing_past_internal_ceiling() {
    let stop = Arc::new(AtomicBool::new(false));
    let overflow = Arc::new(AtomicBool::new(false));
    let input = std::io::Cursor::new(vec![b'x'; INTERNAL_OUTPUT_CEILING + 1]);
    let reader = spawn_reader(
        input,
        stop,
        Arc::clone(&overflow),
        Arc::new(AtomicUsize::new(0)),
        None,
        Arc::new(AtomicBool::new(false)),
    );
    let captured = reader.join().unwrap().unwrap();
    assert!(overflow.load(Ordering::Acquire));
    assert!(captured.len() <= INTERNAL_OUTPUT_CEILING);
}

#[test]
fn output_readers_enforce_one_combined_internal_ceiling() {
    let stop = Arc::new(AtomicBool::new(false));
    let overflow = Arc::new(AtomicBool::new(false));
    let output_bytes = Arc::new(AtomicUsize::new(0));
    let stdout = spawn_reader(
        std::io::Cursor::new(vec![b'o'; 9 * 1024 * 1024]),
        Arc::clone(&stop),
        Arc::clone(&overflow),
        Arc::clone(&output_bytes),
        None,
        Arc::new(AtomicBool::new(false)),
    );
    let stderr = spawn_reader(
        std::io::Cursor::new(vec![b'e'; 9 * 1024 * 1024]),
        stop,
        Arc::clone(&overflow),
        output_bytes,
        None,
        Arc::new(AtomicBool::new(false)),
    );
    let stdout = stdout.join().unwrap().unwrap();
    let stderr = stderr.join().unwrap().unwrap();
    assert!(overflow.load(Ordering::Acquire));
    assert!(stdout.len() + stderr.len() <= INTERNAL_OUTPUT_CEILING);
}

#[test]
fn output_readers_accept_combined_output_at_or_below_internal_ceiling() {
    for (stdout_size, stderr_size) in [
        (8 * 1024 * 1024, 8 * 1024 * 1024),
        (9 * 1024 * 1024, 7 * 1024 * 1024),
    ] {
        let stop = Arc::new(AtomicBool::new(false));
        let overflow = Arc::new(AtomicBool::new(false));
        let output_bytes = Arc::new(AtomicUsize::new(0));
        let stdout = spawn_reader(
            std::io::Cursor::new(vec![b'o'; stdout_size]),
            Arc::clone(&stop),
            Arc::clone(&overflow),
            Arc::clone(&output_bytes),
            None,
            Arc::new(AtomicBool::new(false)),
        );
        let stderr = spawn_reader(
            std::io::Cursor::new(vec![b'e'; stderr_size]),
            stop,
            Arc::clone(&overflow),
            output_bytes,
            None,
            Arc::new(AtomicBool::new(false)),
        );
        let stdout = stdout.join().unwrap().unwrap();
        let stderr = stderr.join().unwrap().unwrap();
        assert!(!overflow.load(Ordering::Acquire));
        assert_eq!(stdout.len() + stderr.len(), INTERNAL_OUTPUT_CEILING);
    }
}

#[test]
fn closed_live_sink_sets_the_supervisor_failure_signal() {
    const CHILD: &str = "TAPID_TEST_CLOSED_LIVE_SINK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Other tests fork concurrently and can briefly inherit the sink's
        // pipe reader even with CLOEXEC. Isolate the EPIPE assertion so it
        // observes only this test's deliberately terminated sink.
        let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "execution::platform_backend::tests::closed_live_sink_sets_the_supervisor_failure_signal",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
        assert!(status.success(), "closed live sink child failed: {status}");
        return;
    }
    let (read, write) = launch_pipe().unwrap();
    let (mut child, mut input) = spawn_live_sink(write.as_raw_fd()).unwrap();
    drop(write);
    child.kill().unwrap();
    child.wait().unwrap();
    drop(read);
    let stop = AtomicBool::new(false);
    let failed = AtomicBool::new(false);
    assert!(write_live_output(&mut input, b"output", &stop, &failed).is_err());
    assert!(failed.load(Ordering::Acquire));
}

#[test]
fn bounded_live_sink_preserves_normal_output_exactly_once() {
    let (read, write) = launch_pipe().unwrap();
    let (child, mut input) = spawn_live_sink(write.as_raw_fd()).unwrap();
    drop(write);
    let stop = AtomicBool::new(false);
    let backpressure = AtomicBool::new(false);
    write_live_output(&mut input, b"first\n", &stop, &backpressure).unwrap();
    write_live_output(&mut input, b"second\n", &stop, &backpressure).unwrap();
    drop(input);
    let mut sinks = LiveSinks {
        children: vec![child],
    };
    sinks.finish().unwrap();
    let mut output = Vec::new();
    fs::File::from(read).read_to_end(&mut output).unwrap();
    assert_eq!(output, b"first\nsecond\n");
    assert!(!backpressure.load(Ordering::Acquire));
}

#[test]
fn live_sink_exec_closes_unrelated_inheritable_descriptors() {
    const CHILD: &str = "TAPID_TEST_LIVE_SINK_FDS";
    if std::env::var_os(CHILD).is_some() {
        let file = fs::File::open("/etc/hosts").unwrap();
        let (pipe_read, pipe_write) = launch_pipe().unwrap();
        let (socket, peer) = UnixStream::pair().unwrap();
        for (source, target) in [
            (file.as_raw_fd(), 100),
            (pipe_read.as_raw_fd(), 101),
            (socket.as_raw_fd(), 102),
        ] {
            assert_eq!(unsafe { libc::dup2(source, target) }, target);
            assert_eq!(unsafe { libc::fcntl(target, libc::F_SETFD, 0) }, 0);
        }
        let (destination_read, destination_write) = launch_pipe().unwrap();
        let (mut sink, input) = spawn_live_sink(destination_write.as_raw_fd()).unwrap();
        drop(destination_write);
        let mut entries = [libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        }; 128];
        let bytes = unsafe {
            libc::proc_pidinfo(
                sink.id() as i32,
                libc::PROC_PIDLISTFDS,
                0,
                entries.as_mut_ptr().cast(),
                std::mem::size_of_val(&entries) as libc::c_int,
            )
        };
        assert!(bytes > 0);
        let inherited: Vec<_> = entries
            [..bytes as usize / std::mem::size_of::<libc::proc_fdinfo>()]
            .iter()
            .map(|entry| entry.proc_fd)
            .filter(|fd| *fd > 2)
            .collect();
        drop(input);
        assert!(sink.wait().unwrap().success());
        drop(destination_read);
        drop(pipe_write);
        drop(peer);
        for fd in [100, 101, 102] {
            unsafe { libc::close(fd) };
        }
        assert!(
            inherited.is_empty(),
            "relay inherited descriptors {inherited:?}"
        );
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "execution::platform_backend::tests::live_sink_exec_closes_unrelated_inheritable_descriptors",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn malformed_and_unavailable_sandbox_launchers_fail_before_target_marker() {
    let root = temp_project("launcher-failure");
    let marker = root.join("untrusted-marker");
    let marker_text = marker.to_string_lossy();
    let malformed = Command::new(SANDBOX_EXEC)
        .args([
            "-p",
            "(version 1)(this-operation-does-not-exist)",
            "/bin/sh",
            "-c",
            "printf spawned > \"$1\"",
            "target",
            &marker_text,
        ])
        .env_clear()
        .status()
        .unwrap();
    assert!(!malformed.success());
    assert!(!marker.exists());
    let unavailable = Command::new("/definitely/unavailable/sandbox-exec")
        .args([
            "/bin/sh",
            "-c",
            "printf spawned > \"$1\"",
            "target",
            &marker_text,
        ])
        .env_clear()
        .status();
    assert!(unavailable.is_err());
    assert!(!marker.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn securely_materializing_a_write_grant_rejects_a_symlink_component() {
    use std::os::unix::fs::symlink;
    let root = temp_project("materialize");
    let outside = temp_project("materialize-outside");
    symlink(&outside, root.join("link")).unwrap();
    let error = securely_create_relative_directories(&root, Path::new("link/child"))
        .expect_err("O_NOFOLLOW traversal must reject symlinks");
    assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
    assert!(!outside.join("child").exists());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside).unwrap();
}

#[test]
fn network_false_denies_tcp_bind_and_connect_unix_sockets_dns_and_reserved_connect() {
    let root = PathBuf::from(format!("/private/tmp/tapid-network-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let unix_path = root.join("s");
    let _ = fs::remove_file(&unix_path);
    let scripts = [
            "require 'socket'; TCPServer.new('127.0.0.1', 0)".to_owned(),
            format!("require 'socket'; TCPSocket.new('127.0.0.1', {port})"),
            format!("require 'socket'; UNIXServer.new({unix_path:?})"),
            "require 'socket'; s=UDPSocket.new; s.connect('127.0.0.1',53)".to_owned(),
            "require 'socket'; s=Socket.new(:INET,:STREAM); begin; s.connect_nonblock(Socket.sockaddr_in(9,'192.0.2.1')); rescue IO::WaitWritable; exit 0; end".to_owned(),
        ];
    for script in scripts {
        let allowed = run_ruby(&root, &script, true, true);
        assert_eq!(
            allowed.termination(),
            &Termination::Exited(0),
            "positive control: {script}"
        );
        if unix_path.exists() {
            fs::remove_file(&unix_path).unwrap();
        }
        let outcome = run_ruby(&root, &script, false, true);
        assert_ne!(outcome.termination(), &Termination::Exited(0), "{script}");
    }
    assert!(!unix_path.exists());
    drop(listener);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn subprocess_false_denies_fork_but_allows_exec_replacement() {
    let root = temp_project("subprocess");
    let denied = run_ruby(
        &root,
        "begin; Process.fork { exit 0 }; Process.wait; exit 9; rescue SystemCallError; exit 0; end",
        false,
        false,
    );
    assert_eq!(denied.termination(), &Termination::Exited(0));
    let request = ExecutionRequest::builder("/bin/sh")
        .args(["-c", "exec /bin/sh -c 'exit 23'"])
        .project_root(&root)
        .policy(policy(false, false, Vec::new()))
        .executable_search_path("/usr/bin")
        .build()
        .unwrap();
    let replaced = super::super::execute(&request).unwrap();
    assert_eq!(replaced.termination(), &Termination::Exited(23));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn child_gets_only_validated_environment_null_stdin_and_fidelity_preserved_arguments() {
    let root = temp_project("environment");
    let hostile = OsString::from("space ' quote \" dollar $ semicolon ;");
    let request = ExecutionRequest::builder("/usr/bin/ruby")
        .args([
            OsString::from("--disable-gems"),
            OsString::from("-e"),
            OsString::from("abort unless STDIN.read.empty?; print [ENV.to_h, ARGV].inspect"),
            hostile.clone(),
        ])
        .project_root(&root)
        .policy(policy(false, true, vec!["TAPID_SENTINEL".into()]))
        .executable_search_path("/usr/bin")
        .env("TAPID_SENTINEL", "present")
        .build()
        .unwrap();
    let outcome = super::super::execute(&request).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    let output = String::from_utf8(outcome.stdout().to_vec()).unwrap();
    assert!(output.contains("TAPID_SENTINEL"));
    assert!(output.contains("PATH"));
    for forbidden in [
        "HOME",
        "TMPDIR",
        "SSH_AUTH_SOCK",
        "HTTP_PROXY",
        "DYLD_INSERT_LIBRARIES",
    ] {
        assert!(!output.contains(forbidden), "ambient {forbidden} leaked");
    }
    assert!(output.contains(&format!("{:?}", hostile.to_str().unwrap())));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn inherited_regular_connected_and_listening_descriptors_are_closed() {
    let root = temp_project("fds");
    let file = fs::File::open("/etc/hosts").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(peer_listener.local_addr().unwrap()).unwrap();
    let (_peer, _) = peer_listener.accept().unwrap();
    let sources = [file.as_raw_fd(), stream.as_raw_fd(), listener.as_raw_fd()];
    let fds = [100, 101, 102];
    for (source, fd) in sources.into_iter().zip(fds) {
        // SAFETY: each source fd is live; high-number duplicates create adversarial inherited
        // authorities without colliding with std's child setup descriptors.
        assert_eq!(unsafe { libc::dup2(source, fd) }, fd);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
    }
    let script = format!(
        "fds={fds:?}; exit(fds.all? {{ |fd| begin; IO.for_fd(fd, autoclose: false); false; rescue Errno::EBADF; true; end }} ? 0 : 9)"
    );
    let outcome = run_ruby(&root, &script, true, true);
    for fd in fds {
        // SAFETY: these are the test-owned duplicates created above.
        unsafe { libc::close(fd) };
    }
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn inherited_authority_does_not_cross_fork_exec_setsid_or_double_fork() {
    let root = temp_project("propagation");
    let outside = root.with_extension("outside-marker");
    let outside_text = outside.to_string_lossy();
    let scripts = [
        format!("Process.fork {{ File.write({outside_text:?}, 'x') }}; Process.wait"),
        format!(
            r#"Process.fork {{ exec('/bin/sh','-c', 'printf x > "$1"', 'child', {outside_text:?}) }}; Process.wait"#
        ),
        format!(
            "Process.fork {{ Process.setsid; File.write({outside_text:?}, 'x') }}; Process.wait"
        ),
        format!(
            "Process.fork {{ Process.fork {{ File.write({outside_text:?}, 'x') }}; Process.wait }}; Process.wait"
        ),
    ];
    for script in scripts {
        let _ = run_ruby(&root, &script, false, true);
        assert!(!outside.exists(), "descendant escaped Seatbelt: {script}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn managed_tree_and_every_resource_limit_fail_closed() {
    let root = temp_project("unsupported");
    let build = |assurance, limits| {
        let configured = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            assurance,
            FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
            false,
            Vec::new(),
            true,
            limits,
        )
        .unwrap();
        ExecutionRequest::builder("/bin/sh")
            .args(["-c", "exit 0"])
            .project_root(&root)
            .policy(configured)
            .executable_search_path("/usr/bin")
            .build()
            .unwrap()
    };
    assert!(
        !containment_support(&build(
            AssuranceLevel::ManagedTree,
            ExecutionLimits::default()
        ))
        .is_supported()
    );
    for limits in [
        ExecutionLimits::new(Some(1), None, None, None).unwrap(),
        ExecutionLimits::new(None, Some(1), None, None).unwrap(),
        ExecutionLimits::new(None, None, Some(1), None).unwrap(),
        ExecutionLimits::new(None, None, None, Some(1)).unwrap(),
    ] {
        assert!(!containment_support(&build(AssuranceLevel::Restricted, limits)).is_supported());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn path_alias_symlink_rename_hardlink_and_hostile_parameter_inputs_do_not_escape() {
    let root = temp_project("hostile-\")-(allow-default)-");
    let outside = root.with_extension("outside");
    fs::write(&outside, b"outside").unwrap();
    let alias = root.to_string_lossy().replacen("/private/var/", "/var/", 1);
    let request = ExecutionRequest::builder("/usr/bin/ruby")
            .args([
                "--disable-gems",
                "-e",
                "outside=ARGV[0]; begin; File.symlink(outside,'sym'); File.write('sym','bad'); rescue SystemCallError; end; begin; File.rename(outside,'renamed'); rescue SystemCallError; end; begin; File.link(outside,'hard'); rescue SystemCallError; end; File.write('allowed','ok')",
                outside.to_str().unwrap(),
            ])
            .project_root(&alias)
            .policy(policy(false, true, Vec::new()))
            .executable_search_path("/usr/bin")
            .build()
            .unwrap();
    let outcome = super::super::execute(&request).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
    assert!(!root.join("renamed").exists());
    assert!(!root.join("hard").exists());
    assert_eq!(fs::read(root.join("allowed")).unwrap(), b"ok");
    assert!(
        outcome
            .enforcement()
            .resolved_filesystem()
            .grants()
            .iter()
            .all(|grant| !grant.path().to_string_lossy().starts_with("/var/"))
    );
    let _ = fs::remove_file(root.join("sym"));
    fs::remove_file(outside).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn held_output_pipe_is_bounded_and_does_not_hang_completion() {
    let root = temp_project("held-pipe");
    let started = Instant::now();
    let outcome = run_ruby(
        &root,
        "pid = Process.fork { sleep 30 }; File.write('child-pid', pid); puts 'parent-exit'",
        false,
        true,
    );
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::NotGuaranteed
    );
    let child_pid = fs::read_to_string(root.join("child-pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    unsafe { libc::kill(child_pid, libc::SIGKILL) };
    fs::remove_dir_all(root).unwrap();
}
