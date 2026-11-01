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
        self.command_at(args, online, self.project.path())
    }

    fn command_at(&self, args: &[&str], online: bool, cwd: &Path) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path());
        for key in ["PATH", "SystemRoot", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        if args.first() != Some(&"run") {
            command
                .arg("--store-dir")
                .arg(self.home.path().join("store"));
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .args(["run", "probe"])
            .current_dir(project.path())
            .env_clear()
            .env("HOME", home.path());
        // Coverage profiles belong outside the project whose state is asserted below.
        if let Some(profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile_file);
        }
        let output = command.output().unwrap();
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

#[test]
fn nested_member_install_uses_workspace_owner() {
    for relative in ["apps/news", "apps/news/src/nested"] {
        let fixture = Fixture::new();
        let root = fixture.project.path();
        fs::create_dir_all(root.join(relative)).unwrap();
        let original_root = fs::read(root.join("package.json")).unwrap();
        let original_ui = fs::read(root.join("packages/ui/package.json")).unwrap();
        let output = fixture.command_at(&["install", "external@1.0.0"], true, &root.join(relative));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(root.join("package.json")).unwrap(), original_root);
        assert_eq!(
            fs::read(root.join("packages/ui/package.json")).unwrap(),
            original_ui
        );
        let news: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("apps/news/package.json")).unwrap())
                .unwrap();
        assert_eq!(news["dependencies"]["external"], "1.0.0");
        fixture.assert_imports("nested-install");
    }
}

#[test]
fn inferred_run_rejects_result_file_in_sibling_before_launch() {
    let fixture = Fixture::new();
    let root = fixture.project.path();
    for destination in [
        "result.json",
        "apps/news/result.json",
        "packages/ui/result.json",
    ] {
        let destination = root.join(destination);
        let output = fixture.command_at(
            &[
                "run",
                "probe",
                "--result-file",
                destination.to_str().unwrap(),
            ],
            false,
            &root.join("apps/news"),
        );
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("result file must be outside the project root"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!destination.exists());
    }
}

#[test]
fn inferred_lifecycle_and_replay_keep_root_ownership_and_selected_manifest() {
    for relative in [
        ".",
        "apps/news",
        "apps/news/src/deep",
        "packages/ui",
        "packages/ui/src/deep",
    ] {
        let fixture = Fixture::new();
        let root = fixture.project.path();
        fs::create_dir_all(root.join(relative)).unwrap();
        let selected = if relative.starts_with("apps/") {
            "apps/news/package.json"
        } else if relative.starts_with("packages/") {
            "packages/ui/package.json"
        } else {
            "package.json"
        };
        let untouched = [
            "package.json",
            "apps/news/package.json",
            "packages/ui/package.json",
        ]
        .into_iter()
        .filter(|path| *path != selected)
        .map(|path| (path, fs::read(root.join(path)).unwrap()))
        .collect::<Vec<_>>();
        for args in [
            vec!["install"],
            vec!["add", "external@1.0.0", "--dry-run"],
            vec!["add", "external@1.0.0"],
            vec!["update", "external", "--dry-run"],
            vec!["update", "external"],
            vec!["remove", "external", "--dry-run"],
            vec!["remove", "external"],
            vec!["install", "external@1.0.0"],
            vec!["prune"],
            vec!["install", "--offline"],
            vec!["install", "--frozen"],
        ] {
            let preview = args.contains(&"--dry-run");
            let before = preview.then(|| snapshot(root));
            let online =
                !args.contains(&"--offline") && !args.contains(&"--frozen") && args[0] != "prune";
            let output = fixture.command_at(&args, online, &root.join(relative));
            assert!(
                output.status.success(),
                "{relative} {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            for (path, bytes) in &untouched {
                assert_eq!(
                    &fs::read(root.join(path)).unwrap(),
                    bytes,
                    "{relative} {args:?}: {path}"
                );
            }
            if let Some(before) = before {
                assert_eq!(snapshot(root), before);
            }
            assert!(root.join("tapid.lock").exists());
            assert!(!root.join("apps/news/tapid.lock").exists());
            assert!(!root.join("packages/ui/tapid.lock").exists());
            assert!(!root.join("apps/news/node_modules").exists());
            assert!(!root.join("packages/ui/node_modules").exists());
        }
        fixture.assert_imports(relative);
    }
}

