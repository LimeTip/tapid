use super::*;
use crate::config::{
    AssuranceLevel, ExecutionLimits, FilesystemPolicy, SandboxMode, SandboxPolicy,
};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn required_policy() -> SandboxPolicy {
    SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec!["NODE_ENV".into()],
        true,
        ExecutionLimits::new(None, None, None, None).unwrap(),
    )
    .unwrap()
}

#[test]
fn restricted_separates_authority_containment_from_managed_tree_ownership() {
    let restricted = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let restricted = EnforcementDimensions::requested_by(&restricted);
    assert!(restricted.descriptor_hygiene());
    assert!(restricted.descendant_authority_propagation());
    assert!(!restricted.process_tree_membership());
    assert!(!restricted.complete_cleanup());

    let managed = EnforcementDimensions::requested_by(&required_policy());
    assert!(managed.process_tree_membership());
    assert!(managed.complete_cleanup());
}

#[test]
fn managed_tree_reports_lifecycle_ownership_as_an_independent_dimension() {
    let managed = required_policy();
    let managed_requested = EnforcementDimensions::requested_by(&managed);
    let managed_evidence = evidence_for_dimensions(&managed_requested, "test", &[]);
    assert!(
        managed_evidence
            .iter()
            .any(|evidence| evidence.dimension() == EnforcementDimension::DescendantLifecycle)
    );

    let restricted = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        managed.filesystem().clone(),
        managed.network(),
        managed.environment().to_vec(),
        managed.subprocess(),
        managed.limits().clone(),
    )
    .unwrap();
    let restricted_evidence = evidence_for_dimensions(
        &EnforcementDimensions::requested_by(&restricted),
        "test",
        &[],
    );
    assert!(
        !restricted_evidence
            .iter()
            .any(|evidence| evidence.dimension() == EnforcementDimension::DescendantLifecycle)
    );
}

#[test]
fn support_reports_each_dimension_with_scope_mechanism_and_limitations() {
    let requested = EnforcementDimensions::requested_by(&required_policy());
    let support = support_with_evidence(requested.clone(), requested.clone(), requested);

    let filesystem = support
        .declared_evidence()
        .iter()
        .find(|evidence| evidence.dimension() == EnforcementDimension::FilesystemRead)
        .unwrap();
    assert_eq!(filesystem.scope(), EnforcementScope::DescendantTree);
    assert_eq!(filesystem.mechanism(), "test mechanism");
    assert_eq!(filesystem.limitations(), &["test evidence only"]);
    assert_eq!(
        support.observed_evidence().len(),
        support.declared_evidence().len()
    );
}

#[test]
fn missing_dimension_metadata_fails_before_spawn() {
    struct Backend {
        support: ContainmentSupport,
        preparations: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, _request: &ExecutionRequest) -> ContainmentSupport {
            self.support.clone()
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.preparations.set(self.preparations.get() + 1);
            unreachable!()
        }
    }

    let request = ExecutionRequest::builder("node").build().unwrap();
    let required = EnforcementDimensions::requested_by(request.policy());
    let mut support = support_with_evidence(required.clone(), required.clone(), required);
    support
        .declared_evidence
        .retain(|evidence| evidence.dimension() != EnforcementDimension::DescriptorHygiene);
    let backend = Backend {
        support,
        preparations: std::cell::Cell::new(0),
    };
    assert_eq!(
        execute_with_backend(&request, &backend)
            .unwrap_err()
            .category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert_eq!(backend.preparations.get(), 0);
}

#[cfg(target_os = "macos")]
#[test]
fn backend_devices_are_explicit_typed_and_identity_checked() {
    let grant = resolve_runtime_grant(PathBuf::from("/dev/null"), FilesystemAccess::Read).unwrap();
    assert_eq!(grant.kind, FilesystemGrantKind::CharacterDevice);
    assert!(bind_canonical(&grant).is_ok());
    let mut changed = grant.clone();
    changed.path = PathBuf::from("/dev/random");
    assert!(bind_canonical(&changed).is_err());
    assert!(resolve_runtime_grant(PathBuf::from("/dev/zero"), FilesystemAccess::Read).is_err());
}

