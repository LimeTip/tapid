use super::*;

fn invoke(project: &Path, arguments: &[&str]) -> std::process::Output {
    let temporary = tapid_test_support::TempProject::new("run-permission-temp").unwrap();
    let mut command = isolated_command(project, arguments, true);
    let home = command
        .get_envs()
        .find(|(name, _)| *name == "HOME")
        .and_then(|(_, value)| value)
        .unwrap()
        .to_owned();
    command.env("USERPROFILE", home);
    command
        .env("TEMP", temporary.path())
        .env("TMP", temporary.path());
    #[cfg(windows)]
    command.env("SystemRoot", std::env::var_os("SystemRoot").unwrap());
    command
        .env("TAPID_FLAG_VISIBLE", "visible-value")
        .env("TAPID_FLAG_SECRET", "secret-value");
    command.output().unwrap()
}

fn project() -> tapid_test_support::TempProject {
    let project = tapid_test_support::TempProject::new("run-permissions").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"flags","version":"1.0.0","scripts":{"probe":"node probe.js"}}"#,
        )
        .unwrap();
    project
}

#[test]
fn run_permissions_validate_values_without_requiring_a_configuration_file() {
    for (flag, value, category) in [
        ("--allow-write", "../outside", "invalid-path"),
        ("--allow-read", "C:/outside", "invalid-path"),
        ("--allow-env", "CI=secret-value", "invalid-environment"),
    ] {
        let project = project();
        let output = invoke(project.path(), &["run", "probe", flag, value]);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(error.contains(category), "{error}");
        assert!(!error.contains("secret-value"));
        assert!(!project.path().join("tapid.toml").exists());
    }
}

