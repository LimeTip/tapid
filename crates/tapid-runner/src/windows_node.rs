//! Test-only bounded diagnostics for stock Node IPC under the current AppContainer backend.
use super::*;

const IPC_TIMEOUT_MS: u32 = 3_000;
const CONTROL_OUTPUT_LIMIT: u64 = 4_096;
const IPC_MARKER: &[u8] = b"TAPID_NODE_IPC_V1\n";
const IPC_CONTROL: &str = r#"
const {spawn} = require('node:child_process');
const payload = 'tapid-ipc-roundtrip-v1';
const childCode = "process.on('message',m=>{if(m!=='tapid-ipc-roundtrip-v1')process.exit(2);process.send(m,()=>process.disconnect());})";
let completed = 0;
for (let i = 0; i < 2; i++) {
  const child = spawn(process.execPath, ['-e', childCode], {stdio:['ignore','inherit','inherit','ipc'], env:process.env});
  let replied = false;
  child.on('error',()=>process.exit(3));
  child.on('message',m=>{if(replied || m!==payload)process.exit(4);replied=true;});
  child.on('exit',(code,signal)=>{
    if(code!==0 || signal || !replied)process.exit(5);
    if(++completed===2)process.stdout.write('TAPID_NODE_IPC_V1\n');
  });
  child.send(payload);
}
"#;

pub(super) fn verify_ipc(
    runtime: &Path,
    container: &WindowsAppContainer,
    environment: &[u16],
    working_directory: &[u16],
    cancellation: Option<&WindowsCancellation>,
) -> Result<(), ExecutionError> {
    let (stdout, _) = run_control(
        runtime,
        IPC_CONTROL,
        container,
        environment,
        working_directory,
        cancellation,
    )?;
    if stdout != IPC_MARKER {
        return Err(incompatible(
            "round-trip acknowledgement is missing or invalid",
        ));
    }
    Ok(())
}

fn incompatible(reason: &str) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::UnsupportedContainment,
        format!(
            "Windows stock Node AppContainer IPC diagnostic failed: {reason}; the current Tapid containment adapter needs compatibility work"
        ),
    )
}