#[cfg(unix)]
#[test]
fn project_policy_rejects_devices_and_special_files() {
    use std::os::unix::net::UnixListener;
    let root = temporary_directory("special-files");
    let socket = PathBuf::from(format!("/tmp/tapid-special-{}.sock", std::process::id()));
    let _listener = UnixListener::bind(&socket).unwrap();
    for path in [Path::new("/dev/null"), socket.as_path()] {
        for access in [FilesystemAccess::Read, FilesystemAccess::Write] {
            assert!(
                resolve_project_grant(Path::new("/"), path.to_str().unwrap(), access).is_err(),
                "accepted {path:?}"
            );
        }
    }
    fs::remove_file(socket).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn request_builder_preserves_an_explicit_validated_contract() {
    let request = ExecutionRequest::builder("node")
        .arg("script.js")
        .project_root("/project")
        .policy(required_policy())
        .env("NODE_ENV", "test")
        .build()
        .unwrap();

    assert_eq!(request.program(), "node");
    assert_eq!(request.arguments(), &["script.js"]);
    assert_eq!(request.project_root(), std::path::Path::new("/project"));
    assert_eq!(request.policy().mode(), SandboxMode::Required);
    assert_eq!(
        request.environment().get(std::ffi::OsStr::new("NODE_ENV")),
        Some(&std::ffi::OsString::from("test"))
    );
}

#[cfg(unix)]
#[test]
fn trusted_node_runtime_identity_is_retained_and_revalidated() {
    use std::os::unix::fs::PermissionsExt;

    let root = temporary_directory("trusted-node-runtime");
    let runtime = root.join("node");
    fs::write(&runtime, b"first").unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
    let runtime = fs::canonicalize(&runtime).unwrap();
    let runtime_dir = fs::canonicalize(&root).unwrap();
    let request = ExecutionRequest::builder("/bin/sh")
        .trusted_node_runtime(&runtime)
        .executable_search_path(&runtime_dir)
        .build()
        .unwrap();

    assert_eq!(
        request.trusted_node_runtime(),
        fs::canonicalize(&runtime).unwrap()
    );
    // Keep the original inode allocated so delete/recreate inode reuse on
    // Linux cannot accidentally turn this replacement test into an identity match.
    fs::rename(&runtime, root.join("original-node")).unwrap();
    fs::write(&runtime, b"replacement").unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();

    let error = request.validate_trusted_node_runtime().unwrap_err();
    assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn request_builder_preserves_windows_verbatim_boundary_contract() {
    let request = ExecutionRequest::builder("cmd.exe")
        .args(["/D", "/S", "/C", "echo ok"])
        .windows_verbatim_arguments(true)
        .build()
        .unwrap();

    assert!(request.uses_windows_verbatim_arguments());
}

#[test]
fn windows_verbatim_boundary_rejects_non_cmd_shapes_and_line_injection() {
    for request in [
        ExecutionRequest::builder("powershell.exe").args(["/D", "/S", "/C", "echo ok"]),
        ExecutionRequest::builder("cmd.exe").args(["/C", "echo ok"]),
        ExecutionRequest::builder("cmd.exe").args(["/D", "/S", "/C", "echo\rbreak"]),
        ExecutionRequest::builder("cmd.exe").args(["/D", "/S", "/C", "echo\nbreak"]),
    ] {
        assert!(request.windows_verbatim_arguments(true).build().is_err());
    }
}

#[test]
fn request_builder_preserves_ordered_executable_search_paths() {
    let request = ExecutionRequest::builder("node")
        .executable_search_paths(["/project/node_modules/.bin", "/runtime/bin"])
        .build()
        .unwrap();

    assert_eq!(
        request.executable_search_paths(),
        [
            PathBuf::from("/project/node_modules/.bin"),
            PathBuf::from("/runtime/bin")
        ]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn process_memory_stats_access_requires_linux_restricted_sandbox() {
    let default = ExecutionRequest::builder("node").build().unwrap();
    assert!(!default.allow_process_memory_stats());

    assert!(
        ExecutionRequest::builder("node")
            .policy(required_policy())
            .allow_process_memory_stats(true)
            .build()
            .is_err(),
        "ManagedTree must not accept the Restricted-only opt-in"
    );
    let disabled_restricted = SandboxPolicy::new_with_assurance(
        SandboxMode::Disabled,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    assert!(
        ExecutionRequest::builder("node")
            .policy(disabled_restricted)
            .allow_process_memory_stats(true)
            .build()
            .is_err(),
        "Disabled mode must not accept an opt-in that it cannot enforce"
    );

    let restricted = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let opted_in = ExecutionRequest::builder("node")
        .policy(restricted)
        .allow_process_memory_stats(true)
        .build()
        .unwrap();
    assert!(opted_in.allow_process_memory_stats());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn process_memory_stats_access_is_rejected_off_linux() {
    let error = ExecutionRequest::builder("node")
        .allow_process_memory_stats(true)
        .build()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("only by the Linux Restricted backend")
    );
}

#[test]
fn executable_search_path_payload_bounds_and_duplicates_are_rejected() {
    let at_count = (0..MAX_EXECUTABLE_SEARCH_PATH_COUNT)
        .map(|index| PathBuf::from(format!("/{index}")))
        .collect::<Vec<_>>();
    assert!(
        ExecutionRequest::builder("node")
            .executable_search_paths(at_count.clone())
            .build()
            .is_ok()
    );
    assert!(
        ExecutionRequest::builder("node")
            .executable_search_paths(at_count.into_iter().chain([PathBuf::from("/over")]))
            .build()
            .is_err()
    );

    let at_path_limit = format!("/{}", "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS - 1));
    assert!(
        ExecutionRequest::builder("node")
            .executable_search_path(&at_path_limit)
            .build()
            .is_ok()
    );
    assert!(
        ExecutionRequest::builder("node")
            .executable_search_path(format!("{at_path_limit}x"))
            .build()
            .is_err()
    );

    let mut at_total = (0..7)
        .map(|index| {
            let prefix = format!("/{index}/");
            PathBuf::from(format!(
                "{prefix}{}",
                "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS - prefix.len())
            ))
        })
        .collect::<Vec<_>>();
    let used = 7 * MAX_EXECUTABLE_SEARCH_PATH_UNITS + 7 + 1;
    let remainder = MAX_EXECUTABLE_SEARCH_PATHS_UNITS - used;
    at_total.push(PathBuf::from(format!(
        "/last/{}",
        "x".repeat(remainder - "/last/".len())
    )));
    assert!(validate_executable_search_paths(&at_total).is_ok());
    assert!(
        ExecutionRequest::builder("node")
            .executable_search_paths(at_total.clone())
            .build()
            .is_err()
    );
    at_total.last_mut().unwrap().as_mut_os_string().push("x");
    assert!(validate_executable_search_paths(&at_total).is_err());

    assert!(
        ExecutionRequest::builder("node")
            .executable_search_paths(["/same", "/same"])
            .build()
            .is_err()
    );
    assert!(
        ExecutionRequest::builder("node")
            .executable_search_path("/bad\0path")
            .build()
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn unix_search_directory_must_match_native_join_paths_semantics() {
    let paths = [PathBuf::from("/runtime/with:colon")];
    assert!(std::env::join_paths(&paths).is_err());

    let error = ExecutionRequest::builder("node")
        .executable_search_paths(paths)
        .build()
        .unwrap_err();
    assert_eq!(error.category(), ExecutionErrorCategory::InvalidRequest);
    assert!(error.to_string().contains("cannot be joined into PATH"));
}

#[test]
fn windows_join_paths_helper_matches_quoted_separator_semantics() {
    let first: Vec<u16> = r"C:\runtime\bin".encode_utf16().collect();
    let with_separator: Vec<u16> = r"C:\runtime;tools\bin".encode_utf16().collect();
    let joined = join_windows_path_units([first.as_slice(), with_separator.as_slice()]).unwrap();
    assert_eq!(
        String::from_utf16(&joined).unwrap(),
        r#"C:\runtime\bin;"C:\runtime;tools\bin""#
    );

    let with_quote: Vec<u16> = "C:\\bad\"path".encode_utf16().collect();
    assert!(join_windows_path_units([with_quote.as_slice()]).is_err());
}

#[test]
fn every_search_directory_is_bounded_and_joinable_before_duplicate_comparison() {
    let oversized = PathBuf::from(format!("/{}", "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS)));
    let error = ExecutionRequest::builder("node")
        .executable_search_paths([oversized.clone(), oversized])
        .build()
        .unwrap_err();
    assert!(error.to_string().contains("exceeds"), "{error}");
    assert!(!error.to_string().contains("duplicate"), "{error}");

    #[cfg(unix)]
    {
        let unjoinable = PathBuf::from("/bad:entry");
        let error = ExecutionRequest::builder("node")
            .executable_search_paths([unjoinable.clone(), unjoinable])
            .build()
            .unwrap_err();
        assert!(
            error.to_string().contains("cannot be joined into PATH"),
            "{error}"
        );
        assert!(!error.to_string().contains("duplicate"), "{error}");
    }
}

#[test]
fn windows_alias_helper_rejects_ascii_case_equivalent_paths() {
    let upper: Vec<u16> = r"C:\Runtime\BIN".encode_utf16().collect();
    let lower: Vec<u16> = r"c:\runtime\bin".encode_utf16().collect();
    assert!(windows_path_units_semantically_equal(&upper, &lower));
}

#[test]
fn required_mode_preflight_fails_before_a_child_can_create_a_marker() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "tapid-runner-preflight-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let marker = directory.join("child-spawned");

    #[cfg(unix)]
    let request = ExecutionRequest::builder("/bin/sh")
        .args(["-c".into(), format!("touch {}", marker.display())])
        .project_root(&directory)
        .policy(required_policy())
        .build()
        .unwrap();
    #[cfg(windows)]
    let request = ExecutionRequest::builder("cmd.exe")
        .args(["/C".into(), format!("type nul > \"{}\"", marker.display())])
        .project_root(&directory)
        .policy(required_policy())
        .build()
        .unwrap();

    let error = execute(&request).unwrap_err();
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert!(!marker.exists());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unsupported_preflight_makes_zero_prepare_attempts() {
    struct CountingBackend {
        prepare_attempts: std::cell::Cell<usize>,
    }

    impl ExecutionBackend for CountingBackend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            ContainmentSupport::unsupported(
                BackendIdentity {
                    name: "test/unsupported".into(),
                    version: "1".into(),
                    deprecation: None,
                },
                "test",
                "deliberately unavailable",
                EnforcementDimensions::requested_by(request.policy()),
                EnforcementDimensions::none(),
                EnforcementDimensions::none(),
            )
        }

        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.prepare_attempts.set(self.prepare_attempts.get() + 1);
            Err(PreparationError::from(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "must not be reached",
            )))
        }
    }

    let backend = CountingBackend {
        prepare_attempts: std::cell::Cell::new(0),
    };
    let request = ExecutionRequest::builder("node")
        .policy(required_policy())
        .build()
        .unwrap();
    let error = execute_with_backend(&request, &backend).unwrap_err();
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert_eq!(backend.prepare_attempts.get(), 0);
}

#[test]
fn malformed_supported_preflight_makes_zero_prepare_attempts() {
    struct CountingBackend {
        support: ContainmentSupport,
        prepare_attempts: std::cell::Cell<usize>,
    }

    impl ExecutionBackend for CountingBackend {
        fn containment_support(&self, _request: &ExecutionRequest) -> ContainmentSupport {
            self.support.clone()
        }

        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.prepare_attempts.set(self.prepare_attempts.get() + 1);
            Err(PreparationError::from(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "must not be reached",
            )))
        }
    }

    let request = ExecutionRequest::builder("node")
        .policy(required_policy())
        .build()
        .unwrap();
    let required = EnforcementDimensions::requested_by(request.policy());
    let mut requested_mismatch = required.clone();
    requested_mismatch.network = false;
    let mut missing_declared = required.clone();
    missing_declared.filesystem_write = false;
    let mut missing_observed = required.clone();
    missing_observed.descendant_lifecycle = false;

    for support in [
        support_with_evidence(requested_mismatch, required.clone(), required.clone()),
        support_with_evidence(required.clone(), missing_declared, required.clone()),
        support_with_evidence(required.clone(), required.clone(), missing_observed),
    ] {
        let backend = CountingBackend {
            support,
            prepare_attempts: std::cell::Cell::new(0),
        };
        let error = execute_with_backend(&request, &backend).unwrap_err();
        assert!(
            matches!(
                error.category(),
                ExecutionErrorCategory::PolicyViolation
                    | ExecutionErrorCategory::UnsupportedContainment
            ),
            "{error}"
        );
        assert_eq!(backend.prepare_attempts.get(), 0);
    }
}

#[test]
fn disabled_mode_makes_zero_prepare_attempts() {
    struct CountingBackend {
        prepare_attempts: std::cell::Cell<usize>,
    }

    impl ExecutionBackend for CountingBackend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }

        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.prepare_attempts.set(self.prepare_attempts.get() + 1);
            Err(PreparationError::from(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "must not be reached",
            )))
        }
    }

    let disabled = SandboxPolicy::new(
        SandboxMode::Disabled,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let request = ExecutionRequest::builder("node")
        .policy(disabled)
        .build()
        .unwrap();
    let backend = CountingBackend {
        prepare_attempts: std::cell::Cell::new(0),
    };

    let error = execute_with_backend(&request, &backend).unwrap_err();
    assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
    assert_eq!(backend.prepare_attempts.get(), 0);
}

#[test]
fn process_creation_error_cannot_escape_owned_cleanup() {
    struct Backend {
        process_creations: std::rc::Rc<std::cell::Cell<usize>>,
        cleanup_checks: std::rc::Rc<std::cell::Cell<usize>>,
    }
    struct Lifecycle {
        process_creations: std::rc::Rc<std::cell::Cell<usize>>,
        cleanup_checks: std::rc::Rc<std::cell::Cell<usize>>,
        cleanup_completion: CompletionEvidence,
    }
    impl ExecutionLifecycle for Lifecycle {
        fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
            self.process_creations.set(self.process_creations.get() + 1);
            Err(ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                "native process creation failed after creating a child",
            ))
        }
        fn cleanup(&mut self) -> CompletionEvidence {
            self.cleanup_checks.set(self.cleanup_checks.get() + 1);
            self.cleanup_completion.clone()
        }
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            Ok(OwnedExecutionAttempt::new(
                preflight,
                Box::new(Lifecycle {
                    process_creations: self.process_creations.clone(),
                    cleanup_checks: self.cleanup_checks.clone(),
                    cleanup_completion: completion_for(preflight),
                }),
            ))
        }
    }

    let process_creations = std::rc::Rc::new(std::cell::Cell::new(0));
    let cleanup_checks = std::rc::Rc::new(std::cell::Cell::new(0));
    let request = ExecutionRequest::builder("node")
        .policy(required_policy())
        .build()
        .unwrap();
    let error = execute_with_backend(
        &request,
        &Backend {
            process_creations: process_creations.clone(),
            cleanup_checks: cleanup_checks.clone(),
        },
    )
    .unwrap_err();

    assert_eq!(error.category(), ExecutionErrorCategory::Spawn);
    assert_eq!(process_creations.get(), 1);
    assert_eq!(cleanup_checks.get(), 1);
}

