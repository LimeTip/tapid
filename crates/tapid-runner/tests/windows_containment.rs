#![cfg(windows)]

use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tapid_runner::{
    AssuranceLevel, CleanupConfidence, ExecutionLimits, ExecutionRequest, FilesystemPolicy,
    SandboxMode, SandboxPolicy, Termination, execute,
};

fn temporary_project(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("tapid-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).unwrap();
    path
}

fn managed_policy() -> SandboxPolicy {
    SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::ManagedTree,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
        false,
        vec!["TAPID_TEST_MARKER".into()],
        true,
        ExecutionLimits::new(Some(30), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap()
}

#[test]
fn windows_managed_tree_executes_with_checked_appcontainer_and_job_evidence() {
    let root = temporary_project("managed-tree");
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = PathBuf::from(system_root).join("System32");
    let command = system32.join("cmd.exe");
    let request = ExecutionRequest::builder(command.as_os_str())
        .args([
            OsString::from("/D"),
            OsString::from("/C"),
            OsString::from("echo TAPID_WINDOWS_MANAGED_TREE>allowed.txt"),
        ])
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy())
        .env("TAPID_TEST_MARKER", "allowed-value")
        .build()
        .unwrap();

    let outcome = execute(&request).expect("Windows ManagedTree must execute natively");
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert_eq!(
        fs::read_to_string(root.join("allowed.txt")).unwrap().trim(),
        "TAPID_WINDOWS_MANAGED_TREE"
    );
    assert_eq!(
        outcome.enforcement().assurance(),
        AssuranceLevel::ManagedTree
    );
    assert_eq!(
        outcome.enforcement().backend().name(),
        "tapid-runner/windows-appcontainer-job"
    );
    for dimension in [
        tapid_runner::EnforcementDimension::FilesystemRead,
        tapid_runner::EnforcementDimension::FilesystemWrite,
        tapid_runner::EnforcementDimension::Network,
        tapid_runner::EnforcementDimension::EnvironmentSanitization,
        tapid_runner::EnforcementDimension::DescriptorHygiene,
        tapid_runner::EnforcementDimension::DescendantAuthorityPropagation,
        tapid_runner::EnforcementDimension::DescendantLifecycle,
        tapid_runner::EnforcementDimension::ProcessTreeMembership,
        tapid_runner::EnforcementDimension::CompleteCleanup,
        tapid_runner::EnforcementDimension::Timeout,
        tapid_runner::EnforcementDimension::Output,
        tapid_runner::EnforcementDimension::ProcessCount,
        tapid_runner::EnforcementDimension::Memory,
    ] {
        assert!(
            outcome
                .enforcement()
                .established_evidence()
                .iter()
                .any(|evidence| evidence.dimension() == dimension),
            "missing launch evidence for {dimension:?}"
        );
    }
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    fs::remove_dir_all(root).unwrap();
}
