use std::io::Read;
use tapid_publish::{NpmPackOptions, artifact_digest, pack_npm};

#[test]
fn npm_archive_binds_captured_inventory_and_normalizes_headers() {
    let project = tapid_test_support::TempProject::new("npm-pack").unwrap();
    let manifest = br#"{"name":"example","version":"1.2.3","scripts":{"prepack":"touch marker","prepare":"touch marker","postpack":"touch marker"}}"#;
    std::fs::write(project.path().join("package.json"), manifest).unwrap();
    std::fs::write(project.path().join("index.js"), b"module.exports = 42;\n").unwrap();
    let artifact = pack_npm(project.path(), &NpmPackOptions::default()).unwrap();
    assert_eq!(artifact.name(), "example");
    assert_eq!(artifact.version(), "1.2.3");
    assert_eq!(artifact.digest(), &artifact_digest(artifact.bytes()));
    assert_eq!(artifact.files().len(), 2);
    assert_eq!(&artifact.bytes()[4..8], &[0; 4]);
    assert_eq!(artifact.bytes()[9], 255);
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(artifact.bytes()));
    for (entry, expected) in archive.entries().unwrap().zip(artifact.files()) {
        let mut entry = entry.unwrap();
        assert_eq!(
            entry.path().unwrap().to_str().unwrap(),
            format!("package/{}", expected.path)
        );
        assert_eq!(entry.header().uid().unwrap(), 0);
        assert_eq!(entry.header().gid().unwrap(), 0);
        assert_eq!(entry.header().mtime().unwrap(), 0);
        assert_eq!(entry.header().mode().unwrap(), expected.mode);
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content.len() as u64, expected.size);
        assert_eq!(artifact_digest(&content), expected.digest);
        if expected.path == "package.json" {
            assert_eq!(content, manifest);
        }
    }
    assert_eq!(
        artifact.bytes(),
        pack_npm(project.path(), &NpmPackOptions::default())
            .unwrap()
            .bytes()
    );
    assert!(!project.path().join("marker").exists());
}

#[test]
fn npm_selection_honors_nested_negation_and_mandatory_targets() {
    let project = tapid_test_support::TempProject::new("npm-selection").unwrap();
    for directory in ["dist", "src", "node_modules"] {
        std::fs::create_dir(project.path().join(directory)).unwrap();
    }
    for (path, bytes) in [
        (
            "package.json",
            r#"{"name":"example","version":"1.0.0","files":["dist"],"main":"src/main.js","bin":"cli.js"}"#,
        ),
        (".npmignore", "dist\n"),
        ("dist/.npmignore", "*.js\n!keep.js\n"),
        ("dist/keep.js", "kept"),
        ("dist/drop.js", "dropped"),
        ("src/main.js", "main"),
        ("cli.js", "bin"),
        ("README.md", "readme"),
        (".env", "secret"),
        ("node_modules/dependency.js", "dependency"),
        ("other.js", "other"),
    ] {
        std::fs::write(project.path().join(path), bytes).unwrap();
    }
    let artifact = pack_npm(project.path(), &NpmPackOptions::default()).unwrap();
    assert_eq!(
        artifact
            .files()
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        [
            "README.md",
            "cli.js",
            "dist/keep.js",
            "package.json",
            "src/main.js"
        ]
    );
    assert_eq!(
        artifact
            .files()
            .iter()
            .find(|file| file.path == "cli.js")
            .unwrap()
            .mode,
        0o755
    );
}

#[test]
fn npm_ignore_negation_cannot_expand_files_allowlist() {
    let project = tapid_test_support::TempProject::new("npm-ignore-subset").unwrap();
    std::fs::create_dir(project.path().join("dist")).unwrap();
    for (path, bytes) in [
        (
            "package.json",
            r#"{"name":"example","version":"1.0.0","files":["dist/*.js"]}"#,
        ),
        ("dist/.npmignore", "!other.txt\n"),
        ("dist/other.txt", "excluded"),
        ("dist/index.js", "included"),
    ] {
        std::fs::write(project.path().join(path), bytes).unwrap();
    }
    let artifact = pack_npm(project.path(), &NpmPackOptions::default()).unwrap();
    assert_eq!(
        artifact
            .files()
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["dist/index.js", "package.json"]
    );
}