#[test]
fn post_spawn_failure_returns_only_after_checked_cleanup() {
    struct Backend {
        cleanup_checks: std::rc::Rc<std::cell::Cell<usize>>,
    }
    struct Lifecycle {
        cleanup_completion: Option<CompletionEvidence>,
        cleanup_checks: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl ExecutionLifecycle for Lifecycle {
        fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
            Err(ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                "wait failed",
            ))
        }
        fn cleanup(&mut self) -> CompletionEvidence {
            self.cleanup_checks.set(self.cleanup_checks.get() + 1);
            self.cleanup_completion.take().unwrap()
        }
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            let dimensions =
                EnforcementDimensions::completion_required(preflight.support.requested());
            let evidence = evidence_for_dimensions(&dimensions, "cleanup observation", &[]);
            let cleanup_completion = CompletionEvidence::checked(
                preflight,
                dimensions,
                evidence,
                CleanupConfidence::KernelOwnedComplete,
            )
            .unwrap();
            Ok(OwnedExecutionAttempt::new(
                preflight,
                Box::new(Lifecycle {
                    cleanup_completion: Some(cleanup_completion),
                    cleanup_checks: self.cleanup_checks.clone(),
                }),
            ))
        }
    }

    let cleanup_checks = std::rc::Rc::new(std::cell::Cell::new(0));
    let request = ExecutionRequest::builder("node")
        .policy(required_policy())
        .build()
        .unwrap();
    let error = execute_with_backend(
        &request,
        &Backend {
            cleanup_checks: cleanup_checks.clone(),
        },
    )
    .unwrap_err();

    assert_eq!(error.category(), ExecutionErrorCategory::Spawn);
    assert_eq!(cleanup_checks.get(), 1);
    assert_eq!(
        error.completion().unwrap().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
}

#[test]
fn public_outcome_contract_distinguishes_termination_and_enforcement() {
    fn inspect(outcome: &ExecutionOutcome) {
        let _: &Termination = outcome.termination();
        let _: &[u8] = outcome.stdout();
        let _: &[u8] = outcome.stderr();
        let receipt: &EnforcementReceipt = outcome.enforcement();
        let _: &ContainmentSupport = receipt.support();
        let _: &BackendIdentity = receipt.backend();
        let _: &EnforcementDimensions = receipt.requested();
        let _: &EnforcementDimensions = receipt.declared();
        let _: &EnforcementDimensions = receipt.observed();
        let _: &EnforcementDimensions = receipt.enforced();
        let _: &ResolvedFilesystemGrants = receipt.resolved_filesystem();
        let _: &ExecutionLimits = receipt.configured_limits();
    }
    let _ = inspect;
}

#[test]
fn containment_support_reports_backend_capabilities_without_claiming_execution() {
    let request = ExecutionRequest::builder("node")
        .policy(required_policy())
        .build()
        .unwrap();

    let support = platform_backend::containment_support(&request);
    assert!(!support.backend().name().is_empty());
    assert!(!support.backend().version().is_empty());
    #[cfg(target_os = "macos")]
    assert!(support.backend().deprecation().is_some());
    #[cfg(not(target_os = "macos"))]
    assert!(support.backend().deprecation().is_none());
    assert_eq!(support.enforceable(), &EnforcementDimensions::none());
    assert!(support.requested().filesystem_read());
    assert!(support.requested().filesystem_write());
    assert!(support.requested().network());
    assert!(support.requested().environment_sanitization());
    assert!(support.requested().descendant_lifecycle());
    assert!(!support.requested().resource_limits());
    assert!(support.unsupported_reason().is_some());
}

#[test]
fn requested_evidence_distinguishes_subprocess_denial_from_descendant_lifecycle() {
    let denied = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        false,
        ExecutionLimits::default(),
    )
    .unwrap();
    let allowed = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();

    let denied = EnforcementDimensions::requested_by(&denied);
    let allowed = EnforcementDimensions::requested_by(&allowed);
    assert!(denied.subprocess_restriction());
    assert!(!allowed.subprocess_restriction());
    assert!(denied.descendant_lifecycle());
    assert!(allowed.descendant_lifecycle());
}

#[test]
fn requested_resource_evidence_reports_each_configured_limit_independently() {
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::new(Some(1), None, Some(2), None).unwrap(),
    )
    .unwrap();
    let requested = EnforcementDimensions::requested_by(&policy);
    assert!(requested.timeout());
    assert!(!requested.output());
    assert!(requested.process_count());
    assert!(!requested.memory());
}

fn support_with_evidence(
    requested: EnforcementDimensions,
    declared: EnforcementDimensions,
    observed: EnforcementDimensions,
) -> ContainmentSupport {
    let declared_evidence =
        evidence_for_dimensions(&declared, "test mechanism", &["test evidence only"]);
    let observed_evidence =
        evidence_for_dimensions(&observed, "test mechanism", &["test evidence only"]);
    ContainmentSupport::supported(
        BackendIdentity {
            name: "test".into(),
            version: "1".into(),
            deprecation: None,
        },
        requested,
        declared,
        observed,
        declared_evidence,
        observed_evidence,
    )
}

fn receipt_for(
    preflight: &ValidatedPreflight,
    enforced: EnforcementDimensions,
) -> Result<EnforcementReceipt, ExecutionError> {
    let evidence = evidence_for_dimensions(&enforced, "test launch establishment", &[]);
    EnforcementReceipt::checked(preflight, enforced, evidence)
}

fn completion_for(preflight: &ValidatedPreflight) -> CompletionEvidence {
    let requested = preflight.support.requested().clone();
    let completion_required = EnforcementDimensions::completion_required(&requested);
    let confidence = if requested.process_tree_membership() {
        CleanupConfidence::KernelOwnedComplete
    } else {
        CleanupConfidence::NotGuaranteed
    };
    let evidence =
        evidence_for_dimensions(&completion_required, "test completion observation", &[]);
    CompletionEvidence::checked(preflight, completion_required, evidence, confidence).unwrap()
}

struct FinishedLifecycle {
    result: Option<Result<Box<ExecutionOutcome>, ExecutionError>>,
    cleanup: CompletionEvidence,
}

impl ExecutionLifecycle for FinishedLifecycle {
    fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
        self.result.take().expect("test lifecycle finishes once")
    }

    fn cleanup(&mut self) -> CompletionEvidence {
        self.cleanup.clone()
    }
}

fn finished_attempt<'a>(
    preflight: &'a ValidatedPreflight,
    outcome: ExecutionOutcome,
) -> OwnedExecutionAttempt<'a> {
    let cleanup = outcome.completion().clone();
    OwnedExecutionAttempt::new(
        preflight,
        Box::new(FinishedLifecycle {
            result: Some(Ok(Box::new(outcome))),
            cleanup,
        }),
    )
}

struct OutcomeBackend {
    termination: Termination,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    preparations: std::cell::Cell<usize>,
}

