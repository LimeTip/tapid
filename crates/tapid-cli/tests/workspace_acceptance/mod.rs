use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tapid_test_support::{TempHome, TempProject};

struct Fixture {
    project: TempProject,
    home: TempHome,
}

impl Fixture {
    fn new() -> Self {
        let project = TempProject::new("workspace-reference").unwrap();
        for (path, bytes) in [
            (
                "package.json",
                include_bytes!("../fixtures/workspace/package.json").as_slice(),
            ),
            (
                "probe.js",
                include_bytes!("../fixtures/workspace/probe.js").as_slice(),
            ),
            (
                "apps/news/package.json",
                include_bytes!("../fixtures/workspace/apps/news/package.json").as_slice(),
            ),
            (
                "apps/news/probe.js",
                include_bytes!("../fixtures/workspace/apps/news/probe.js").as_slice(),
            ),
            (
                "packages/ui/package.json",
                include_bytes!("../fixtures/workspace/packages/ui/package.json").as_slice(),
            ),
            (
                "packages/ui/index.js",
                include_bytes!("../fixtures/workspace/packages/ui/index.js").as_slice(),
            ),
            (
                "registry.json",
                include_bytes!("../fixtures/workspace/registry.json").as_slice(),
            ),
        ] {
            project.write(path, bytes).unwrap();
        }
        Self {
            project,
            home: TempHome::new("workspace-reference").unwrap(),
        }
    }

    fn command(&self, args: &[&str], online: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .args(args)
            .arg("--store-dir")
            .arg(self.home.path().join("store"))
            .current_dir(self.project.path())
            .env_clear()
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path());
        for key in ["PATH", "SystemRoot", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        if online {
            command
                .arg("--registry-fixture")
                .arg(self.project.path().join("registry.json"));
        }
        command.output().unwrap()
    }

    fn succeeds(&self, args: &[&str], online: bool) {
        let output = self.command(args, online);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn assert_imports(&self, phase: &str) {
        let root = self.project.path();
        assert_eq!(
            fs::canonicalize(root.join("node_modules/@example/ui")).unwrap(),
            fs::canonicalize(root.join("packages/ui")).unwrap()
        );
        assert!(!root.join("apps/news/tapid.lock").exists());
        assert!(!root.join("apps/news/node_modules").exists());
        for (cwd, marker) in [
            (root.to_path_buf(), "ROOT_WORKSPACE_OK"),
            (root.join("apps/news"), "NEWS_WORKSPACE_OK"),
        ] {
            if let Some(output) = super::node_assertion_output(
                Command::new("node").arg("probe.js").current_dir(cwd),
                std::env::var_os("TAPID_REQUIRE_NODE_ASSERTIONS").is_some(),
                "workspace-reference",
            ) {
                assert!(
                    output.status.success(),
                    "{phase}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), marker);
                eprintln!(
                    "TAPID_NODE_ASSERTION_OK workspace-reference phase={phase} output={marker}"
                );
            }
        }
        let lock: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("tapid.lock")).unwrap()).unwrap();
        assert_eq!(lock["workspacePackages"].as_object().unwrap().len(), 2);
        assert_eq!(lock["packages"].as_object().unwrap().len(), 1);
        let news = lock["workspacePackages"]
            .as_object()
            .unwrap()
            .values()
            .find(|package| package["source"]["name"] == "news")
            .unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("apps/news/package.json")).unwrap())
                .unwrap();
        if manifest["dependencies"].get("@example/ui").is_some() {
            assert!(
                news["dependencies"]["@example/ui"]
                    .as_str()
                    .unwrap()
                    .contains("workspace:packages/ui:@example/ui@1.2.0")
            );
        } else {
            assert!(news["dependencies"].get("@example/ui").is_none());
        }
        assert!(
            news["dependencies"]["external"]
                .as_str()
                .unwrap()
                .contains("external@1.0.0")
        );
    }
}

#[test]
fn workspace_reference_install_replay_and_lifecycle_keep_node_imports() {
    let fixture = Fixture::new();
    fixture.succeeds(&["install"], true);
    fixture.assert_imports("root-online");
    let root_manifest = fs::read(fixture.project.path().join("package.json")).unwrap();
    let lock = fs::read(fixture.project.path().join("tapid.lock")).unwrap();
    fixture.succeeds(&["install", "--workspace", "news"], true);
    assert_eq!(
        fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
        lock
    );
    fixture.assert_imports("member-online");

    // Replay must rebuild the links and registry tree, without access to fixture metadata.
    fs::remove_file(fixture.project.path().join("registry.json")).unwrap();
    for selector in [vec![], vec!["--workspace", "news"]] {
        for flag in ["--offline", "--frozen"] {
            fs::remove_dir_all(fixture.project.path().join("node_modules")).unwrap();
            let mut args = vec!["install", flag];
            args.extend_from_slice(&selector);
            fixture.succeeds(&args, false);
            assert_eq!(
                fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
                lock
            );
            fixture.assert_imports(flag);
        }
    }
    fixture
        .project
        .write(
            "registry.json",
            include_bytes!("../fixtures/workspace/registry.json"),
        )
        .unwrap();
    for selector in [vec![], vec!["--workspace", "news"]] {
        let unaffected = if selector.is_empty() {
            "apps/news/package.json"
        } else {
            "package.json"
        };
        let expected = fs::read(fixture.project.path().join(unaffected)).unwrap();
        for command in [
            vec!["add", "@example/ui@^1.0.0"],
            vec!["update", "@example/ui"],
            vec!["remove", "@example/ui"],
            vec!["add", "@example/ui@^1.0.0"],
            vec!["prune"],
        ] {
            let mut args = command;
            args.extend_from_slice(&selector);
            fixture.succeeds(&args, args[0] != "prune");
            // Removing the declaration does not remove a discovered workspace link.
            fixture.assert_imports(args[0]);
            assert_eq!(
                fs::read(fixture.project.path().join(unaffected)).unwrap(),
                expected
            );
        }
    }
    assert_ne!(
        fs::read(fixture.project.path().join("package.json")).unwrap(),
        root_manifest
    );
}

