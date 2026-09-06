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
    assert_eq!(
        prepared.executable_search_directories(),
        [
            fs::canonicalize(runtime.parent().unwrap()).unwrap(),
            fs::canonicalize(project.join("node_modules/.bin")).unwrap(),
        ]
    );

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