impl ExecutionBackend for OutcomeBackend {
    fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
        let requested = EnforcementDimensions::requested_by(request.policy());
        support_with_evidence(requested.clone(), requested.clone(), requested)
    }

    fn prepare<'a>(
        &'a self,
        _request: &ExecutionRequest,
        preflight: &'a ValidatedPreflight,
    ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
        self.preparations.set(self.preparations.get() + 1);
        let receipt = receipt_for(preflight, preflight.support.requested().clone()).unwrap();
        let outcome = ExecutionOutcome::checked(
            self.termination.clone(),
            self.stdout.clone(),
            self.stderr.clone(),
            receipt,
            completion_for(preflight),
        )
        .unwrap();
        Ok(finished_attempt(preflight, outcome))
    }
}

fn policy_with_limits(limits: ExecutionLimits) -> SandboxPolicy {
    SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        limits,
    )
    .unwrap()
}

fn assert_outcome_rejected_after_spawn(policy: SandboxPolicy, backend: OutcomeBackend) {
    let request = ExecutionRequest::builder("node")
        .policy(policy)
        .build()
        .unwrap();
    assert_eq!(
        execute_with_backend(&request, &backend)
            .unwrap_err()
            .category(),
        ExecutionErrorCategory::PolicyViolation
    );
    assert_eq!(backend.preparations.get(), 1);
}

#[test]
fn backend_output_over_exact_preflight_limit_is_rejected_after_spawn() {
    assert_outcome_rejected_after_spawn(
        policy_with_limits(ExecutionLimits::new(None, Some(3), None, None).unwrap()),
        OutcomeBackend {
            termination: Termination::Exited(0),
            stdout: vec![1, 2],
            stderr: vec![3, 4],
            preparations: std::cell::Cell::new(0),
        },
    );
}

#[test]
fn backend_timeout_without_exact_preflight_limit_is_rejected_after_spawn() {
    assert_outcome_rejected_after_spawn(
        policy_with_limits(ExecutionLimits::default()),
        OutcomeBackend {
            termination: Termination::TimedOut,
            stdout: vec![],
            stderr: vec![],
            preparations: std::cell::Cell::new(0),
        },
    );
}

#[test]
fn backend_output_termination_without_exact_preflight_limit_is_rejected_after_spawn() {
    assert_outcome_rejected_after_spawn(
        policy_with_limits(ExecutionLimits::default()),
        OutcomeBackend {
            termination: Termination::OutputLimitExceeded,
            stdout: vec![],
            stderr: vec![],
            preparations: std::cell::Cell::new(0),
        },
    );
}

#[test]
fn backend_process_termination_without_exact_preflight_limit_is_rejected_after_spawn() {
    assert_outcome_rejected_after_spawn(
        policy_with_limits(ExecutionLimits::default()),
        OutcomeBackend {
            termination: Termination::ProcessLimitExceeded,
            stdout: vec![],
            stderr: vec![],
            preparations: std::cell::Cell::new(0),
        },
    );
}

#[test]
fn backend_memory_termination_without_exact_preflight_limit_is_rejected_after_spawn() {
    assert_outcome_rejected_after_spawn(
        policy_with_limits(ExecutionLimits::default()),
        OutcomeBackend {
            termination: Termination::MemoryLimitExceeded,
            stdout: vec![],
            stderr: vec![],
            preparations: std::cell::Cell::new(0),
        },
    );
}

#[test]
fn backend_terminations_coherent_with_exact_preflight_limits_are_accepted() {
    let cases = [
        (
            Termination::Exited(0),
            ExecutionLimits::new(None, Some(3), None, None).unwrap(),
            vec![1, 2],
            vec![3],
        ),
        (
            Termination::TimedOut,
            ExecutionLimits::new(Some(1), None, None, None).unwrap(),
            vec![],
            vec![],
        ),
        (
            Termination::OutputLimitExceeded,
            ExecutionLimits::new(None, Some(1), None, None).unwrap(),
            vec![1],
            vec![],
        ),
        (
            Termination::ProcessLimitExceeded,
            ExecutionLimits::new(None, None, Some(1), None).unwrap(),
            vec![],
            vec![],
        ),
        (
            Termination::MemoryLimitExceeded,
            ExecutionLimits::new(None, None, None, Some(1)).unwrap(),
            vec![],
            vec![],
        ),
    ];

    for (termination, limits, stdout, stderr) in cases {
        let backend = OutcomeBackend {
            termination: termination.clone(),
            stdout,
            stderr,
            preparations: std::cell::Cell::new(0),
        };
        let request = ExecutionRequest::builder("node")
            .policy(policy_with_limits(limits))
            .build()
            .unwrap();
        let outcome = execute_with_backend(&request, &backend).unwrap();
        assert_eq!(outcome.termination(), &termination);
        assert_eq!(backend.preparations.get(), 1);
    }
}

fn preflight_for(policy: SandboxPolicy, support: ContainmentSupport) -> ValidatedPreflight {
    let request = ExecutionRequest::builder("node")
        .policy(policy)
        .build()
        .unwrap();
    let policy = resolve_policy(&request, RuntimeFilesystemAdditions::default()).unwrap();
    let bindings = FilesystemBindings::canonical_path(&policy).unwrap();
    ValidatedPreflight {
        support,
        policy,
        bindings,
        child_environment: request.child_environment(),
    }
}

