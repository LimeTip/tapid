use super::command_support::command_in_home;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Output,
};
use tapid_test_support::{TempHome, TempProject};

fn invoke(project: &Path, home: &Path, args: &[&str]) -> Output {
    let mut command = command_in_home(project, home, args, true);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command.output().unwrap()
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let directory = path.is_dir();
            entries.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                if directory {
                    None
                } else {
                    Some(fs::read(&path).unwrap())
                },
            );
            if directory {
                visit(root, &path, entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

#[test]
fn completions_bash_works_without_project_configuration() {
    let project = TempProject::new("completions").unwrap();
    let home = TempHome::new("completions").unwrap();
    project.write("package.json", b"invalid manifest").unwrap();
    project
        .write("tapid.toml", b"invalid configuration")
        .unwrap();
    let output = invoke(project.path(), home.path(), &["completions", "bash"]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let script = String::from_utf8(output.stdout).unwrap();
    assert!(script.contains("_tapid()"), "{script}");
}

#[test]
fn completions_supported_shells_preserve_discovery_and_visibility() {
    let project = TempProject::new("completion-shells").unwrap();
    let home = TempHome::new("completion-shells").unwrap();
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let generate = || invoke(project.path(), home.path(), &["completions", shell]);
        let output = generate();
        assert!(output.status.success(), "{shell}: {output:?}");
        assert!(output.stderr.is_empty());
        assert_eq!(
            output.stdout,
            generate().stdout,
            "{shell} must be deterministic"
        );
        let script = String::from_utf8(output.stdout).unwrap();
        assert!(!script.contains('\u{1b}'));
        for hidden in [
            "__tool-exec",
            "__verify-release-record",
            "__prepare-release-install",
        ] {
            assert!(
                !script.contains(hidden),
                "{shell} offers hidden command {hidden}"
            );
        }
        let expected: &[&str] = match shell {
            "bash" => &[
                "tapid__subcmd__manifest)\n            opts=\"-h --json --help validate help\"",
                "tapid,i)\n                cmd=\"tapid__subcmd__install\"",
                "--frozen",
                "--json",
            ],
            "zsh" => &["'validate:", "'i:", "--frozen", "--json"],
            "fish" => &[
                "manifest",
                "-a \"validate\"",
                "-a \"i\"",
                "-l frozen",
                "-l json",
            ],
            "powershell" => &[
                "'tapid;manifest'",
                "'validate', 'validate'",
                "'i', 'i'",
                "'--frozen'",
                "'--json'",
            ],
            _ => unreachable!(),
        };
        for token in expected {
            assert!(script.contains(token), "{shell} missing {token}");
        }
        match shell {
            "bash" => {
                let install = script
                    .split("tapid__subcmd__install)\n")
                    .nth(1)
                    .unwrap()
                    .split(";;")
                    .next()
                    .unwrap();
                assert!(install.contains("--frozen"));
                assert!(install.contains("--json"));
            }
            "zsh" => {
                let manifest = script
                    .split("_tapid__subcmd__manifest_commands() {")
                    .nth(1)
                    .unwrap()
                    .split('}')
                    .next()
                    .unwrap();
                assert!(manifest.contains("'validate:"));
                let alias = script
                    .split("\n(i)\n")
                    .nth(1)
                    .unwrap()
                    .split(";;")
                    .next()
                    .unwrap();
                assert!(alias.contains("--frozen"));
                assert!(alias.contains("--json"));
            }
            "fish" => {
                assert!(
                    script
                        .lines()
                        .any(|line| line.contains("using_subcommand manifest;")
                            && line.contains("-a \"validate\""))
                );
                assert!(script.lines().any(
                    |line| line.contains("using_subcommand i\"") && line.contains("-l frozen")
                ));
                assert!(
                    script
                        .lines()
                        .any(|line| line.contains("needs_command\"") && line.contains("-l json"))
                );
            }
            "powershell" => {
                let manifest = script
                    .split("'tapid;manifest' {")
                    .nth(1)
                    .unwrap()
                    .split("break")
                    .next()
                    .unwrap();
                assert!(manifest.contains("'validate', 'validate'"));
                let alias = script
                    .split("'tapid;i' {")
                    .nth(1)
                    .unwrap()
                    .split("break")
                    .next()
                    .unwrap();
                assert!(alias.contains("'--frozen'"));
                assert!(alias.contains("'--json'"));
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn completions_help_and_argument_errors_advertise_only_supported_shells() {
    let project = TempProject::new("completion-arguments").unwrap();
    let home = TempHome::new("completion-arguments").unwrap();
    let help = invoke(project.path(), home.path(), &["completions", "--help"]);
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(
        text.contains("[possible values: bash, zsh, fish, powershell]"),
        "{text}"
    );
    for args in [
        vec!["completions"],
        vec!["completions", "elvish"],
        vec!["completions", "unknown"],
    ] {
        let output = invoke(project.path(), home.path(), &args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let mut json_args = args;
        json_args.push("--json");
        let output = invoke(project.path(), home.path(), &json_args);
        assert_eq!(output.status.code(), Some(2));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["operation"], "parse");
        assert_eq!(result["errors"][0]["code"], "ARGUMENT_INVALID");
    }
}

#[test]
fn completions_closed_stdout_reports_a_controlled_failure() {
    let project = TempProject::new("completion-pipe").unwrap();
    let home = TempHome::new("completion-pipe").unwrap();
    let mut command = command_in_home(project.path(), home.path(), &["completions", "bash"], true);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(
        diagnostic.contains("error: cannot print completions:"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("panicked"));
}

#[test]
fn completions_leave_project_home_store_and_pending_recovery_untouched() {
    let project = TempProject::new("completion-state").unwrap();
    let home = TempHome::new("completion-state").unwrap();
    project.write("package.json", b"invalid manifest").unwrap();
    project.write("tapid.toml", b"invalid config").unwrap();
    project
        .write(
            ".tapid-lifecycle-journal.json",
            b"pending recovery sentinel",
        )
        .unwrap();
    project.write("tapid.lock", b"invalid lock").unwrap();
    project
        .write("node_modules/sentinel", b"installed tree")
        .unwrap();
    home.write(".npmrc", b"inert credential sentinel").unwrap();
    home.write(".config/tapid/config.toml", b"invalid home config")
        .unwrap();
    // command_in_home sets XDG_CACHE_HOME and LOCALAPPDATA to this home;
    // macOS instead derives its cache directory from HOME.
    let store_sentinel = if cfg!(target_os = "macos") {
        "Library/Caches/tapid/store/sentinel"
    } else {
        "tapid/store/sentinel"
    };
    home.write(store_sentinel, b"cached bytes").unwrap();
    let before = (tree(project.path()), tree(home.path()));
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let output = invoke(project.path(), home.path(), &["completions", shell]);
        assert!(output.status.success(), "{output:?}");
    }
    for args in [
        vec!["--json", "completions", "bash"],
        vec!["completions", "bash", "--json"],
    ] {
        let output = invoke(project.path(), home.path(), &args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["operation"], "completions");
        assert_eq!(result["errors"][0]["code"], "JSON_UNSUPPORTED_COMMAND");
        assert_eq!(result["changes"]["state"], "unchanged");
    }
    assert_eq!((tree(project.path()), tree(home.path())), before);
}