// Capture file bytes and link targets without traversing workspace symlinks.
fn snapshot(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let relative = path.strip_prefix(base).unwrap().to_path_buf();
        if metadata.file_type().is_symlink() {
            result.insert(
                relative,
                format!("link:{}", fs::read_link(path).unwrap().display()).into_bytes(),
            );
        } else if metadata.is_dir() {
            result.insert(relative, b"directory".to_vec());
            for entry in fs::read_dir(path).unwrap() {
                visit(base, &entry.unwrap().path(), result);
            }
        } else {
            result.insert(relative, fs::read(path).unwrap());
        }
    }
    let mut result = BTreeMap::new();
    visit(path, path, &mut result);
    result
}

#[test]
fn unsupported_workspace_glob_preserves_manifests_lock_store_and_activation() {
    let fixture = Fixture::new();
    fixture.succeeds(&["install"], true);
    let root = fixture.project.path();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("package.json")).unwrap()).unwrap();
    manifest["workspaces"] = serde_json::json!(["apps/*", "packages/**"]);
    fixture
        .project
        .write("package.json", &serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    let before = snapshot(root);
    let store_before = snapshot(&fixture.home.path().join("store"));
    for args in [
        vec!["install"],
        vec!["install", "--frozen"],
        vec!["install", "--offline"],
        vec!["add", "@example/ui@^1.0.0", "--workspace", "news"],
        vec!["remove", "@example/ui", "--workspace", "news"],
        vec!["update", "--workspace", "news"],
        vec!["prune", "--workspace", "news"],
    ] {
        let output = fixture.command(&args, false);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("unsupported workspace pattern 'packages/**'"),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(snapshot(root), before, "{args:?} changed project state");
        assert_eq!(
            snapshot(&fixture.home.path().join("store")),
            store_before,
            "{args:?} changed store state"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlinked_workspace_manifest_preserves_existing_installation() {
    use std::os::unix::fs::symlink;
    for target in ["real.json", "directory", "missing.json"] {
        let fixture = Fixture::new();
        fixture.succeeds(&["install"], true);
        let root = fixture.project.path();
        fs::rename(
            root.join("packages/ui/package.json"),
            root.join("packages/ui/real.json"),
        )
        .unwrap();
        fs::create_dir(root.join("packages/ui/directory")).unwrap();
        symlink(target, root.join("packages/ui/package.json")).unwrap();
        let before = snapshot(root);
        let store_before = snapshot(&fixture.home.path().join("store"));
        for args in [
            vec!["install"],
            vec!["install", "--frozen"],
            vec!["install", "--offline"],
            vec!["add", "@example/ui@^1.0.0", "--workspace", "news"],
        ] {
            let output = fixture.command(&args, false);
            assert!(!output.status.success(), "{args:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("workspace manifest must be a regular file, not a symlink"),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(snapshot(root), before, "{args:?} changed project state");
            assert_eq!(
                snapshot(&fixture.home.path().join("store")),
                store_before,
                "{args:?} changed store state"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn root_run_rejects_symlinked_manifest_before_script_or_policy_loading() {
    use std::os::unix::fs::symlink;
    for target in ["real.json", "directory", "missing.json"] {
        let project = TempProject::new("root-run-manifest-symlink").unwrap();
        let home = TempHome::new("root-run-manifest-symlink").unwrap();
        project
            .write(
                "real.json",
                br#"{"name":"root","version":"1.0.0","scripts":{"probe":"exit 0"}}"#,
            )
            .unwrap();
        project
            .write("tapid.toml", b"invalid policy must not be parsed")
            .unwrap();
        fs::create_dir(project.path().join("directory")).unwrap();
        symlink(target, project.path().join("package.json")).unwrap();
        let before = snapshot(project.path());
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(["run", "probe"])
            .current_dir(project.path())
            .env_clear()
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("manifest must be a regular file, not a symlink"),
            "{target}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert_eq!(snapshot(project.path()), before);
    }
}