#[cfg(unix)]
#[test]
fn npm_rejects_directory_links_even_outside_allowlist() {
    let project = tapid_test_support::TempProject::new("npm-links").unwrap();
    std::fs::write(
        project.path().join("package.json"),
        r#"{"name":"example","version":"1.0.0","files":[]}"#,
    )
    .unwrap();
    std::fs::create_dir(project.path().join("directory")).unwrap();
    std::os::unix::fs::symlink("directory", project.path().join("link")).unwrap();
    assert!(pack_npm(project.path(), &NpmPackOptions::default()).is_err());
}

#[test]
fn npm_rejects_unsafe_inputs_and_each_resource_budget_without_mutation() {
    let project = tapid_test_support::TempProject::new("npm-bounds").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    project.write("data.txt", b"12345678").unwrap();
    let original = std::fs::read(project.path().join("package.json")).unwrap();
    let mut limits = Vec::new();
    for field in 0..9 {
        let mut options = NpmPackOptions::default();
        match field {
            0 => options.limits.max_files = 1,
            1 => options.limits.max_visited_entries = 1,
            2 => options.limits.max_path_bytes = 4,
            3 => options.limits.max_file_bytes = 4,
            4 => options.limits.max_content_bytes = original.len() as u64,
            5 => options.limits.max_tar_bytes = 128,
            6 => options.limits.max_archive_bytes = 8,
            7 => {
                project.write(".npmignore", b"*.nothing\n").unwrap();
                options.limits.max_ignore_file_bytes = 1;
            }
            _ => {
                options.limits.max_match_steps = 0;
            }
        }
        limits.push(options);
    }
    for (index, options) in limits.iter().enumerate() {
        assert!(pack_npm(project.path(), options).is_err(), "budget {index}");
        assert_eq!(
            std::fs::read(project.path().join("package.json")).unwrap(),
            original
        );
    }
    for pattern in ["[abc]", "a/**b", "../escape", "!data.txt", "{a,b}", "@(a)"] {
        project
            .write(
                "package.json",
                format!(r#"{{"name":"example","version":"1.0.0","files":[{pattern:?}]}}"#)
                    .as_bytes(),
            )
            .unwrap();
        assert!(
            pack_npm(project.path(), &NpmPackOptions::default()).is_err(),
            "{pattern}"
        );
    }
}

#[test]
fn npm_root_ignore_and_safety_exclusions_apply_before_mandatory_files() {
    let project = tapid_test_support::TempProject::new("npm-root-policy").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"example","version":"1.0.0","main":".env"}"#,
        )
        .unwrap();
    project.write(".env", b"not-a-real-secret").unwrap();
    assert!(pack_npm(project.path(), &NpmPackOptions::default()).is_err());
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    project
        .write(".npmignore", b"ignored/\n!ignored/keep.txt\nREADME.md\n")
        .unwrap();
    project.write("ignored/keep.txt", b"keep").unwrap();
    project.write("ignored/drop.txt", b"drop").unwrap();
    project.write("README.md", b"mandatory").unwrap();
    for path in [
        "nested/.NPMRC",
        "nested/.env.dev",
        "nested/out.TGZ",
        "nested/.tapid-pack-orphan",
        "nested/backup~",
        "nested/#backup#",
        "nested/pnpm-lock.yaml",
    ] {
        project.write(path, b"excluded").unwrap();
    }
    let artifact = pack_npm(project.path(), &NpmPackOptions::default()).unwrap();
    assert_eq!(
        artifact
            .files()
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["README.md", "ignored/keep.txt", "package.json"]
    );
}

#[test]
fn npm_reports_gitignore_without_using_it_as_selection_policy() {
    let project = tapid_test_support::TempProject::new("npm-gitignore").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    project.write(".gitignore", b"index.js\n").unwrap();
    project.write("index.js", b"included").unwrap();
    let artifact = pack_npm(project.path(), &NpmPackOptions::default()).unwrap();
    assert!(artifact.gitignore_not_used());
    assert!(artifact.files().iter().any(|file| file.path == "index.js"));
}