fn temporary_directory(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "tapid-runner-{prefix}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn invalid_search_directories_fail_generic_preflight_before_spawn() {
    let root = temporary_directory("invalid-search-paths");
    let directory = root.join("directory");
    let file = root.join("file");
    fs::create_dir(&directory).unwrap();
    fs::write(&file, b"not a directory").unwrap();
    let canonical_root = fs::canonicalize(&root).unwrap();
    let canonical_directory = fs::canonicalize(&directory).unwrap();
    let noncanonical = noncanonical_parent_alias(&canonical_directory);
    let invalid = [
        PathBuf::from("relative"),
        canonical_root.join("missing"),
        fs::canonicalize(file).unwrap(),
        noncanonical,
    ];

    for path in invalid {
        let backend = OutcomeBackend {
            termination: Termination::Exited(0),
            stdout: vec![],
            stderr: vec![],
            preparations: std::cell::Cell::new(0),
        };
        let request = ExecutionRequest::builder("node")
            .project_root(&canonical_root)
            .executable_search_path(path)
            .build()
            .unwrap();
        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::PolicyViolation
        );
        assert_eq!(backend.preparations.get(), 0);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn search_directories_are_ordered_backend_runtime_receipt_grants() {
    struct Backend;
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            request: &ExecutionRequest,
            preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            assert_eq!(
                request.executable_search_paths(),
                [
                    preflight.policy.read[1].path.clone(),
                    preflight.policy.read[2].path.clone()
                ]
            );
            assert_eq!(
                preflight.child_environment.get(OsStr::new("PATH")),
                Some(
                    &std::env::join_paths(request.executable_search_paths())
                        .expect("validated search paths must join")
                )
            );
            let receipt = receipt_for(preflight, preflight.support.requested().clone()).unwrap();
            let outcome = ExecutionOutcome::checked(
                Termination::Exited(0),
                vec![],
                vec![],
                receipt,
                completion_for(preflight),
            )
            .unwrap();
            Ok(finished_attempt(preflight, outcome))
        }
    }

    let root = temporary_directory("search-path-root");
    let managed_bin = root.join("node_modules/.bin");
    let node_bin = root.join("selected-node/bin");
    fs::create_dir_all(&managed_bin).unwrap();
    fs::create_dir_all(&node_bin).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let managed_bin = fs::canonicalize(managed_bin).unwrap();
    let node_bin = fs::canonicalize(node_bin).unwrap();
    let request = ExecutionRequest::builder("node")
        .project_root(&root)
        .executable_search_paths([managed_bin.clone(), node_bin.clone()])
        .build()
        .unwrap();

    let outcome = execute_with_backend(&request, &Backend).unwrap();
    let runtime_grants = outcome
        .enforcement()
        .resolved_filesystem()
        .grants()
        .iter()
        .filter(|grant| grant.source() == FilesystemGrantSource::BackendRuntime)
        .collect::<Vec<_>>();
    assert_eq!(runtime_grants.len(), 2);
    assert_eq!(runtime_grants[0].path(), managed_bin);
    assert_eq!(runtime_grants[1].path(), node_bin);
    assert!(runtime_grants.iter().all(|grant| {
        grant.access() == FilesystemAccess::Read
            && grant.kind() == FilesystemGrantKind::DirectorySubtree
    }));
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn canonical_resolution_aliases_are_rejected_deterministically() {
    use std::os::unix::fs::symlink;

    struct Backend {
        probes: std::cell::Cell<usize>,
        preparations: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            self.probes.set(self.probes.get() + 1);
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.preparations.set(self.preparations.get() + 1);
            unreachable!()
        }
    }

    let root = temporary_directory("search-path-alias");
    let target = root.join("target");
    let first = root.join("first");
    let second = root.join("second");
    fs::create_dir(&target).unwrap();
    symlink(&target, &first).unwrap();
    symlink(&target, &second).unwrap();
    let request = ExecutionRequest::builder("node")
        .project_root(fs::canonicalize(&root).unwrap())
        .executable_search_paths([first, second])
        .build()
        .unwrap();
    let backend = Backend {
        probes: std::cell::Cell::new(0),
        preparations: std::cell::Cell::new(0),
    };

    let error = execute_with_backend(&request, &backend).unwrap_err();
    assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
    assert!(error.to_string().contains("alias"), "{error}");
    assert_eq!(backend.probes.get(), 1);
    assert_eq!(backend.preparations.get(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn resolved_preflight_is_the_only_source_of_receipt_filesystem_grants() {
    struct Backend {
        runtime: PathBuf,
    }

    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }

        fn runtime_filesystem_additions(
            &self,
            _request: &ExecutionRequest,
        ) -> Result<RuntimeFilesystemAdditions, ExecutionError> {
            RuntimeFilesystemAdditions::checked(vec![self.runtime.clone()], vec![])
        }

        fn bind_filesystem(
            &self,
            _request: &ExecutionRequest,
            policy: &ResolvedSandboxPolicy,
        ) -> Result<FilesystemBindings, ExecutionError> {
            for grant in &policy.write {
                if matches!(
                    grant.resolution,
                    GrantResolution::MissingWriteDirectory { .. }
                ) {
                    fs::create_dir_all(&grant.path).map_err(|error| {
                        path_error("materialize missing write subtree", &grant.path, error)
                    })?;
                }
            }
            FilesystemBindings::canonical_path(policy)
        }

        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            assert_eq!(preflight.policy.read.len(), 2);
            assert_eq!(preflight.policy.write.len(), 1);
            assert!(matches!(
                preflight.policy.write[0].resolution,
                GrantResolution::MissingWriteDirectory { .. }
            ));
            let enforced = preflight.support.requested().clone();
            let receipt = receipt_for(preflight, enforced).unwrap();
            let outcome = ExecutionOutcome::checked(
                Termination::Exited(0),
                vec![],
                vec![],
                receipt,
                completion_for(preflight),
            )
            .unwrap();
            Ok(finished_attempt(preflight, outcome))
        }
    }

    let root = temporary_directory("resolved-root");
    let runtime_root = temporary_directory("resolved-runtime");
    let runtime = runtime_root.join("node");
    fs::write(&runtime, b"runtime executable").unwrap();
    fs::create_dir(root.join("existing")).unwrap();
    let canonical_root = fs::canonicalize(&root).unwrap();
    let canonical_runtime = fs::canonicalize(&runtime).unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(
            vec!["existing".into()],
            vec!["generated/nested/output.txt".into()],
        )
        .unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let request = ExecutionRequest::builder("node")
        .project_root(&root)
        .policy(policy)
        .build()
        .unwrap();

    let outcome = execute_with_backend(
        &request,
        &Backend {
            runtime: canonical_runtime.clone(),
        },
    )
    .unwrap();
    let grants = outcome.enforcement().resolved_filesystem().grants();
    assert_eq!(grants.len(), 3);
    assert_eq!(grants[0].path(), canonical_root.join("existing"));
    assert_eq!(grants[0].kind(), FilesystemGrantKind::DirectorySubtree);
    assert_eq!(grants[0].source(), FilesystemGrantSource::ProjectPolicy);
    assert_eq!(grants[1].path(), canonical_runtime);
    assert_eq!(grants[1].source(), FilesystemGrantSource::BackendRuntime);
    assert_eq!(grants[1].kind(), FilesystemGrantKind::ExactFile);
    assert_eq!(
        grants[2].path(),
        canonical_root.join("generated/nested/output.txt")
    );
    assert_eq!(grants[2].access(), FilesystemAccess::Write);
    assert_eq!(grants[2].kind(), FilesystemGrantKind::DirectorySubtree);
    assert!(
        grants
            .iter()
            .all(|grant| grant.binding() == FilesystemBindingMode::CanonicalPath)
    );

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(runtime_root).unwrap();
}

#[test]
fn resolved_policy_rejects_omitted_or_extra_receipt_grants() {
    let policy = required_policy();
    let requested = EnforcementDimensions::requested_by(&policy);
    let support = support_with_evidence(requested.clone(), requested.clone(), requested.clone());
    let mut preflight = preflight_for(policy, support);

    let original = preflight.bindings.grants.pop().unwrap();
    assert!(preflight.bindings.validate(&preflight.policy).is_err());
    preflight.bindings.grants.push(BoundFilesystemGrant {
        receipt: ResolvedFilesystemGrant {
            path: PathBuf::from("/extra"),
            access: FilesystemAccess::Read,
            kind: FilesystemGrantKind::DirectorySubtree,
            source: FilesystemGrantSource::ProjectPolicy,
            binding: FilesystemBindingMode::CanonicalPath,
        },
        held: None,
        #[cfg(unix)]
        native_identity: None,
    });
    assert!(preflight.bindings.validate(&preflight.policy).is_err());
    preflight.bindings.grants.clear();
    preflight.bindings.grants.push(original);
}

fn noncanonical_parent_alias(canonical: &std::path::Path) -> PathBuf {
    // PathBuf::push/join normalizes parent components in Windows verbatim
    // paths. Append native text instead so the fixture remains noncanonical.
    let mut alias = canonical.as_os_str().to_owned();
    alias.push(std::path::MAIN_SEPARATOR_STR);
    alias.push("..");
    alias.push(std::path::MAIN_SEPARATOR_STR);
    alias.push(canonical.file_name().unwrap());
    let alias = PathBuf::from(alias);
    assert_ne!(alias.as_os_str(), canonical.as_os_str());
    alias
}

#[test]
fn noncanonical_runtime_additions_fail_before_spawn() {
    assert!(RuntimeFilesystemAdditions::checked(vec![PathBuf::from("relative")], vec![]).is_err());

    let root = temporary_directory("runtime-canonical");
    let canonical = fs::canonicalize(&root).unwrap();
    let noncanonical = noncanonical_parent_alias(&canonical);
    assert!(RuntimeFilesystemAdditions::checked(vec![noncanonical], vec![]).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn symlink_escape_in_project_grant_fails_before_spawn() {
    use std::os::unix::fs::symlink;

    struct Backend {
        prepare_attempts: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.prepare_attempts.set(self.prepare_attempts.get() + 1);
            Err(PreparationError::from(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "must not be reached",
            )))
        }
    }

    let root = temporary_directory("symlink-root");
    let outside = temporary_directory("symlink-outside");
    symlink(&outside, root.join("escape")).unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec!["escape".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let request = ExecutionRequest::builder("node")
        .project_root(&root)
        .policy(policy)
        .build()
        .unwrap();
    let backend = Backend {
        prepare_attempts: std::cell::Cell::new(0),
    };

    let error = execute_with_backend(&request, &backend).unwrap_err();
    assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
    assert_eq!(backend.prepare_attempts.get(), 0);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside).unwrap();
}

#[test]
fn support_models_declared_and_observed_evidence_separately() {
    let requested = EnforcementDimensions::requested_by(&required_policy());
    let mut declared = requested.clone();
    declared.timeout = true;
    let observed = requested.clone();
    let support = support_with_evidence(requested, declared.clone(), observed.clone());

    assert_eq!(support.declared(), &declared);
    assert_eq!(support.observed(), &observed);
    assert_eq!(support.enforceable(), &declared);
}

#[test]
fn checked_receipt_rejects_every_extra_or_missing_dimension() {
    let policy = required_policy();
    let requested = EnforcementDimensions::requested_by(&policy);

    let mut declared = requested.clone();
    declared.timeout = true;
    let support = support_with_evidence(requested.clone(), declared.clone(), declared);
    let preflight = preflight_for(policy.clone(), support);
    let mut extra_configured_by_neither_policy_nor_request = requested.clone();
    extra_configured_by_neither_policy_nor_request.timeout = true;
    assert!(receipt_for(&preflight, extra_configured_by_neither_policy_nor_request).is_err());

    let support = support_with_evidence(requested.clone(), requested.clone(), requested.clone());
    let preflight = preflight_for(policy, support);
    let mut extra_unsupported = requested.clone();
    extra_unsupported.timeout = true;
    assert!(receipt_for(&preflight, extra_unsupported).is_err());

    let mut missing = requested;
    missing.network = false;
    assert!(receipt_for(&preflight, missing).is_err());
}

#[test]
fn checked_receipt_requires_declared_and_observed_evidence() {
    let limits = ExecutionLimits::new(Some(1), Some(2), Some(3), Some(4)).unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        limits.clone(),
    )
    .unwrap();
    let requested = EnforcementDimensions::requested_by(&policy);

    let mut not_observed = requested.clone();
    not_observed.memory = false;
    let support = support_with_evidence(requested.clone(), requested.clone(), not_observed);
    let preflight = preflight_for(policy, support);
    assert!(receipt_for(&preflight, requested).is_err());
}

