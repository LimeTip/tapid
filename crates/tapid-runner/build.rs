use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=src/windows_compat");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=TAPID_WINDOWS_COMPAT_COMPILE_ONLY");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("tapid-node-compat.dll");
    let compile_only = match env::var("TAPID_WINDOWS_COMPAT_COMPILE_ONLY") {
        Err(env::VarError::NotPresent) => false,
        Ok(value) if value == "1" => {
            assert!(
                !env::var("HOST").unwrap().contains("windows")
                    && env::var("PROFILE").as_deref() == Ok("debug"),
                "compile-only compatibility mode is limited to non-Windows debug type checks"
            );
            true
        }
        _ => panic!("invalid TAPID_WINDOWS_COMPAT_COMPILE_ONLY"),
    };
    if !matches!(
        env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
        Ok("x86_64" | "aarch64")
    ) {
        std::fs::write(output, []).expect("write unsupported-architecture component marker");
        return;
    }
    let mut compiler = Command::new(env::var_os("RUSTC").unwrap());
    compiler
        .args([
            "--crate-name",
            "tapid_node_compat",
            "--crate-type",
            "cdylib",
            "--edition",
            "2024",
            "--target",
            &env::var("TARGET").unwrap(),
            "-C",
            "opt-level=2",
            "-C",
            "panic=abort",
            "src/windows_compat/dll.rs",
            "-o",
        ])
        .arg(&output);
    if compile_only {
        compiler.arg("--emit=metadata");
    }
    if let Ok(flags) = env::var("CARGO_ENCODED_RUSTFLAGS") {
        compiler.args(flags.split('\u{1f}').filter(|flag| !flag.is_empty()));
    }
    assert!(
        compiler
            .status()
            .expect("compile Windows compatibility component")
            .success(),
        "Windows compatibility component compilation failed"
    );
    if compile_only {
        // Explicit compile-only CI can type-check the exact component source without
        // Windows SDK import libraries. This marker cannot execute Node: preparation
        // rejects it. Native builds and every release always compile the actual DLL.
        std::fs::write(output, []).expect("write compile-only component marker");
    }
}
