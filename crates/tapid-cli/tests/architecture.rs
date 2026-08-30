use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_ENTRYPOINT_LINES: usize = 100;
const MAX_PRODUCTION_FILE_LINES: usize = 800;

fn is_production_rust(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("rs")
        && !matches!(
            path.file_name().and_then(|value| value.to_str()),
            Some("test.rs" | "tests.rs")
        )
        && !path
            .components()
            .any(|component| matches!(component.as_os_str().to_str(), Some("test" | "tests")))
}

fn rust_files_under(root: PathBuf) -> Vec<PathBuf> {
    let mut pending = vec![root];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).expect("read source directory") {
            let path = entry.expect("read source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if is_production_rust(&path) {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn main_is_a_thin_entrypoint() {
    let main = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    let source = fs::read_to_string(main).expect("read src/main.rs");
    let significant_lines = source
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();

    assert!(
        significant_lines <= MAX_ENTRYPOINT_LINES,
        "src/main.rs has {significant_lines} significant lines, expected at most {MAX_ENTRYPOINT_LINES}"
    );
    assert!(
        source.contains("tapid::run()"),
        "entrypoint must call the application facade"
    );
}

#[test]
fn application_modules_do_not_render_or_choose_process_status() {
    let application_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/application");
    for path in rust_files_under(application_root) {
        let source = fs::read_to_string(&path).expect("read application module");
        assert!(
            !source.contains("println!") && !source.contains("eprintln!"),
            "{} renders terminal output inside the application layer",
            path.display()
        );
        assert!(
            !source.contains("ExitCode"),
            "{} chooses process exit codes inside the application layer",
            path.display()
        );
    }
}

#[test]
fn commands_are_split_by_user_facing_capability() {
    let command_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
    for capability in ["init", "install", "lock", "manifest", "run", "upgrade"] {
        assert!(
            command_root.join(format!("{capability}.rs")).is_file(),
            "missing command capability module: {capability}"
        );
    }
}

#[test]
fn rust_file_discovery_recurses_into_nested_modules() {
    let root = std::env::temp_dir().join(format!(
        "tapid-architecture-recursion-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos()
    ));
    fs::create_dir(&root).expect("claim unique test directory");
    let nested = root.join("nested");
    fs::create_dir(&nested).unwrap();
    let tests = nested.join("tests");
    fs::create_dir(&tests).unwrap();
    fs::write(root.join("top.rs"), "").unwrap();
    fs::write(root.join("test.rs"), "").unwrap();
    fs::write(nested.join("mod.rs"), "").unwrap();
    fs::write(tests.join("fixture.rs"), "").unwrap();

    let mut files = rust_files_under(root.clone());
    files.sort();

    let mut expected = vec![root.join("top.rs"), nested.join("mod.rs")];
    expected.sort();
    assert_eq!(files, expected);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn production_modules_stay_navigable() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut oversized = Vec::new();

    for path in rust_files_under(source_root) {
        let lines = fs::read_to_string(&path)
            .expect("read Rust source")
            .lines()
            .count();
        if lines > MAX_PRODUCTION_FILE_LINES {
            oversized.push(format!("{}: {lines}", path.display()));
        }
    }

    assert!(
        oversized.is_empty(),
        "production Rust files must not exceed {MAX_PRODUCTION_FILE_LINES} lines:\n{}",
        oversized.join("\n")
    );
}
