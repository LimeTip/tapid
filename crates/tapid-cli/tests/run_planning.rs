#[cfg(target_os = "macos")]
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static TEST_LAUNCHER_INIT: extern "C" fn() = {
    extern "C" fn init() {
        tapid_runner::initialize_or_dispatch_private_launcher();
    }
    init
};

#[path = "../src/run.rs"]
#[allow(dead_code)]
mod run;

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tapid_runner::RunConfig;

static PROJECT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn project() -> (PathBuf, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "tapid-run-plan-{}-{}",
        std::process::id(),
        PROJECT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(path.join("node_modules/.bin")).unwrap();
    let runtime_dir = path.join("runtime/bin");
    fs::create_dir_all(&runtime_dir).unwrap();
    let runtime = runtime_dir.join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::write(&runtime, b"runtime").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
    }
    (path, runtime)
}

#[test]
fn prepared_request_uses_npm_shell_exact_arguments_and_controlled_search_directories() {
    let (project, runtime) = project();
    let config =
        RunConfig::parse_toml("[run.scripts.dev]\nenvironment = [\"PUBLIC_VALUE\"]\n").unwrap();
    let mut ambient = BTreeMap::new();
    ambient.insert("PUBLIC_VALUE".into(), "declared".into());
    ambient.insert("SECRET_TOKEN".into(), "must-not-escape".into());

    let prepared = run::prepare_execution_request(
        &project,
        "dev",
        &config,
        "node server.js",
        &[
            "--hostname".into(),
            "127.0.0.1".into(),
            "--port".into(),
            "4173".into(),
        ],
        run::HostExecutionEnvironment {
            node_runtime: Some(&runtime),
            path: None,
            allowlisted: &ambient,
        },
    )
    .unwrap();

    #[cfg(unix)]
    {
        assert_eq!(prepared.request().program(), "/bin/sh");
        assert_eq!(
            prepared.request().arguments(),
            [
                "-c",
                "node server.js \"$@\"",
                "tapid-script",
                "--hostname",
                "127.0.0.1",
                "--port",
                "4173"
            ]
        );
    }
    #[cfg(windows)]
    {
        assert_eq!(prepared.request().program(), "cmd.exe");
        assert_eq!(&prepared.request().arguments()[..3], ["/D", "/S", "/C"]);
    }
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        prepared.executable_search_directories(),
        [
            fs::canonicalize(runtime.parent().unwrap()).unwrap(),
            fs::canonicalize(project.join("node_modules/.bin")).unwrap(),
        ]
    );
    #[cfg(target_os = "macos")]
    {
        let paths = prepared.executable_search_directories();
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            fs::canonicalize(project.join("node_modules/.bin")).unwrap()
        );
        assert_eq!(
            paths[1],
            fs::canonicalize(runtime.parent().unwrap()).unwrap()
        );
    }

    assert_eq!(
        prepared
            .request()
            .environment()
            .get(OsStr::new("PUBLIC_VALUE")),
        Some(&OsString::from("declared"))
    );
    assert!(
        !prepared
            .request()
            .environment()
            .contains_key(OsStr::new("SECRET_TOKEN"))
    );
    assert!(
        !prepared
            .request()
            .environment()
            .contains_key(OsStr::new("PATH"))
    );

    fs::remove_dir_all(project).unwrap();
}

#[cfg(unix)]
#[test]
fn preexisting_managed_bin_symlink_outside_project_is_rejected() {
    use std::os::unix::fs::symlink;

    let (project, runtime) = project();
    let outside = project.with_extension("outside");
    fs::create_dir(&outside).unwrap();
    fs::remove_dir(project.join("node_modules/.bin")).unwrap();
    symlink(&outside, project.join("node_modules/.bin")).unwrap();
    let config = RunConfig::parse_toml("[run.scripts.dev]\n").unwrap();

    let error = run::prepare_execution_request(
        &project,
        "dev",
        &config,
        "node server.js",
        &[],
        run::HostExecutionEnvironment {
            node_runtime: Some(&runtime),
            path: None,
            allowlisted: &BTreeMap::new(),
        },
    )
    .unwrap_err();

    assert!(matches!(error, run::RunPreparationError::InvalidManagedBin));
    fs::remove_dir_all(project).unwrap();
    fs::remove_dir_all(outside).unwrap();
}