#[test]
fn npm_long_path_records_have_normalized_metadata() {
    let project = tapid_test_support::TempProject::new("npm-long-path").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    let path = format!("{}/{}/index.js", "a".repeat(70), "b".repeat(70));
    project.write(&path, b"long-name").unwrap();
    let artifact = pack_npm(project.path(), &NpmPackOptions::default()).unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(artifact.bytes()));
    let mut long_names = 0;
    for entry in archive.entries().unwrap().raw(true) {
        let entry = entry.unwrap();
        assert_eq!(entry.header().uid().unwrap(), 0);
        assert_eq!(entry.header().gid().unwrap(), 0);
        assert_eq!(entry.header().mtime().unwrap(), 0);
        assert_eq!(entry.header().username().unwrap(), Some(""));
        assert_eq!(entry.header().groupname().unwrap(), Some(""));
        if entry.header().entry_type().is_gnu_longname() {
            long_names += 1;
        }
    }
    assert_eq!(long_names, 1);
}

#[test]
fn npm_required_paths_and_remaining_budgets_fail_closed() {
    let project = tapid_test_support::TempProject::new("npm-more-bounds").unwrap();
    for target in [
        "/absolute",
        "C:/drive",
        "../parent",
        "CON.txt",
        "dir./file",
        "a//b",
        "file ",
        "a:b",
        "a\\b",
        "missing.js",
    ] {
        let manifest = serde_json::json!({"name":"example", "version":"1.0.0", "main":target});
        project
            .write("package.json", manifest.to_string().as_bytes())
            .unwrap();
        assert!(
            pack_npm(project.path(), &NpmPackOptions::default()).is_err(),
            "{target:?}"
        );
    }
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    project.write("nested/.npmignore", b"*.txt\n").unwrap();
    project.write("nested/file.txt", b"ignored").unwrap();
    for budget in 0..3 {
        let mut options = NpmPackOptions::default();
        match budget {
            0 => options.limits.max_depth = 0,
            1 => options.limits.max_patterns = 0,
            _ => options.limits.max_ignore_bytes = 1,
        }
        assert!(
            pack_npm(project.path(), &options).is_err(),
            "budget {budget}"
        );
    }
}

#[cfg(unix)]
#[test]
fn npm_rejects_selected_symlinks() {
    use std::os::unix::fs::symlink;
    let project = tapid_test_support::TempProject::new("npm-unsafe-kinds").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    symlink("package.json", project.path().join("link")).unwrap();
    assert!(pack_npm(project.path(), &NpmPackOptions::default()).is_err());
}

#[test]
fn npm_component_case_collisions_are_rejected_when_distinct_on_source_filesystem() {
    let project = tapid_test_support::TempProject::new("npm-case-components").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    project.write("ss/one", b"one").unwrap();
    project.write("ß/two", b"two").unwrap();
    if std::fs::read_dir(project.path()).unwrap().count() == 3 {
        assert!(pack_npm(project.path(), &NpmPackOptions::default()).is_err());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn npm_rejects_non_utf8_source_names() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let project = tapid_test_support::TempProject::new("npm-non-utf8").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    std::fs::write(
        project.path().join(OsString::from_vec(vec![0xff])),
        b"invalid",
    )
    .unwrap();
    assert!(pack_npm(project.path(), &NpmPackOptions::default()).is_err());
}

#[test]
fn npm_files_patterns_preserve_literal_exclamation_and_parentheses() {
    let project = tapid_test_support::TempProject::new("npm-literal-pattern").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"example","version":"1.0.0","files":["a!file(1).js"]}"#,
        )
        .unwrap();
    project.write("a!file(1).js", b"literal").unwrap();
    let artifact = pack_npm(project.path(), &NpmPackOptions::default()).unwrap();
    assert!(
        artifact
            .files()
            .iter()
            .any(|file| file.path == "a!file(1).js")
    );
}

#[cfg(unix)]
#[test]
fn npm_rejects_existing_windows_device_and_ambiguous_names() {
    let project = tapid_test_support::TempProject::new("npm-windows-path-policy").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    for name in [
        "CON.txt",
        "COM¹.txt",
        "CONIN$",
        "a:b",
        "a\\b",
        "trailing.",
        "trailing ",
    ] {
        let path = project.path().join(name);
        std::fs::write(&path, b"unsafe").unwrap();
        assert!(
            pack_npm(project.path(), &NpmPackOptions::default()).is_err(),
            "accepted {name:?}"
        );
        std::fs::remove_file(&path).unwrap();
    }
}