#[test]
fn explicit_selection_precedes_inference_without_owner_fallback() {
    let fixture = Fixture::new();
    fixture.succeeds(&["install"], true);
    let root = fixture.project.path();
    let original_news = fs::read(root.join("apps/news/package.json")).unwrap();
    let original_root = fs::read(root.join("package.json")).unwrap();
    let output = fixture.command_at(
        &["add", "external@1.0.0", "--workspace", "@example/ui"],
        true,
        &root.join("apps/news"),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(root.join("apps/news/package.json")).unwrap(),
        original_news
    );
    assert_eq!(fs::read(root.join("package.json")).unwrap(), original_root);
    let other = TempProject::new("explicit-project-cwd").unwrap();
    let output = fixture.command_at(
        &[
            "add",
            "external@1.0.0",
            "--project-dir",
            root.to_str().unwrap(),
        ],
        true,
        other.path(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(root.join("apps/news/package.json")).unwrap(),
        original_news
    );
    for args in [
        vec!["install", "--workspace", "missing"],
        vec!["install", "--project-dir", "missing"],
    ] {
        let before = snapshot(root);
        let store = snapshot(&fixture.home.path().join("store"));
        let output = fixture.command_at(&args, true, &root.join("apps/news"));
        assert!(!output.status.success());
        assert_eq!(snapshot(root), before);
        assert_eq!(snapshot(&fixture.home.path().join("store")), store);
    }
}

#[test]
fn explicit_dot_preserves_standalone_target_even_inside_workspace() {
    let fixture = Fixture::new();
    // UI has no dependencies, so exact standalone selection is observable without
    // involving the outer workspace's local dependency graph.
    let root = fixture.project.path();
    let output = fixture.command_at(
        &["install", "--project-dir", "."],
        true,
        &root.join("packages/ui"),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join("packages/ui/tapid.lock").exists());
    assert!(!root.join("tapid.lock").exists());
}

#[test]
fn discovery_rejections_preserve_existing_project_and_store() {
    let fixture = Fixture::new();
    fixture.succeeds(&["install"], true);
    let root = fixture.project.path();
    fixture
        .project
        .write(
            "apps/news/undeclared/package.json",
            br#"{"name":"rogue","version":"1.0.0"}"#,
        )
        .unwrap();
    let cwd = root.join("apps/news/undeclared");
    let before = snapshot(root);
    let store = snapshot(&fixture.home.path().join("store"));
    for args in [
        vec!["install"],
        vec!["install", "--offline"],
        vec!["install", "--frozen"],
        vec!["add", "external"],
        vec!["remove", "external"],
        vec!["update"],
        vec!["prune"],
        vec!["run", "probe"],
    ] {
        let output = fixture.command_at(&args, false, &cwd);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("undeclared package"),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(snapshot(root), before);
        assert_eq!(snapshot(&fixture.home.path().join("store")), store);
    }
}

#[test]
fn inferred_run_rejects_member_grants_above_root_policy_before_launch() {
    let fixture = Fixture::new();
    let root = fixture.project.path();
    fixture
        .project
        .write(
            "tapid.toml",
            b"[run.scripts.probe]\nassurance='restricted'\nread=['.']\nsubprocess=true\nnetwork=false\n",
        )
        .unwrap();
    fixture
        .project
        .write(
            "apps/news/tapid.toml",
            b"[run.scripts.probe]\nassurance='restricted'\nread=['.']\nwrite=['.']\nsubprocess=true\nnetwork=true\n",
        )
        .unwrap();
    fs::create_dir_all(root.join("apps/news/src/deep")).unwrap();
    let before = snapshot(root);
    for relative in ["apps/news", "apps/news/src/deep"] {
        for grant in [vec!["--allow-write", "."], vec!["--allow-network"]] {
            let mut args = vec!["run", "probe"];
            args.extend(grant);
            let output = fixture.command_at(&args, false, &root.join(relative));
            assert!(!output.status.success(), "{relative}: {args:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("exceeds the configured run policy"),
                "{relative}: {args:?}: {stderr}"
            );
            assert_eq!(snapshot(root), before);
        }
    }
}

#[cfg(unix)]
#[test]
fn inferred_run_uses_member_cwd_and_root_relative_policy() {
    let fixture = Fixture::new();
    let root = fixture.project.path();
    fixture.project.write("tapid.toml", b"[run.scripts.probe]\nassurance='restricted'\nread=['.']\nwrite=['output']\nsubprocess=true\nnetwork=false\n").unwrap();
    fs::create_dir(root.join("output")).unwrap();
    fs::create_dir_all(root.join("apps/news/src/deep")).unwrap();
    fixture
        .project
        .write(
            "apps/news/tapid.toml",
            b"malformed member policy must not be read",
        )
        .unwrap();
    fixture.project.write("apps/news/probe.js", br#"const fs = require('node:fs'); const path = require('node:path'); if (path.basename(process.cwd()) !== 'news') process.exit(41); fs.writeFileSync('../../output/ok', 'member cwd under root grant'); console.log('MEMBER_POLICY_OK');"#).unwrap();
    fixture
        .project
        .write(
            "packages/ui/package.json",
            br#"{"name":"@example/ui","version":"1.2.0","scripts":{"probe":"node probe.js"}}"#,
        )
        .unwrap();
    fixture.project.write("packages/ui/probe.js", br#"const fs=require('node:fs'); if(require('node:path').basename(process.cwd())!=='ui') process.exit(42); fs.writeFileSync('../../output/ok','member cwd under root grant'); console.log('UI_POLICY_OK');"#).unwrap();
    fixture.project.write("probe.js", br#"require('node:fs').writeFileSync('output/ok','member cwd under root grant'); console.log('ROOT_POLICY_OK');"#).unwrap();
    fs::create_dir_all(root.join("packages/ui/src/deep")).unwrap();
    for (relative, args, marker) in [
        (".", vec!["run", "probe"], "ROOT_POLICY_OK"),
        ("apps/news", vec!["run", "probe"], "MEMBER_POLICY_OK"),
        (
            "apps/news/src/deep",
            vec!["run", "probe"],
            "MEMBER_POLICY_OK",
        ),
        ("packages/ui/src/deep", vec!["run", "probe"], "UI_POLICY_OK"),
        (
            "apps/news",
            vec!["run", "probe", "--workspace", "@example/ui"],
            "UI_POLICY_OK",
        ),
    ] {
        let output = fixture.command_at(&args, false, &root.join(relative));
        assert!(
            output.status.success(),
            "{relative}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
        assert_eq!(
            fs::read(root.join("output/ok")).unwrap(),
            b"member cwd under root grant"
        );
    }
    // A member cannot replace the root's denied write authority with its own policy.
    fixture.project.write("tapid.toml", b"[run.scripts.probe]\nassurance='restricted'\nread=['.']\nsubprocess=true\nnetwork=false\n").unwrap();
    fixture.project.write("apps/news/tapid.toml", b"[run.scripts.probe]\nassurance='restricted'\nread=['.']\nwrite=['.']\nsubprocess=true\nnetwork=true\n").unwrap();
    fs::remove_file(root.join("output/ok")).unwrap();
    let output = fixture.command_at(&["run", "probe"], false, &root.join("apps/news"));
    assert!(!output.status.success());
    assert!(
        ["EACCES", "EPERM", "EROFS"]
            .iter()
            .any(|code| String::from_utf8_lossy(&output.stderr).contains(code)),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.join("output/ok").exists());
}