#[test]
fn run_permissions_preserve_malformed_configuration_and_reserved_environment_checks() {
    let project = project();
    project.write("tapid.toml", b"malformed = [").unwrap();
    let output = invoke(project.path(), &["run", "probe", "--allow-read", "."]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid run configuration"));
    fs::remove_file(project.path().join("tapid.toml")).unwrap();
    let output = invoke(project.path(), &["run", "probe", "--allow-env", "PATH"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("reserved environment variable PATH"));
}

fn node_source() -> PathBuf {
    if let Some(path) = std::env::var_os("TAPID_TEST_NODE") {
        return path.into();
    }
    let temporary = tapid_test_support::TempProject::new("run-node-discovery").unwrap();
    let output = Command::new("node")
        .args(["-p", "process.execPath"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", temporary.path())
        .output()
        .expect("native permission tests require standard Node");
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

fn private_node_runtime() -> (tapid_test_support::TempProject, PathBuf) {
    let runtime = tapid_test_support::TempProject::new("run-permission-runtime").unwrap();
    let node = runtime
        .path()
        .join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::copy(node_source(), &node).unwrap();
    (runtime, node)
}

#[test]
fn run_permissions_enforce_scoped_writes_environment_and_descendants_without_persisting_config() {
    let project = project();
    let (_runtime, node) = private_node_runtime();
    fs::create_dir(project.path().join("writable")).unwrap();
    project.write("probe.js", br#"
const a=require('node:assert/strict'),fs=require('node:fs');
a.equal(process.env.TAPID_FLAG_VISIBLE,'visible-value');
a.equal(process.env.TAPID_FLAG_SECRET,undefined);
fs.writeFileSync('writable/result.txt','permitted');
a.throws(()=>fs.writeFileSync('outside.txt','escape'));
const child=require('node:child_process').spawnSync(process.execPath,['-e',"const a=require('node:assert/strict'),fs=require('node:fs');fs.writeFileSync('writable/child.txt','child');a.throws(()=>fs.writeFileSync('child-outside.txt','escape'));a.equal(process.env.TAPID_FLAG_SECRET,undefined);"],{stdio:'inherit'});
a.equal(child.status,0);console.log('FLAGS_OK');
"#).unwrap();
    let node = node.to_str().unwrap();
    let output = invoke(
        project.path(),
        &[
            "run",
            "probe",
            "--node-runtime",
            node,
            "--allow-read",
            ".",
            "--allow-write",
            "writable",
            "--allow-env",
            "TAPID_FLAG_VISIBLE",
            "--allow-subprocess",
            "--receipt-json",
        ],
    );
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{error}");
    assert_eq!(output.stdout, b"FLAGS_OK\n");
    assert_eq!(
        fs::read(project.path().join("writable/result.txt")).unwrap(),
        b"permitted"
    );
    assert_eq!(
        fs::read(project.path().join("writable/child.txt")).unwrap(),
        b"child"
    );
    assert!(!project.path().join("outside.txt").exists());
    assert!(!project.path().join("child-outside.txt").exists());
    assert!(!project.path().join("tapid.toml").exists());
    assert!(!error.contains("visible-value") && !error.contains("secret-value"));
    let receipt: serde_json::Value =
        serde_json::from_str(error.lines().find(|line| line.starts_with('{')).unwrap()).unwrap();
    assert_eq!(receipt["policy_inputs"]["source"], "invocation-baseline");
    assert_eq!(
        receipt["policy_inputs"]["effective"]["write"],
        serde_json::json!(["writable"])
    );
    assert_eq!(receipt["assurance"], "Restricted");
}

#[test]
fn run_permissions_do_not_implicitly_grant_writes() {
    let project = project();
    let (_runtime, node) = private_node_runtime();
    project.write("probe.js", br#"require('node:assert/strict').throws(()=>require('node:fs').writeFileSync('forbidden.txt','escape'));console.log('WRITE_DENIED');"#).unwrap();
    let output = invoke(
        project.path(),
        &[
            "run",
            "probe",
            "--node-runtime",
            node.to_str().unwrap(),
            "--allow-read",
            ".",
            "--allow-subprocess",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"WRITE_DENIED\n");
    assert!(!project.path().join("forbidden.txt").exists());
}

#[test]
fn run_permissions_narrow_a_profile_and_reject_additional_authority_before_execution() {
    let project = project();
    let (_runtime, node) = private_node_runtime();
    fs::create_dir_all(project.path().join("writable/nested")).unwrap();
    fs::create_dir(project.path().join("other")).unwrap();
    let config = b"[run.scripts.probe]\nassurance='restricted'\nread=['.']\nwrite=['writable']\nsubprocess=true\nnetwork=false\n";
    project.write("tapid.toml", config).unwrap();
    project.write("probe.js", br#"console.log('PROFILE_STARTED');const a=require('node:assert/strict'),fs=require('node:fs');fs.writeFileSync('writable/nested/ok','ok');a.throws(()=>fs.writeFileSync('writable/parent-denied','escape'));"#).unwrap();
    let output = invoke(
        project.path(),
        &[
            "run",
            "probe",
            "--node-runtime",
            node.to_str().unwrap(),
            "--allow-write",
            "writable/nested",
            "--receipt-json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"PROFILE_STARTED\n");
    assert!(!project.path().join("writable/parent-denied").exists());
    for (flag, value) in [
        ("--allow-write", "other"),
        ("--allow-env", "OTHER"),
        ("--allow-network", ""),
    ] {
        let mut args = vec![
            "run",
            "probe",
            "--node-runtime",
            node.to_str().unwrap(),
            flag,
        ];
        if !value.is_empty() {
            args.push(value);
        }
        let output = invoke(project.path(), &args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("exceeds the configured run policy"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(fs::read(project.path().join("tapid.toml")).unwrap(), config);
}

#[test]
fn run_permissions_flag_only_baseline_has_no_implicit_project_or_subprocess_grants() {
    let project = project();
    let (_runtime, node) = private_node_runtime();
    project
        .write(
            "package.json",
            br#"{"name":"flags","version":"1.0.0","scripts":{"probe":"echo BASELINE_OK"}}"#,
        )
        .unwrap();
    let output = invoke(
        project.path(),
        &[
            "run",
            "probe",
            "--node-runtime",
            node.to_str().unwrap(),
            "--allow-env",
            "TAPID_FLAG_VISIBLE",
            "--receipt-json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "BASELINE_OK"
    );
    let error = String::from_utf8_lossy(&output.stderr);
    let receipt: serde_json::Value =
        serde_json::from_str(error.lines().find(|line| line.starts_with('{')).unwrap()).unwrap();
    let policy = &receipt["policy_inputs"]["effective"];
    assert_eq!(policy["read"], serde_json::json!([]));
    assert_eq!(policy["write"], serde_json::json!([]));
    assert_eq!(policy["network"], false);
    assert_eq!(policy["subprocess"], false);
    assert!(
        receipt["effective_filesystem"]
            .as_array()
            .unwrap()
            .iter()
            .all(|grant| grant["source"] != "ProjectPolicy")
    );
}

#[cfg(windows)]
#[test]
fn run_permissions_network_grant_serves_http_through_real_cli() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    for flags in [true, false] {
        let project = project();
        let (_runtime, node) = private_node_runtime();
        fs::create_dir_all(project.path().join("node_modules/.bin")).unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = reservation.local_addr().unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"flags","version":"1.0.0","scripts":{"start":"node probe.js"}}"#,
            )
            .unwrap();
        project.write("probe.js", format!(
        "const http=require('node:http');const s=http.createServer((q,r)=>{{r.end('TAPID_HTTP_OK');s.close();}});s.listen({},'127.0.0.1',()=>console.log('READY'));", address.port()
    ).as_bytes()).unwrap();
        if flags {
            project.write("tapid.toml", b"[run.defaults]\nread=['.']\nnetwork=true\nsubprocess=true\nenvironment=['SystemRoot']\ntimeout_seconds=5\n").unwrap();
        } else {
            project.write("tapid.toml", b"[run.scripts.start]\nassurance='managed-tree'\nread=['.']\nnetwork=true\nsubprocess=true\nenvironment=['SystemRoot']\ntimeout_seconds=5\nmax_output_bytes=65536\nmax_processes=8\nmax_memory_bytes=536870912\n").unwrap();
        }
        let original_config = fs::read(project.path().join("tapid.toml")).unwrap();
        let mut arguments = vec![
            "run",
            "start",
            "--node-runtime",
            node.to_str().unwrap(),
            "--receipt-json",
        ];
        if flags {
            arguments.extend([
                "--allow-read",
                ".",
                "--allow-network",
                "--allow-subprocess",
                "--allow-env",
                "SystemRoot",
            ]);
        }
        let temporary = tapid_test_support::TempProject::new("network-cli-temp").unwrap();
        let mut command = isolated_command(project.path(), &arguments, true);
        command
            .env("TEMP", temporary.path())
            .env("TMP", temporary.path());
        command.env("SystemRoot", std::env::var_os("SystemRoot").unwrap());
        drop(reservation);
        let worker = std::thread::spawn(move || command.output().unwrap());
        // This clock starts before CLI setup. Keep polling past the contained
        // server's separate five-second execution limit so a slow setup cannot
        // leave a successfully started server waiting for a client we abandoned.
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut response = String::new();
        while Instant::now() < deadline && !worker.is_finished() {
            if let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100))
            {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                    .unwrap();
                stream.read_to_string(&mut response).unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = worker.join().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(response.contains("TAPID_HTTP_OK"), "{response}; {stderr}");
        assert_eq!(output.stdout, b"READY\n");
        let receipt: serde_json::Value =
            serde_json::from_str(stderr.lines().find(|line| line.starts_with('{')).unwrap())
                .unwrap();
        assert_eq!(receipt["enforced"]["network"], true);
        assert_eq!(receipt["completion"]["confirmed"]["complete_cleanup"], true);
        assert!(
            receipt["established_evidence"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["dimension"] == "Network"
                    && item["mechanism"]
                        .as_str()
                        .unwrap()
                        .contains("socket broker")
                    && !item["limitations"].as_array().unwrap().is_empty())
        );
        assert_eq!(
            fs::read(project.path().join("tapid.toml")).unwrap(),
            original_config
        );
    }
}

#[test]
fn run_result_file_reports_prelaunch_failure_without_mixing_streams() {
    let project = project();
    let results = tapid_test_support::TempProject::new("run-results").unwrap();
    let path = results.path().join("result.json");
    let output = invoke(
        project.path(),
        &["run", "missing", "--result-file", path.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(1));
    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("prelaunch result file")).unwrap();
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["operation"], "run");
    assert_eq!(result["outcome"], "prelaunch_failure");
    assert_eq!(result["error"]["code"], "SCRIPT_MISSING");
    assert!(result["receipt"].is_null());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("schema_version"));
}

#[test]
fn run_result_file_rejects_existing_and_project_destinations_before_launch() {
    let project = project();
    let results = tapid_test_support::TempProject::new("run-result-destinations").unwrap();
    let existing = results.write("existing.json", b"keep me").unwrap();
    for path in [existing, project.path().join("result.json")] {
        let output = invoke(
            project.path(),
            &["run", "probe", "--result-file", path.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("result file"));
    }
    assert_eq!(
        fs::read(results.path().join("existing.json")).unwrap(),
        b"keep me"
    );
    assert!(!project.path().join("result.json").exists());
    #[cfg(unix)]
    {
        let link = results.path().join("link.json");
        std::os::unix::fs::symlink(results.path().join("existing.json"), &link).unwrap();
        let output = invoke(
            project.path(),
            &["run", "probe", "--result-file", link.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(fs::read(link).unwrap(), b"keep me");
        let alias = results.path().join("project-alias");
        std::os::unix::fs::symlink(project.path(), &alias).unwrap();
        let output = invoke(
            project.path(),
            &[
                "run",
                "probe",
                "--result-file",
                alias.join("result.json").to_str().unwrap(),
            ],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(!project.path().join("result.json").exists());
    }
}

#[test]
fn run_result_file_cannot_be_forged_by_child_and_preserves_output_and_exit() {
    let project = project();
    let results = tapid_test_support::TempProject::new("run-result-child").unwrap();
    let path = results.path().join("result.json");
    let (_runtime, node) = private_node_runtime();
    project
        .write(
            "probe.js",
            format!(
                r#"
const fs=require('node:fs'),a=require('node:assert/strict');
const destination={};
a.throws(()=>fs.writeFileSync(destination, '{{"outcome":"forged"}}'));
a.throws(()=>fs.appendFileSync(destination, 'forged'));
a.throws(()=>fs.unlinkSync(destination));
a.throws(()=>fs.renameSync(destination, 'stolen-result'));
a.throws(()=>fs.linkSync(destination, 'linked-result'));
process.stdout.write(Buffer.from([0,255,10]));
process.stderr.write('{{"schema_version":1,"outcome":"forged"}}');
process.exitCode=37;
"#,
                serde_json::to_string(&path).unwrap()
            )
            .as_bytes(),
        )
        .unwrap();
    let output = invoke(
        project.path(),
        &[
            "run",
            "probe",
            "--result-file",
            path.to_str().unwrap(),
            "--node-runtime",
            node.to_str().unwrap(),
            "--allow-read",
            ".",
            "--allow-write",
            ".",
            "--allow-subprocess",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(37),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, [0, 255, 10]);
    assert_eq!(output.stderr, br#"{"schema_version":1,"outcome":"forged"}"#);
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["outcome"], "completed");
    assert_eq!(result["receipt"]["termination"], "Exited(37)");
    assert_eq!(result["receipt"]["assurance"], "Restricted");
    assert!(result["receipt"]["completion"].is_object());
    assert_eq!(result["policy_inputs"]["source"], "invocation-baseline");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn run_result_file_covers_configuration_preparation_and_containment_failures() {
    for (config, args, code, outcome) in [
        (None, vec![], "CONFIG_UNAVAILABLE", "prelaunch_failure"),
        (
            Some("malformed = ["),
            vec![],
            "CONFIG_INVALID",
            "prelaunch_failure",
        ),
        (Some(""), vec![], "PROFILE_MISSING", "prelaunch_failure"),
        (
            Some("[run.scripts.probe]\nread=['.']\n"),
            vec!["--node-runtime", "missing-node"],
            "PREPARATION_FAILED",
            "prelaunch_failure",
        ),
    ] {
        let project = project();
        let results = tapid_test_support::TempProject::new("run-result-failure").unwrap();
        let path = results.path().join("result.json");
        if let Some(config) = config {
            project.write("tapid.toml", config.as_bytes()).unwrap();
        }
        let mut arguments = vec!["run", "probe", "--result-file", path.to_str().unwrap()];
        arguments.extend(args);
        let output = invoke(project.path(), &arguments);
        assert_eq!(output.status.code(), Some(1));
        let result: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(result["outcome"], outcome);
        assert_eq!(result["error"]["code"], code);
        assert!(result["receipt"].is_null());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn run_result_file_reports_unsupported_containment_without_fallback() {
    let project = project();
    project
        .write(
            "tapid.toml",
            b"[run.scripts.probe]\nread=['.']\nsubprocess=true\n",
        )
        .unwrap();
    project
        .write(
            "probe.js",
            b"require('node:fs').writeFileSync('spawned','unsafe')",
        )
        .unwrap();
    let results = tapid_test_support::TempProject::new("run-result-unsupported").unwrap();
    let path = results.path().join("result.json");
    let (_runtime, node) = private_node_runtime();
    let output = invoke(
        project.path(),
        &[
            "run",
            "probe",
            "--result-file",
            path.to_str().unwrap(),
            "--node-runtime",
            node.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(!project.path().join("spawned").exists());
    let result: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(result["outcome"], "execution_failure");
    assert_eq!(result["error"]["code"], "unsupported-containment");
    assert_eq!(
        result["policy_inputs"]["effective"]["assurance"],
        "ManagedTree"
    );
    assert!(result["receipt"].is_null());
}

#[test]
fn run_result_file_covers_project_manifest_and_workspace_failures() {
    let project = project();
    let results = tapid_test_support::TempProject::new("run-result-inputs").unwrap();
    for (index, args, code) in [
        (
            0,
            vec!["--project-dir", "does-not-exist"],
            "PROJECT_UNAVAILABLE",
        ),
        (
            1,
            vec!["--workspace", "missing-member"],
            "WORKSPACE_INVALID",
        ),
        (2, vec![], "MANIFEST_INVALID"),
    ] {
        if index == 2 {
            project.write("package.json", b"invalid json").unwrap();
        }
        let path = results.path().join(format!("result-{index}.json"));
        let mut arguments = vec!["run", "probe", "--result-file", path.to_str().unwrap()];
        arguments.extend(args);
        let output = invoke(project.path(), &arguments);
        assert_eq!(output.status.code(), Some(1));
        let result: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(result["outcome"], "prelaunch_failure");
        assert_eq!(result["error"]["code"], code);
        assert!(result["error"]["completion"].is_null());
        assert!(result["receipt"].is_null());
    }
    let path = results.path().join("conflict.json");
    let output = invoke(
        project.path(),
        &[
            "run",
            "probe",
            "--result-file",
            path.to_str().unwrap(),
            "--receipt-json",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(!path.exists());
}