#[test]
fn checked_receipt_rejects_disabled_sandbox_policy() {
    let policy = SandboxPolicy::new(
        SandboxMode::Disabled,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let none = EnforcementDimensions::none();
    let support = support_with_evidence(none.clone(), none.clone(), none.clone());
    let preflight = preflight_for(policy, support);

    assert!(receipt_for(&preflight, none).is_err());
}

#[test]
fn receipt_keeps_launch_establishment_evidence_distinct_from_support_observation() {
    let policy = required_policy();
    let requested = EnforcementDimensions::requested_by(&policy);
    let support = support_with_evidence(requested.clone(), requested.clone(), requested.clone());
    let preflight = preflight_for(policy, support);
    let launch = evidence_for_dimensions(&requested, "launch establishment", &[]);

    let receipt = EnforcementReceipt::checked(&preflight, requested, launch).unwrap();

    assert!(
        receipt
            .established_evidence()
            .iter()
            .all(|evidence| evidence.mechanism() == "launch establishment")
    );
    assert_ne!(
        receipt.established_evidence(),
        receipt.support().observed_evidence()
    );
}

#[test]
fn completion_evidence_is_distinct_and_managed_tree_requires_complete_cleanup() {
    let policy = required_policy();
    let requested = EnforcementDimensions::requested_by(&policy);
    let support = support_with_evidence(requested.clone(), requested.clone(), requested.clone());
    let preflight = preflight_for(policy, support);
    let receipt = receipt_for(&preflight, requested.clone()).unwrap();
    assert!(!receipt.established_evidence().is_empty());

    let incomplete_dimensions = EnforcementDimensions::completion_required(&requested);
    let incomplete_evidence = evidence_for_dimensions(
        &incomplete_dimensions,
        "incomplete cleanup observation",
        &[],
    );
    let incomplete = CompletionEvidence::checked(
        &preflight,
        incomplete_dimensions,
        incomplete_evidence,
        CleanupConfidence::NotGuaranteed,
    )
    .unwrap();
    assert!(
        ExecutionOutcome::checked(
            Termination::Exited(0),
            vec![],
            vec![],
            receipt.clone(),
            incomplete,
        )
        .is_err()
    );

    let complete_dimensions = EnforcementDimensions::completion_required(&requested);
    let complete_evidence =
        evidence_for_dimensions(&complete_dimensions, "complete cleanup observation", &[]);
    let complete = CompletionEvidence::checked(
        &preflight,
        complete_dimensions,
        complete_evidence,
        CleanupConfidence::KernelOwnedComplete,
    )
    .unwrap();
    let outcome =
        ExecutionOutcome::checked(Termination::Exited(0), vec![], vec![], receipt, complete)
            .unwrap();
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
}

#[test]
fn restricted_completion_does_not_reconfirm_launch_dimensions() {
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let requested = EnforcementDimensions::requested_by(&policy);
    let support = support_with_evidence(requested.clone(), requested.clone(), requested.clone());
    let preflight = preflight_for(policy, support);

    let completion = CompletionEvidence::checked(
        &preflight,
        EnforcementDimensions::none(),
        vec![],
        CleanupConfidence::BestEffortObserved,
    )
    .unwrap();

    assert_eq!(completion.confirmed(), &EnforcementDimensions::none());
    assert!(completion.evidence().is_empty());
    assert_eq!(
        completion.cleanup_confidence(),
        CleanupConfidence::BestEffortObserved
    );
}

#[test]
fn managed_completion_confirms_only_lifecycle_and_cleanup_dimensions() {
    let policy = required_policy();
    let requested = EnforcementDimensions::requested_by(&policy);
    let support = support_with_evidence(requested.clone(), requested.clone(), requested.clone());
    let preflight = preflight_for(policy, support);
    let completion = completion_for(&preflight);

    assert!(completion.confirmed().descendant_lifecycle());
    assert!(completion.confirmed().process_tree_membership());
    assert!(completion.confirmed().complete_cleanup());
    assert!(!completion.confirmed().filesystem_read());
    assert!(!completion.confirmed().environment_sanitization());
    assert!(!completion.confirmed().descriptor_hygiene());
    assert_eq!(completion.evidence().len(), 3);
}

#[test]
fn completion_keeps_backend_observation_evidence() {
    let policy = required_policy();
    let requested = EnforcementDimensions::requested_by(&policy);
    let support = support_with_evidence(requested.clone(), requested.clone(), requested.clone());
    let preflight = preflight_for(policy, support);
    let confirmed = EnforcementDimensions::completion_required(&requested);
    let observed = evidence_for_dimensions(&confirmed, "completion observation", &[]);

    let completion = CompletionEvidence::checked(
        &preflight,
        confirmed,
        observed,
        CleanupConfidence::KernelOwnedComplete,
    )
    .unwrap();

    assert!(
        completion
            .evidence()
            .iter()
            .all(|evidence| evidence.mechanism() == "completion observation")
    );
}

#[test]
fn checked_outcome_accepts_success_only_with_a_complete_receipt() {
    let policy = required_policy();
    let requested = EnforcementDimensions::requested_by(&policy);
    let support = support_with_evidence(requested.clone(), requested.clone(), requested.clone());
    let preflight = preflight_for(policy, support);
    let receipt = receipt_for(&preflight, requested).unwrap();
    let completion = completion_for(&preflight);
    let outcome =
        ExecutionOutcome::checked(Termination::Exited(0), vec![], vec![], receipt, completion)
            .unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
}

#[test]
fn explicit_environment_must_be_named_and_allowlisted_by_policy() {
    let invalid = ExecutionRequest::builder("node")
        .env("BAD-NAME", "value")
        .build()
        .unwrap_err();
    assert_eq!(invalid.category(), ExecutionErrorCategory::InvalidRequest);

    let denied = ExecutionRequest::builder("node")
        .env("NODE_ENV", "test")
        .build()
        .unwrap_err();
    assert_eq!(denied.category(), ExecutionErrorCategory::InvalidRequest);

    let request = ExecutionRequest::builder("node")
        .policy(required_policy())
        .env("NODE_ENV", "test")
        .build()
        .unwrap();
    assert_eq!(request.environment().len(), 1);
}

#[test]
fn backend_identity_is_checked_and_bounded() {
    let at_limit = "x".repeat(MAX_BACKEND_IDENTITY_BYTES);
    assert!(BackendIdentity::new(at_limit.clone(), at_limit.clone(), Some(at_limit)).is_ok());
    for invalid in ["", "bad\0value", "bad\nvalue"] {
        assert_eq!(
            BackendIdentity::new(invalid, "1", None)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::InvalidRequest
        );
        assert!(BackendIdentity::new("backend", invalid, None).is_err());
        assert!(BackendIdentity::new("backend", "1", Some(invalid.into())).is_err());
    }
    let oversized = "x".repeat(MAX_BACKEND_IDENTITY_BYTES + 1);
    assert!(BackendIdentity::new(&oversized, "1", None).is_err());
    assert!(BackendIdentity::new("backend", &oversized, None).is_err());
    assert!(BackendIdentity::new("backend", "1", Some(oversized)).is_err());
}

#[test]
fn request_payload_limits_and_nul_are_enforced() {
    assert!(
        ExecutionRequest::builder("x".repeat(MAX_PROGRAM_UNITS))
            .build()
            .is_ok()
    );
    assert!(
        ExecutionRequest::builder("x".repeat(MAX_PROGRAM_UNITS + 1))
            .build()
            .is_err()
    );
    assert!(ExecutionRequest::builder("bad\0program").build().is_err());
    assert!(
        ExecutionRequest::builder("node")
            .args(std::iter::repeat_n("x", MAX_ARGUMENT_COUNT))
            .build()
            .is_ok()
    );
    assert!(
        ExecutionRequest::builder("node")
            .args(std::iter::repeat_n("x", MAX_ARGUMENT_COUNT + 1))
            .build()
            .is_err()
    );
    assert!(
        ExecutionRequest::builder("node")
            .arg("x".repeat(MAX_ARGUMENT_UNITS + 1))
            .build()
            .is_err()
    );
    assert!(
        ExecutionRequest::builder("node")
            .arg("bad\0arg")
            .build()
            .is_err()
    );
    #[cfg(not(windows))]
    {
        let exact_argv = [
            "x".repeat(MAX_ARGUMENT_UNITS),
            "y".repeat(MAX_ARGV_UNITS - MAX_ARGUMENT_UNITS - 4),
        ];
        assert!(
            ExecutionRequest::builder("p")
                .args(exact_argv.clone())
                .build()
                .is_ok()
        );
        let mut oversized_argv = exact_argv;
        oversized_argv[1].push('y');
        assert!(
            ExecutionRequest::builder("p")
                .args(oversized_argv)
                .build()
                .is_err()
        );
        let cumulative = std::iter::repeat_n("x".repeat(MAX_ARGUMENT_UNITS), 2);
        assert!(
            ExecutionRequest::builder("node")
                .args(cumulative)
                .build()
                .is_err()
        );
    }

    let policy = required_policy();
    assert!(
        ExecutionRequest::builder("node")
            .policy(policy.clone())
            .env("NODE_ENV", "x".repeat(MAX_ENVIRONMENT_VALUE_UNITS + 1))
            .build()
            .is_err()
    );
    assert!(
        ExecutionRequest::builder("node")
            .policy(policy.clone())
            .env("NODE_ENV", "bad\0value")
            .build()
            .is_err()
    );
    assert!(
        ExecutionRequest::builder("node")
            .policy(policy)
            .env("PATH", "/untrusted")
            .build()
            .is_err()
    );
}

#[test]
fn project_root_limit_and_nul_are_rejected_before_support_probing() {
    struct Backend {
        probes: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            self.probes.set(self.probes.get() + 1);
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            unreachable!()
        }
    }

    assert!(
        ExecutionRequest::builder("node")
            .project_root("x".repeat(MAX_PROJECT_ROOT_UNITS))
            .build()
            .is_ok()
    );
    assert!(
        ExecutionRequest::builder("node")
            .project_root("x".repeat(MAX_PROJECT_ROOT_UNITS + 1))
            .build()
            .is_err()
    );
    assert!(
        ExecutionRequest::builder("node")
            .project_root("bad\0root")
            .build()
            .is_err()
    );

    for invalid_root in [
        OsString::from("x".repeat(MAX_PROJECT_ROOT_UNITS + 1)),
        OsString::from("bad\0root"),
    ] {
        let backend = Backend {
            probes: std::cell::Cell::new(0),
        };
        let mut request = ExecutionRequest::builder("node").build().unwrap();
        request.project_root = PathBuf::from(invalid_root);
        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::InvalidRequest
        );
        assert_eq!(backend.probes.get(), 0);
    }
}

