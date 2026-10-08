//! Host-runnable wiring checks complement, but do not replace, native Windows containment tests.

#[test]
fn cancellation_scope_finishes_after_cleanup_before_outcome_mapping() {
    let source = include_str!("../src/windows_execution.rs");
    let execute = source
        .split("fn execute(&mut self)")
        .nth(1)
        .unwrap()
        .split("fn cleanup(&mut self)")
        .next()
        .unwrap();
    let after_cleanup = execute
        .split("let completion = self.cleanup_resources();")
        .nth(1)
        .unwrap();
    let finish = after_cleanup
        .find(".finish()")
        .expect("cleanup must finish event ownership");
    assert!(
        finish
            < after_cleanup
                .find("let termination = match termination")
                .unwrap()
    );
    assert!(after_cleanup[..finish].contains(".cancellation"));
    assert!(after_cleanup[..finish].contains(".take()"));
    assert!(after_cleanup.contains("WindowsChildTermination::Cancelled"));
}

#[test]
fn runtime_probe_owns_disposable_runtime_and_project() {
    let source = include_str!("windows_containment.rs");
    let probe = source
        .split("fn windows_runtime_directory_ace_allows_launching_read_execute_binaries()")
        .nth(1)
        .unwrap()
        .split("#[test]")
        .next()
        .unwrap();
    assert!(!probe.contains("TAPID_TEST_NODE"));
    assert!(!probe.contains("node.parent()"));
    assert!(probe.contains("TempProject::new(\"runtime-executable-acl\")"));
    assert!(probe.contains("TempProject::new(\"runtime-executable-project\")"));
    assert!(probe.contains("baseline_runtime_acl"));
    assert!(probe.contains("baseline_probe_acl"));
    assert!(probe.contains("baseline_project_acl"));
}

#[test]
fn disposable_runtime_and_project_are_removed_on_unwind() {
    use tapid_test_support::TempProject;
    let runtime = TempProject::new("runtime-probe-unwind").unwrap();
    let project = TempProject::new("runtime-project-unwind").unwrap();
    let runtime_path = runtime.path().to_path_buf();
    let project_path = project.path().to_path_buf();
    runtime.write("cmd.exe", b"fixture").unwrap();
    let result = std::panic::catch_unwind(move || {
        let _runtime = runtime;
        let _project = project;
        panic!("simulate a probe assertion failure");
    });
    assert!(result.is_err());
    assert!(!runtime_path.exists());
    assert!(!project_path.exists());
}
