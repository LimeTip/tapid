#[path = "../src/run.rs"]
#[allow(dead_code)]
mod run;

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tapid_runner::RunConfig;

fn project() -> (PathBuf, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "tapid-run-plan-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
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
        &runtime,
        &ambient,
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
            fs::canonicalize(project.join("node_modules/.bin")).unwrap(),
            fs::canonicalize(runtime.parent().unwrap()).unwrap(),
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