#[test]
fn windows_command_line_bound_rejects_the_prior_raw_unit_limit() {
    let prior_raw_limit = 1 + 1 + 16_382 + 1 + 16_381 + 1;
    assert_eq!(prior_raw_limit, MAX_ARGV_UNITS);
    assert!(validate_windows_command_line_units(1, [16_382, 16_381]).is_err());
}

#[test]
fn windows_command_line_bound_accepts_at_limit_conservative_cases() {
    assert_eq!(
        windows_command_line_units_upper_bound(4_096, [12_284]),
        Some(MAX_ARGV_UNITS - 1)
    );
    assert!(validate_windows_command_line_units(4_096, [12_284]).is_ok());
    assert_eq!(
        windows_command_line_units_upper_bound(1, [8_190, 8_188]),
        Some(MAX_ARGV_UNITS)
    );
    assert!(validate_windows_command_line_units(1, [8_190, 8_188]).is_ok());
    assert!(validate_windows_command_line_units(1, [8_190, 8_189]).is_err());
}

#[test]
fn windows_command_line_bound_counts_full_worst_case_serialization() {
    // Quoted program + separator-delimited quoted arguments + terminating NUL, with every
    // input unit budgeted for worst-case quote/backslash expansion.
    assert_eq!(
        windows_command_line_units_upper_bound(3, [4, 5]),
        Some((2 * 3 + 2) + 1 + (2 * 4 + 2) + 1 + (2 * 5 + 2) + 1)
    );
}

#[test]
fn captured_output_accounting_rejects_length_overflow() {
    assert!(checked_captured_output_bytes(usize::MAX, 1).is_err());
    assert_eq!(checked_captured_output_bytes(2, 3).unwrap(), 5);
}

#[cfg(unix)]
#[test]
fn unix_request_preserves_non_utf8_values_but_rejects_nul() {
    use std::os::unix::ffi::OsStringExt;
    let opaque_program = OsString::from_vec(vec![b'.', b'/', 0xff]);
    let opaque_argument = OsString::from_vec(vec![0xfe, b'x']);
    let opaque_root = OsString::from_vec(vec![b'.', b'/', 0xfd]);
    let opaque_environment = OsString::from_vec(vec![0xfc, b'x']);
    let request = ExecutionRequest::builder(opaque_program.clone())
        .arg(opaque_argument.clone())
        .project_root(PathBuf::from(opaque_root.clone()))
        .policy(required_policy())
        .env("NODE_ENV", opaque_environment.clone())
        .build()
        .unwrap();
    assert_eq!(request.program(), opaque_program);
    assert_eq!(request.arguments(), &[opaque_argument]);
    assert_eq!(request.project_root(), Path::new(&opaque_root));
    assert_eq!(
        request.environment().get(OsStr::new("NODE_ENV")),
        Some(&opaque_environment)
    );
    assert!(
        ExecutionRequest::builder("node")
            .arg(OsString::from_vec(vec![b'x', 0, b'y']))
            .build()
            .is_err()
    );
}

#[test]
fn receipt_grants_expose_declared_kind_source_access_and_binding() {
    fn inspect(grant: &ResolvedFilesystemGrant) {
        let _: &Path = grant.path();
        let _: FilesystemAccess = grant.access();
        let _: FilesystemGrantKind = grant.kind();
        let _: FilesystemGrantSource = grant.source();
        let _: FilesystemBindingMode = grant.binding();
    }
    let _ = inspect;
}

#[test]
fn missing_read_grant_fails_before_backend_binding_or_spawn() {
    struct Backend {
        bind_attempts: std::cell::Cell<usize>,
        prepare_attempts: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn bind_filesystem(
            &self,
            _request: &ExecutionRequest,
            _policy: &ResolvedSandboxPolicy,
        ) -> Result<FilesystemBindings, ExecutionError> {
            self.bind_attempts.set(self.bind_attempts.get() + 1);
            Err(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "must not bind",
            ))
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.prepare_attempts.set(self.prepare_attempts.get() + 1);
            Err(PreparationError::from(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "must not prepare",
            )))
        }
    }
    let root = temporary_directory("missing-read");
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec!["absent".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let request = ExecutionRequest::builder("node")
        .project_root(&root)
        .policy(policy)
        .build()
        .unwrap();
    let backend = Backend {
        bind_attempts: std::cell::Cell::new(0),
        prepare_attempts: std::cell::Cell::new(0),
    };
    assert_eq!(
        execute_with_backend(&request, &backend)
            .unwrap_err()
            .category(),
        ExecutionErrorCategory::PolicyViolation
    );
    assert_eq!(backend.bind_attempts.get(), 0);
    assert_eq!(backend.prepare_attempts.get(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_search_path_payload_is_rejected_before_support_probing() {
    struct Backend {
        probes: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            self.probes.set(self.probes.get() + 1);
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            unreachable!()
        }
    }

    let mut malformed = vec![
        vec![PathBuf::from(format!(
            "/{}",
            "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS)
        ))],
        vec![PathBuf::from("/bad\0path")],
        vec![PathBuf::from("/same"), PathBuf::from("/same")],
        (0..=MAX_EXECUTABLE_SEARCH_PATH_COUNT)
            .map(|index| PathBuf::from(format!("/{index}")))
            .collect(),
        (0..9)
            .map(|index| {
                let prefix = format!("/{index}/");
                PathBuf::from(format!(
                    "{prefix}{}",
                    "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS - prefix.len())
                ))
            })
            .collect(),
    ];
    #[cfg(unix)]
    malformed.push(vec![PathBuf::from("/bad:entry")]);
    #[cfg(windows)]
    malformed.push(vec![PathBuf::from("C:\\bad\"entry")]);
    for executable_search_paths in malformed {
        let backend = Backend {
            probes: std::cell::Cell::new(0),
        };
        let mut request = ExecutionRequest::builder("node").build().unwrap();
        request.executable_search_paths = executable_search_paths;
        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::InvalidRequest
        );
        assert_eq!(backend.probes.get(), 0);
    }
}

#[test]
fn malformed_request_is_rejected_before_support_probing() {
    struct Backend {
        probes: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            self.probes.set(self.probes.get() + 1);
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            unreachable!()
        }
    }

    let backend = Backend {
        probes: std::cell::Cell::new(0),
    };
    let mut request = ExecutionRequest::builder("node").build().unwrap();
    request.arguments.push(OsString::from("bad\0argument"));

    assert_eq!(
        execute_with_backend(&request, &backend)
            .unwrap_err()
            .category(),
        ExecutionErrorCategory::InvalidRequest
    );
    assert_eq!(backend.probes.get(), 0);
}