#[test]
fn relative_host_path_entry_cannot_select_a_project_controlled_node() {
    let (project, _) = project();
    let relative_runtime = PathBuf::from("target").join(format!(
        "tapid-relative-runtime-{}-{}",
        std::process::id(),
        PROJECT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&relative_runtime).unwrap();
    let runtime = relative_runtime.join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::write(&runtime, b"runtime").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = RunConfig::parse_toml("[run.scripts.dev]\n").unwrap();
    let error = run::prepare_execution_request(
        &project,
        "dev",
        &config,
        "node server.js",
        &[],
        run::HostExecutionEnvironment {
            node_runtime: None,
            path: Some(relative_runtime.as_os_str()),
            allowlisted: &BTreeMap::new(),
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        run::RunPreparationError::MissingNodeRuntime
    ));
    fs::remove_dir_all(project).unwrap();
    fs::remove_dir_all(relative_runtime).unwrap();
}

#[test]
fn arbitrary_executable_filename_is_not_accepted_as_node() {
    let (project, runtime) = project();
    let fake = runtime.with_file_name(if cfg!(windows) {
        "runtime.exe"
    } else {
        "runtime"
    });
    fs::rename(&runtime, &fake).unwrap();
    let config = RunConfig::parse_toml("[run.scripts.dev]\n").unwrap();

    let error = run::prepare_execution_request(
        &project,
        "dev",
        &config,
        "node server.js",
        &[],
        run::HostExecutionEnvironment {
            node_runtime: Some(&fake),
            path: None,
            allowlisted: &BTreeMap::new(),
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        run::RunPreparationError::InvalidNodeRuntime
    ));
    fs::remove_dir_all(project).unwrap();
}

#[test]
fn windows_cmd_arguments_match_promise_spawn_escaping() {
    let cases = [
        ("", "\"\""),
        ("plain", "plain"),
        ("two words", "^\"two^ words^\""),
        ("Grüße", "Grüße"),
        ("a\"b", "^\"a\\^\"b^\""),
        ("%PATH%", "^%PATH^%"),
        ("wow!", "wow^!"),
        ("a^b", "a^^b"),
        ("a&b", "a^&b"),
        ("a|b", "a^|b"),
        ("a<b", "a^<b"),
        ("a>b", "a^>b"),
        ("(a)", "^(a^)"),
        ("C:\\tail\\", "C:\\tail\\"),
        ("space tail\\", "^\"space^ tail\\\\^\""),
    ];
    for (input, expected) in cases {
        assert_eq!(
            run::windows_cmd_escape_argument(input, false).unwrap(),
            expected
        );
    }
    assert_eq!(
        run::windows_cmd_escape_argument("%PATH%", true).unwrap(),
        "^^^%PATH^^^%"
    );
    assert!(run::windows_cmd_escape_argument("line\rbreak", false).is_err());
    assert!(run::windows_cmd_escape_argument("line\nbreak", true).is_err());
}

#[test]
fn windows_initial_executable_selects_native_or_double_escape() {
    let (root, _) = project();
    let bin = root.join("windows-bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("native.exe"), b"native").unwrap();
    fs::write(bin.join("wrapper.cmd"), b"wrapper").unwrap();

    assert!(!run::windows_initial_command_double_escape(
        "native serve",
        std::slice::from_ref(&bin)
    ));
    assert!(run::windows_initial_command_double_escape(
        "wrapper serve",
        std::slice::from_ref(&bin)
    ));
    assert!(run::windows_initial_command_double_escape(
        "explicit.cmd serve",
        std::slice::from_ref(&bin)
    ));
    assert!(!run::windows_initial_command_double_escape(
        "\"wrapper\" serve",
        std::slice::from_ref(&bin)
    ));
    assert!(!run::windows_initial_command_double_escape(
        "\"wrapper.cmd\" serve",
        std::slice::from_ref(&bin)
    ));
    assert!(!run::windows_initial_command_double_escape(
        "wrapper\tserve",
        std::slice::from_ref(&bin)
    ));
    assert!(!run::windows_initial_command_double_escape(
        "native && wrapper.cmd",
        std::slice::from_ref(&bin)
    ));
    assert!(run::windows_initial_command_double_escape(
        "wrapper.cmd && native",
        &[bin]
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_allowlist_rejects_case_equivalent_duplicates() {
    let names = vec!["NODE_ENV".to_owned(), "Node_Env".to_owned()];
    assert!(matches!(
        run::validate_allowlisted_environment_names(&names, true),
        Err(run::RunPreparationError::DuplicateEnvironmentName(name)) if name == "Node_Env"
    ));
    assert!(run::validate_allowlisted_environment_names(&names, false).is_ok());
}

#[cfg(target_os = "macos")]
#[test]
fn local_bin_wins_over_runtime_tools_but_node_stays_verified() {
    use std::os::unix::fs::PermissionsExt;
    let (project, runtime) = project();
    let local = project.join("node_modules/.bin");
    for (path, contents) in [
        (
            runtime.clone(),
            "#!/bin/sh\ncase \"$1\" in */node_modules/.bin/pick) printf local-pick ;; *) printf verified-node ;; esac",
        ),
        (
            runtime.parent().unwrap().join("pick"),
            "#!/bin/sh\nprintf runtime-pick",
        ),
        (local.join("pick"), "#!/usr/bin/env node\nlocal executable"),
        (local.join("node"), "#!/bin/sh\nprintf hostile-node"),
    ] {
        fs::write(&path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = RunConfig::parse_toml("[run.scripts.dev]\nassurance = \"restricted\"\n").unwrap();
    for caller_path in ["/hostile", ""] {
        let prepared = run::prepare_execution_request(
            &project,
            "dev",
            &config,
            "pick; node",
            &[],
            run::HostExecutionEnvironment {
                node_runtime: Some(&runtime),
                path: Some(OsStr::new(caller_path)),
                allowlisted: &BTreeMap::new(),
            },
        )
        .unwrap();
        let outcome = run::execute_checked(&prepared).unwrap();
        assert_eq!(outcome.termination(), &tapid_runner::Termination::Exited(0));
        assert_eq!(outcome.stdout(), b"local-pickverified-node");
        use std::os::unix::ffi::OsStrExt;
        let evidence = outcome.enforcement().executable_resolution().unwrap();
        assert!(!evidence.caller_path_inherited);
        assert_eq!(evidence.path_order.len(), 3);
        assert_eq!(
            evidence.path_order[1],
            fs::canonicalize(&local).unwrap().as_os_str().as_bytes()
        );
        assert_eq!(
            evidence.path_order[2],
            fs::canonicalize(runtime.parent().unwrap())
                .unwrap()
                .as_os_str()
                .as_bytes()
        );
        assert_eq!(evidence.path, evidence.path_order.join(&b':'));
        let node = evidence.reserved_node.as_ref().unwrap();
        assert_eq!(node.mechanism, "byte-verified private snapshot");
        assert!(!node.cleanup_observed);
        assert!(node.limitations.contains("retained"));
        fs::remove_dir_all(std::path::Path::new(OsStr::from_bytes(
            &evidence.path_order[0],
        )))
        .unwrap();
    }
    fs::remove_dir_all(project).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn cli_receipt_reports_assurance_authority_and_completion_without_duplicate_output() {
    let (project, runtime) = project();
    fs::write(project.join("package.json"), r#"{"name":"receipt-test","version":"1.0.0","scripts":{"dev":"printf unique-child-output; printf unique-child-error >&2"}}"#).unwrap();
    fs::write(
        project.join("tapid.toml"),
        "[run.scripts.dev]\nassurance = \"restricted\"\n",
    )
    .unwrap();
    let mut receipts = Vec::new();
    for machine in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .args(["run", "dev", "--project-dir"])
            .arg(&project)
            .arg("--node-runtime")
            .arg(&runtime);
        if machine {
            command.arg("--receipt-json");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"unique-child-output");
        let text = String::from_utf8(output.stderr).unwrap();
        assert_eq!(text.matches("unique-child-error").count(), 1);
        for field in [
            "assurance",
            "Restricted",
            "mechanism",
            "scope",
            "limitations",
            "effective_filesystem",
            "ProjectPolicy",
            "BackendRuntime",
            "configured_limits",
            "completion",
            "cleanup_confidence",
        ] {
            assert!(text.contains(field), "missing {field}: {text}");
        }
        let mut value: serde_json::Value = if machine {
            serde_json::from_str(text.lines().last().unwrap()).unwrap()
        } else {
            serde_json::from_str(text.split_once("sandbox receipt: ").unwrap().1.trim()).unwrap()
        };
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["assurance"], "Restricted");
        let private = value["executable_resolution"]["reserved_node"]["private_path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect::<Vec<_>>();
        let directory = &private[..private.len() - b"/node".len()];
        assert_eq!(
            value["executable_resolution"]["reserved_node"]["cleanup_observed"],
            false
        );
        assert!(
            value["executable_resolution"]["reserved_node"]["limitations"]
                .as_str()
                .unwrap()
                .contains("retained")
        );
        fs::remove_dir_all(PathBuf::from(
            String::from_utf8(directory.to_vec()).unwrap(),
        ))
        .unwrap();
        // The per-launch random directory is the only difference permitted.
        fn normalize(value: &mut serde_json::Value, directory: &[u8]) {
            match value {
                serde_json::Value::String(s) => {
                    *s = s.replace(std::str::from_utf8(directory).unwrap(), "<reserved>");
                }
                serde_json::Value::Array(values) => {
                    let prefix = directory
                        .iter()
                        .map(|b| serde_json::json!(*b))
                        .collect::<Vec<_>>();
                    if values.starts_with(&prefix) {
                        values.splice(
                            ..prefix.len(),
                            b"<reserved>".iter().map(|b| serde_json::json!(*b)),
                        );
                    } else {
                        for item in values {
                            normalize(item, directory);
                        }
                    }
                }
                serde_json::Value::Object(values) => {
                    for item in values.values_mut() {
                        normalize(item, directory);
                    }
                }
                _ => {}
            }
        }
        normalize(&mut value, directory);
        value["executable_resolution"]["reserved_node"]["inode"] = serde_json::json!(0);
        receipts.push(value);
    }
    assert_eq!(
        receipts[0], receipts[1],
        "complete human/JSON receipts differ"
    );
    fs::remove_dir_all(project).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn cli_launch_failure_is_nonzero_without_receipt_and_cleans_reserved_directory() {
    let (project, runtime) = project();
    fs::write(
        project.join("package.json"),
        r#"{"name":"failed-launch","version":"1.0.0","scripts":{"dev":": > marker"}}"#,
    )
    .unwrap();
    fs::write(
        project.join("tapid.toml"),
        "[run.scripts.dev]\nassurance = \"restricted\"\n",
    )
    .unwrap();
    let temporary = project.join("private-tmp");
    fs::create_dir(&temporary).unwrap();
    for machine in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .args(["run", "dev", "--project-dir"])
            .arg(&project)
            .arg("--node-runtime")
            .arg(&runtime)
            .env("TMPDIR", &temporary);
        if machine {
            command.arg("--receipt-json");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("no enforcement receipt"), "{stderr}");
        assert!(!stderr.contains("schema_version"));
        assert!(!stderr.contains("sandbox receipt:"));
        assert!(!project.join("marker").exists());
        assert_eq!(fs::read_dir(&temporary).unwrap().count(), 0);
    }
    fs::remove_dir_all(project).unwrap();
}