fn run_control(
    runtime: &Path,
    script: &str,
    container: &WindowsAppContainer,
    environment: &[u16],
    working_directory: &[u16],
    cancellation: Option<&WindowsCancellation>,
) -> Result<(Vec<u8>, Vec<u8>), ExecutionError> {
    let program: Vec<_> = runtime.as_os_str().encode_wide().collect();
    let arguments = ["-e", script]
        .iter()
        .map(|arg| arg.encode_utf16().collect())
        .collect::<Vec<_>>();
    let command = serialize_windows_command_line_units(&program, &arguments, false)?;
    let limits = ExecutionLimits::new(Some(3), Some(CONTROL_OUTPUT_LIMIT), Some(3), None)
        .expect("static IPC control limits are valid");
    let job = WindowsJob::new(&limits, false)?;
    let mut pipes = WindowsStdioPipes::new()?;
    let mut child = WindowsSuspendedChild::create_with_stdio(
        container,
        &wide(runtime.as_os_str()),
        &command,
        environment,
        working_directory,
        &mut pipes,
    )?;
    job.assign_suspended_process(child.process_handle())?;
    let capture =
        WindowsOutputCapture::start(pipes.into_parent_readers()?, Some(CONTROL_OUTPUT_LIMIT));
    let status = child.resume_and_wait_for_status(
        &job,
        IPC_TIMEOUT_MS,
        capture.output_limit_exceeded(),
        cancellation,
    );
    // The regular child supervisor confirms an empty Job on success and on limit termination.
    // On a native wait error, keep grants alive and explicitly terminate/drain before capture.
    if status.is_err() {
        job.empty_before_revocation();
    }
    let output = capture.finish()?;
    match status? {
        WindowsChildTermination::Exited(0) => Ok(output),
        WindowsChildTermination::Exited(code) => Err(incompatible(&format!(
            "control exited unsuccessfully ({code}): {}",
            String::from_utf8_lossy(&output.1)
        ))),
        WindowsChildTermination::TimedOut => {
            Err(incompatible("control exceeded its 3 second deadline"))
        }
        WindowsChildTermination::OutputLimitExceeded => {
            Err(incompatible("control exceeded its output bound"))
        }
        WindowsChildTermination::Cancelled => Err(incompatible("control was cancelled")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{
        FilesystemAccess, FilesystemBindingMode, FilesystemGrantKind, FilesystemGrantSource,
        ResolvedFilesystemGrant,
        windows_acl::{initialize_inheritance, read_acl},
    };

    fn with_node_control(
        project_access: FilesystemAccess,
        test: impl FnOnce(&Path, &WindowsAppContainer, &[u16], &[u16]),
    ) {
        let project = tapid_test_support::TempProject::new("ipc-control-project").unwrap();
        let runtime = tapid_test_support::TempProject::new("ipc-control-runtime").unwrap();
        let root = fs::canonicalize(project.path()).unwrap();
        let runtime_root = fs::canonicalize(runtime.path()).unwrap();
        let node = runtime_root.join("node.exe");
        fs::copy(
            std::env::var_os("TAPID_TEST_NODE").expect("native Node is required"),
            &node,
        )
        .unwrap();
        initialize_inheritance(&root);
        initialize_inheritance(&runtime_root);
        let paths = [&root, &runtime_root, &node];
        let before: Vec<_> = paths.iter().map(|path| read_acl(path)).collect();
        let mut container = WindowsAppContainer::create().unwrap();
        let resolved = [&root, &runtime_root].map(|path| ResolvedFilesystemGrant {
            path: path.to_path_buf(),
            access: if *path == root {
                project_access
            } else {
                FilesystemAccess::Read
            },
            kind: FilesystemGrantKind::DirectorySubtree,
            source: if *path == root {
                FilesystemGrantSource::ProjectPolicy
            } else {
                FilesystemGrantSource::BackendRuntime
            },
            binding: FilesystemBindingMode::CanonicalPath,
        });
        let mut grants = WindowsFilesystemGrants::apply(&container, &resolved).unwrap();
        let mut environment = std::collections::BTreeMap::new();
        environment.insert(OsString::from("PATH"), runtime_root.as_os_str().to_owned());
        // No caller environment or credentials; use the production Node options.
        add_node_appcontainer_options(&mut environment);
        let environment = windows_environment_block_units(&environment).unwrap();
        let working_directory = process_working_directory(&root).unwrap();
        test(&node, &container, &environment, &working_directory);
        grants.restore().unwrap();
        container.cleanup().unwrap();
        assert_eq!(
            paths.iter().map(|path| read_acl(path)).collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn project_read_grant_can_load_native_images_without_granting_writes() {
        with_node_control(
            FilesystemAccess::Read,
            |node, container, environment, cwd| {
                let root = PathBuf::from(String::from_utf16(&cwd[..cwd.len() - 1]).unwrap());
                let library = root.join("readable-native.dll");
                fs::copy(
                    PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                        .join("System32")
                        .join("version.dll"),
                    &library,
                )
                .unwrap();
                // A system DLL is not a Node addon: this exact error proves the native loader
                // mapped it successfully, without requiring a compiler or external package.
                let script = r#"
const assert = require('node:assert/strict'), fs = require('node:fs'), path = require('node:path');
const library = path.join(process.cwd(), 'readable-native.dll');
assert.throws(() => process.dlopen({exports:{}}, library), /Module did not self-register/);
assert.throws(() => fs.writeFileSync(library, 'overwrite'), e => ['EPERM','EACCES'].includes(e.code));
console.log('NATIVE_READ_OK_WRITE_DENIED');
"#;
                let (stdout, _) =
                    run_control(node, script, container, environment, cwd, None).unwrap();
                assert_eq!(stdout, b"NATIVE_READ_OK_WRITE_DENIED\n");
            },
        );
    }

    #[test]
    fn project_data_only_grant_does_not_allow_native_image_loading() {
        with_node_control(
            FilesystemAccess::ReadData,
            |node, container, environment, cwd| {
                let root = PathBuf::from(String::from_utf16(&cwd[..cwd.len() - 1]).unwrap());
                fs::copy(
                    PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                        .join("System32")
                        .join("version.dll"),
                    root.join("data-only-native.dll"),
                )
                .unwrap();
                let script = r#"
const assert = require('node:assert/strict'), path = require('node:path');
const library = path.join(process.cwd(), 'data-only-native.dll');
assert.throws(() => process.dlopen({exports:{}}, library), /Access is denied/);
console.log('NATIVE_DATA_ONLY_EXECUTE_DENIED');
"#;
                let (stdout, _) =
                    run_control(node, script, container, environment, cwd, None).unwrap();
                assert_eq!(stdout, b"NATIVE_DATA_ONLY_EXECUTE_DENIED\n");
            },
        );
    }

    #[test]
    fn node_control_stdio_succeeds_and_timeout_and_output_limits_reap() {
        with_node_control(
            FilesystemAccess::Read,
            |node, container, environment, cwd| {
                let output = run_control(
                    node,
                    "process.stdout.write('control-ok')",
                    container,
                    environment,
                    cwd,
                    None,
                )
                .unwrap();
                assert_eq!(output, (b"control-ok".to_vec(), vec![]));
                for (script, reason) in [
                    ("setInterval(()=>{},1000)", "3 second deadline"),
                    (
                        "while(true)process.stdout.write('x'.repeat(8192))",
                        "output bound",
                    ),
                    ("process.exit(7)", "exited unsuccessfully"),
                ] {
                    let error =
                        run_control(node, script, container, environment, cwd, None).unwrap_err();
                    assert_eq!(
                        error.category(),
                        ExecutionErrorCategory::UnsupportedContainment
                    );
                    assert!(error.to_string().contains(reason), "{error}");
                }
            },
        );
    }

    #[test]
    fn selected_node_ipc_has_host_control_and_bounded_contained_verdict() {
        with_node_control(
            FilesystemAccess::Read,
            |node, container, environment, cwd| {
                let output = std::process::Command::new(node)
                    .args(["-e", IPC_CONTROL])
                    .env_clear()
                    .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "uncontained IPC positive control failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(output.stdout, IPC_MARKER);
                let started = std::time::Instant::now();
                match verify_ipc(node, container, environment, cwd, None) {
                    Ok(()) => eprintln!("NODE_APPCONTAINER_IPC compatible"),
                    Err(error) => {
                        assert_eq!(
                            error.category(),
                            ExecutionErrorCategory::UnsupportedContainment
                        );
                        let message = error.to_string();
                        assert!(
                            message.contains("3 second deadline")
                                || message.contains("exited unsuccessfully"),
                            "{error}"
                        );
                        eprintln!("NODE_APPCONTAINER_IPC incompatible: {error}");
                    }
                }
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "IPC control failed to terminate its tree within the bound"
                );
            },
        );
    }
}
