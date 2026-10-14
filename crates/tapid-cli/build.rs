use std::{env, fs, path::PathBuf};

/// Copies the workspace or packaged license into the build output for embedding.
fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    // Cargo copies the inherited license-file into the standalone package root.
    let packaged_license = manifest_dir.join("LICENSE");
    let license = if packaged_license.is_file() {
        packaged_license
    } else {
        manifest_dir.join("../../LICENSE")
    };
    println!("cargo:rerun-if-changed={}", license.display());
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("LICENSE");
    fs::copy(license, output).expect("cannot embed the Tapid CLI license");
}
