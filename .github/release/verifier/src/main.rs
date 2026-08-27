use std::{env, fs, process};

use serde_json::Value;
use tapid_signatures::{KeyRing, release};

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("error: {}", message.as_ref());
    process::exit(2);
}

fn arg<'a>(args: &'a [String], index: usize, name: &str) -> &'a str {
    args.get(index)
        .map(String::as_str)
        .unwrap_or_else(|| fail(format!("missing {name}")))
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 6 {
        fail("usage: verifier MANIFEST KEYRING VERSION TAG COMMIT");
    }
    let manifest_path = arg(&args, 1, "manifest path");
    let keyring_path = arg(&args, 2, "keyring path");
    let expected_version = arg(&args, 3, "version");
    let expected_tag = arg(&args, 4, "tag");
    let expected_commit = arg(&args, 5, "commit");

    let manifest: Value = serde_json::from_slice(
        &fs::read(manifest_path)
            .unwrap_or_else(|error| fail(format!("cannot read manifest: {error}"))),
    )
    .unwrap_or_else(|error| fail(format!("manifest is not valid JSON: {error}")));
    let object = manifest
        .as_object()
        .unwrap_or_else(|| fail("manifest must be a JSON object"));
    if object.get("schema").and_then(Value::as_str) != Some("tapid-release-manifest-v1") {
        fail("manifest schema is unsupported");
    }
    if object.get("product").and_then(Value::as_str) != Some("tapid") {
        fail("manifest product is not tapid");
    }
    for (field, expected) in [
        ("version", expected_version),
        ("tag", expected_tag),
        ("commit", expected_commit),
    ] {
        if object.get(field).and_then(Value::as_str) != Some(expected) {
            fail(format!("manifest {field} does not match workflow input"));
        }
    }
    if !object
        .get("artifacts")
        .and_then(Value::as_array)
        .is_some_and(|artifacts| !artifacts.is_empty())
    {
        fail("manifest has no artifacts");
    }

    let keyring_bytes = fs::read(keyring_path)
        .unwrap_or_else(|error| fail(format!("cannot read keyring: {error}")));
    let keyring = KeyRing::from_embedded_json(&keyring_bytes)
        .unwrap_or_else(|error| fail(format!("invalid trusted keyring: {error}")));
    release::verify(&manifest, &keyring)
        .unwrap_or_else(|error| fail(format!("release manifest verification failed: {error}")));
    println!("verified release manifest against RFC 8785 production signing protocol");
}
