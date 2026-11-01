use super::*;
use tapid_test_support::{TempHome, TempProject};

struct InstalledFixture {
    project: TempProject,
    home: TempHome,
    store: PathBuf,
}

impl InstalledFixture {
    fn new(script: &[u8], entry: &str) -> Self {
        let project = TempProject::new("exec-command").unwrap();
        let home = TempHome::new("exec-command").unwrap();
        let store_root = home.path().join("store");
        let store = tapid_store::Store::new(&store_root);
        let manifest = br#"{"name":"root","version":"1.0.0","dependencies":{"first":"npm:demo@1.0.0","second":"npm:other@1.0.0"}}"#;
        project.write("package.json", manifest).unwrap();
        let mut lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
        let mut bindings = std::collections::BTreeMap::new();
        for (install_name, name) in [("first", "demo"), ("second", "other")] {
            let source = TempProject::new("exec-package").unwrap();
            let command = if install_name == "first" {
                "probe"
            } else {
                "other-probe"
            };
            source
                .write(
                    "package.json",
                    format!(
                        r#"{{"name":"{name}","version":"1.0.0","bin":{{"{command}":"{entry}"}}}}"#
                    )
                    .as_bytes(),
                )
                .unwrap();
            source.write(entry, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(source.path().join(entry), fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            let digest = tapid_archive::canonical_tree_digest(source.path()).unwrap();
            store
                .activate_verified_tree(&digest.parse().unwrap(), source.path())
                .unwrap();
            let package = tapid_lockfile::LockedPackage::new_with_provenance(
                "https://registry.npmjs.org",
                name,
                "1.0.0",
                &format!("sha512-{}==", "A".repeat(86)),
                &digest,
                tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
            )
            .unwrap();
            bindings.insert(install_name.to_owned(), package.key());
            lock.insert_package(package).unwrap();
        }
        lock.set_roots(bindings.values().cloned()).unwrap();
        lock.set_root_bindings(bindings).unwrap();
        project
            .write("tapid.lock", lock.to_json().unwrap().as_bytes())
            .unwrap();
        let fixture = Self {
            project,
            home,
            store: store_root,
        };
        let output = fixture
            .command(&[
                "install",
                "--frozen",
                "--offline",
                "--store-dir",
                fixture.store.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fixture
    }

    fn command(&self, args: &[&str]) -> Command {
        command_support::command_in_home(self.project.path(), self.home.path(), args, true)
    }

    fn invoke(&self, extra: &[&str], command: &str) -> (std::process::Output, serde_json::Value) {
        let destination = self.home.path().join("result.json");
        let _ = fs::remove_file(&destination);
        let mut args = vec![
            "exec",
            "--store-dir",
            self.store.to_str().unwrap(),
            "--result-file",
            destination.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        args.extend(["--", command]);
        let output = self.command(&args).output().unwrap();
        let bytes = fs::read(&destination).unwrap();
        assert!(bytes.ends_with(b"\n"));
        (output, serde_json::from_slice(&bytes).unwrap())
    }
}

#[test]
fn exec_selection_is_offline_qualified_and_reports_typed_failures_without_changes() {
    let fixture = InstalledFixture::new(b"console.log('not launched');\n", "cli.js");
    let before = fs::read(fixture.project.path().join("tapid.lock")).unwrap();
    for (options, command, expected) in [
        (vec![], "missing", "COMMAND_MISSING"),
        (
            vec!["--package", "transitive"],
            "probe",
            "PACKAGE_INELIGIBLE",
        ),
        (
            vec!["--package", "first", "--node-runtime", "missing-node"],
            "probe",
            "PREPARATION_FAILED",
        ),
    ] {
        let (output, result) = fixture.invoke(&options, command);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(result["operation"], "exec");
        assert_eq!(result["error"]["code"], expected, "{result}");
        assert!(result["receipt"].is_null());
        if expected == "PREPARATION_FAILED" {
            assert_eq!(result["owner"]["name"], "demo");
            assert_eq!(result["owner"]["install_name"], "first");
            assert_eq!(result["owner"]["kind"], "authenticated-package");
            assert_eq!(result["owner"]["version"], "1.0.0");
        }
    }
    assert_eq!(
        fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
        before
    );
    assert!(!fixture.project.path().join("tapid.toml").exists());
    assert!(!fixture.project.path().join("marker").exists());
}

#[test]
fn exec_rejects_foreign_changed_and_missing_evidence_without_repair() {
    let fixture = InstalledFixture::new(b"console.log('must not launch');\n", "cli.js");
    let lock = fs::read(fixture.project.path().join("tapid.lock")).unwrap();
    let marker = fixture.project.path().join(".tapid-managed");
    let marker_bytes = fs::read(&marker).unwrap();
    fs::remove_file(&marker).unwrap();
    let (output, result) = fixture.invoke(&["--package", "first"], "probe");
    assert!(!output.status.success());
    assert_eq!(result["error"]["code"], "INSTALLATION_INCONSISTENT");
    assert!(!marker.exists());
    fs::write(&marker, marker_bytes).unwrap();

    let retained_store = fixture.home.path().join("retained-store");
    fs::rename(&fixture.store, &retained_store).unwrap();
    let (output, result) = fixture.invoke(&["--package", "first"], "probe");
    assert!(!output.status.success());
    assert_eq!(result["error"]["code"], "INSTALLATION_INCONSISTENT");
    assert!(!fixture.store.exists());
    fs::rename(retained_store, &fixture.store).unwrap();

    fs::write(
        fixture.project.path().join("node_modules/first/cli.js"),
        b"malicious replacement",
    )
    .unwrap();
    let (output, result) = fixture.invoke(&["--package", "first"], "probe");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(result["error"]["code"], "INSTALLATION_INCONSISTENT");
    assert!(result["owner"].is_null());
    assert!(result["receipt"].is_null());
    assert_eq!(
        fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
        lock
    );
    assert_eq!(
        fs::read(fixture.project.path().join("node_modules/first/cli.js")).unwrap(),
        b"malicious replacement"
    );
}

#[test]
fn exec_unsupported_executable_is_rejected_before_runtime_discovery() {
    let fixture = InstalledFixture::new(b"unsupported executable\n", "native.bin");
    let (output, result) = fixture.invoke(&["--package", "first"], "probe");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(result["error"]["code"], "BIN_EXECUTABLE_UNSUPPORTED");
    assert!(result["receipt"].is_null());
}

#[test]
fn exec_native_preserves_arguments_streams_exit_and_exact_permission_boundaries() {
    let fixture = InstalledFixture::new(br#"
const a=require('node:assert/strict'),fs=require('node:fs');
a.deepEqual(process.argv.slice(2),['','a b','$(touch marker)','; echo forged','--json']);
a.equal(process.env.TAPID_EXEC_VISIBLE,'visible');
a.equal(process.env.TAPID_EXEC_SECRET,undefined);
fs.writeFileSync('writable/result','allowed');
a.throws(()=>fs.writeFileSync('outside','denied'));
const child=require('node:child_process').spawnSync(process.execPath,['-e',"require('node:fs').writeFileSync('writable/child','bad')"]);
a.notEqual(child.status,0);
process.stdout.write('EXACT_STDOUT');process.stderr.write('EXACT_STDERR');process.exit(37);
"#, "cli.js");
    fs::create_dir(fixture.project.path().join("writable")).unwrap();
    let runtime = TempProject::new("exec-runtime").unwrap();
    let node = runtime
        .path()
        .join(if cfg!(windows) { "node.exe" } else { "node" });
    let source = std::env::var_os("TAPID_TEST_NODE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let output = Command::new("node")
                .args(["-p", "process.execPath"])
                .output()
                .unwrap();
            assert!(output.status.success());
            PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        });
    fs::copy(source, &node).unwrap();
    let result_path = fixture.home.path().join("native-result.json");
    let output = fixture
        .command(&[
            "exec",
            "--store-dir",
            fixture.store.to_str().unwrap(),
            "--package",
            "first",
            "--node-runtime",
            node.to_str().unwrap(),
            "--result-file",
            result_path.to_str().unwrap(),
            "--allow-read",
            ".",
            "--allow-write",
            "writable",
            "--allow-env",
            "TAPID_EXEC_VISIBLE",
            "--",
            "probe",
            "",
            "a b",
            "$(touch marker)",
            "; echo forged",
            "--json",
        ])
        .env("TAPID_EXEC_VISIBLE", "visible")
        .env("TAPID_EXEC_SECRET", "secret")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(37),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"EXACT_STDOUT");
    assert_eq!(output.stderr, b"EXACT_STDERR");
    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(result_path).unwrap()).unwrap();
    assert_eq!(result["outcome"], "completed");
    assert_eq!(result["owner"]["name"], "demo");
    assert!(result["receipt"].is_object());
    assert_eq!(
        fs::read(fixture.project.path().join("writable/result")).unwrap(),
        b"allowed"
    );
    for path in ["marker", "outside", "writable/child", "tapid.toml"] {
        assert!(!fixture.project.path().join(path).exists(), "{path}");
    }
}