#[test]
fn invalid_backend_identity_prevents_runtime_additions_and_spawn() {
    struct Backend {
        additions: std::cell::Cell<usize>,
        preparations: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            let mut support =
                support_with_evidence(requested.clone(), requested.clone(), requested);
            support.backend.name.clear();
            support
        }
        fn runtime_filesystem_additions(
            &self,
            _request: &ExecutionRequest,
        ) -> Result<RuntimeFilesystemAdditions, ExecutionError> {
            self.additions.set(self.additions.get() + 1);
            Ok(RuntimeFilesystemAdditions::default())
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.preparations.set(self.preparations.get() + 1);
            unreachable!()
        }
    }
    let backend = Backend {
        additions: std::cell::Cell::new(0),
        preparations: std::cell::Cell::new(0),
    };
    let request = ExecutionRequest::builder("node").build().unwrap();
    assert_eq!(
        execute_with_backend(&request, &backend)
            .unwrap_err()
            .category(),
        ExecutionErrorCategory::PolicyViolation
    );
    assert_eq!(backend.additions.get(), 0);
    assert_eq!(backend.preparations.get(), 0);
}

#[test]
fn missing_write_requires_adapter_materialization_and_never_prepares_by_default() {
    struct Backend {
        preparations: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.preparations.set(self.preparations.get() + 1);
            unreachable!()
        }
    }
    let root = temporary_directory("missing-write-unsupported");
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec!["generated/output.txt".into()]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let request = ExecutionRequest::builder("node")
        .project_root(&root)
        .policy(policy)
        .build()
        .unwrap();
    let backend = Backend {
        preparations: std::cell::Cell::new(0),
    };
    assert_eq!(
        execute_with_backend(&request, &backend)
            .unwrap_err()
            .category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert_eq!(backend.preparations.get(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn native_object_binding_holds_identity_and_receipt_reports_it() {
    let root = temporary_directory("native-binding");
    let request = ExecutionRequest::builder("node")
        .project_root(&root)
        .build()
        .unwrap();
    let policy = resolve_policy(&request, RuntimeFilesystemAdditions::default()).unwrap();
    let bindings = FilesystemBindings::native_objects(&policy).unwrap();
    bindings.validate(&policy).unwrap();
    let receipt = bindings.receipt();
    assert_eq!(receipt.grants().len(), 1);
    assert_eq!(
        receipt.grants()[0].binding(),
        FilesystemBindingMode::NativeObject
    );
    assert!(bindings.grants[0].held.is_some());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn complete_environment_block_limit_counts_mandatory_path() {
    let exact_policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec!["ONE".into()],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let final_block_terminator = 1;
    let empty_path_entry = "PATH".len() + 1 + 1;
    let explicit_entry_framing = "ONE".len() + 1 + 1;
    let exact_value_units = MAX_ENVIRONMENT_BLOCK_UNITS
        - final_block_terminator
        - empty_path_entry
        - explicit_entry_framing;
    assert!(
        ExecutionRequest::builder("node")
            .policy(exact_policy.clone())
            .env("ONE", "x".repeat(exact_value_units))
            .build()
            .is_ok()
    );
    assert!(
        ExecutionRequest::builder("node")
            .policy(exact_policy)
            .env("ONE", "x".repeat(exact_value_units + 1))
            .build()
            .is_err()
    );

    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec!["ONE".into(), "TWO".into()],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let request = ExecutionRequest::builder("node")
        .policy(policy)
        .env("ONE", "x".repeat(MAX_ENVIRONMENT_VALUE_UNITS / 2))
        .env("TWO", "y".repeat(MAX_ENVIRONMENT_VALUE_UNITS / 2));
    assert!(request.build().is_err());
}

#[test]
fn complete_environment_combined_bound_includes_joined_path_and_has_no_off_by_one() {
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec!["ONE".into()],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let path = PathBuf::from("/bin");
    let path_units = os_units(path.as_os_str());
    let path_entry_units = "PATH".len() + 1 + path_units + 1;
    let explicit_entry_framing = "ONE".len() + 1 + 1;
    let exact_value_units =
        MAX_ENVIRONMENT_BLOCK_UNITS - 1 - path_entry_units - explicit_entry_framing;

    assert!(
        ExecutionRequest::builder("node")
            .policy(policy.clone())
            .executable_search_path(&path)
            .env("ONE", "x".repeat(exact_value_units))
            .build()
            .is_ok()
    );
    assert!(
        ExecutionRequest::builder("node")
            .policy(policy)
            .executable_search_path(path)
            .env("ONE", "x".repeat(exact_value_units + 1))
            .build()
            .is_err()
    );
}

#[test]
fn empty_search_list_produces_an_explicitly_empty_child_path() {
    struct Backend;
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }

        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            assert_eq!(
                preflight.child_environment.get(OsStr::new("PATH")),
                Some(&OsString::new())
            );
            assert_eq!(preflight.child_environment.len(), 1);
            let receipt = receipt_for(preflight, preflight.support.requested().clone()).unwrap();
            let outcome = ExecutionOutcome::checked(
                Termination::Exited(0),
                vec![],
                vec![],
                receipt,
                completion_for(preflight),
            )
            .unwrap();
            Ok(finished_attempt(preflight, outcome))
        }
    }

    let request = ExecutionRequest::builder("node").build().unwrap();
    execute_with_backend(&request, &Backend).unwrap();
}

#[test]
fn over_limit_complete_environment_is_rejected_before_support_probing() {
    struct Backend {
        probes: std::cell::Cell<usize>,
    }
    impl ExecutionBackend for Backend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            self.probes.set(self.probes.get() + 1);
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }
        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            unreachable!()
        }
    }

    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec!["ONE".into()],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let mut request = ExecutionRequest::builder("node")
        .policy(policy)
        .env("ONE", "ok")
        .build()
        .unwrap();
    request.environment.insert(
        OsString::from("ONE"),
        OsString::from("x".repeat(MAX_ENVIRONMENT_BLOCK_UNITS)),
    );
    let backend = Backend {
        probes: std::cell::Cell::new(0),
    };

    assert_eq!(
        execute_with_backend(&request, &backend)
            .unwrap_err()
            .category(),
        ExecutionErrorCategory::InvalidRequest
    );
    assert_eq!(backend.probes.get(), 0);
}

#[cfg(target_os = "macos")]
#[test]
fn native_restricted_backend_executes_an_allowed_project_operation() {
    let root = std::env::temp_dir().join(format!(
        "tapid-macos-restricted-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(&root).unwrap();
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let request = ExecutionRequest::builder("/bin/sh")
        .args([
            "-c",
            "printf allowed > allowed.txt; printf stdout; printf stderr >&2",
        ])
        .project_root(&root)
        .policy(policy)
        .executable_search_path("/usr/bin")
        .build()
        .unwrap();

    let outcome = execute(&request).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert_eq!(outcome.stdout(), b"stdout");
    assert_eq!(outcome.stderr(), b"stderr");
    assert_eq!(fs::read(root.join("allowed.txt")).unwrap(), b"allowed");
    assert_eq!(
        outcome.enforcement().backend().name(),
        "tapid-runner/macos-seatbelt-restricted-experimental"
    );
    assert!(outcome.enforcement().backend().deprecation().is_some());
    let root_runtime = outcome
        .enforcement()
        .resolved_filesystem()
        .grants()
        .iter()
        .find(|grant| grant.path() == Path::new("/"))
        .expect("effective root directory-data grant must be receipted");
    assert_eq!(root_runtime.source(), FilesystemGrantSource::BackendRuntime);
    assert_eq!(root_runtime.binding(), FilesystemBindingMode::CanonicalPath);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn native_restricted_backend_denies_an_outside_write() {
    let root = std::env::temp_dir().join(format!(
        "tapid-macos-deny-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let outside = root.with_extension("outside");
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(&root).unwrap();
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let request = ExecutionRequest::builder("/bin/sh")
        .args(["-c", "printf denied > \"$1\"", "tapid-script"])
        .arg(&outside)
        .project_root(&root)
        .policy(policy)
        .executable_search_path("/usr/bin")
        .build()
        .unwrap();

    let outcome = execute(&request).unwrap();
    assert_ne!(outcome.termination(), &Termination::Exited(0));
    assert!(!outside.exists());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn native_write_grant_does_not_imply_read_authority() {
    let root = std::env::temp_dir().join(format!(
        "tapid-macos-write-only-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("secret"), b"hidden").unwrap();
    let root = fs::canonicalize(&root).unwrap();
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![], vec![".".into()]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::default(),
    )
    .unwrap();
    let request = ExecutionRequest::builder("/bin/sh")
        .args(["-c", "/bin/cat secret"])
        .project_root(&root)
        .policy(policy)
        .executable_search_path("/usr/bin")
        .build()
        .unwrap();

    let outcome = execute(&request).unwrap();
    assert_ne!(outcome.termination(), &Termination::Exited(0));
    fs::remove_dir_all(root).unwrap();
}
