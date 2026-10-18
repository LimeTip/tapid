use std::{path::Path, process::Command};

pub fn compile_launcher(destination: &Path) {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/windows_node_launcher.rs");
    let mut compiler = Command::new("rustc");
    compiler
        .args([
            "--crate-type",
            "cdylib",
            "--edition",
            "2024",
            "-C",
            "opt-level=2",
            "-C",
            "panic=abort",
        ])
        .arg(source)
        .arg("-o")
        .arg(destination);
    if let Ok(flags) = std::env::var("RUSTFLAGS") {
        compiler.args(flags.split_whitespace());
    }
    let compiled = compiler.output().unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
}
