use super::{LockedPackage, Lockfile, VERSION};
use std::str::FromStr;
use tapid_core::{ArtifactDigest, PackageName, PackageVersion};

#[test]
fn version_is_present() {
    assert!(!VERSION.is_empty());
}

#[test]
fn serializes_packages_in_canonical_order() {
    let mut lockfile =
        Lockfile::new("sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
    lockfile
        .insert_package(
            LockedPackage::new(
                "https://registry.example.test",
                "@tapid/core",
                "1.0.0",
                &format!("sha512-{}", "A".repeat(86)),
                "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .unwrap(),
        )
        .unwrap();
    lockfile
        .insert_package(
            LockedPackage::new(
                "https://registry.example.test",
                "alpha",
                "1.0.0",
                &format!("sha512-{}", "C".repeat(86)),
                "sha256-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            )
            .unwrap(),
        )
        .unwrap();

    let json = lockfile.to_json().unwrap();
    assert!(json.find("@tapid/core@1.0.0").unwrap() < json.find("alpha@1.0.0").unwrap());
    assert_eq!(Lockfile::from_json(&json).unwrap(), lockfile);
}

#[test]
fn rejects_machine_specific_or_secret_values() {
    let mut lockfile =
        Lockfile::new("sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
    let mut package = LockedPackage::new(
        "https://registry.example.test",
        "tapid",
        "1.0.0",
        &format!("sha512-{}", "A".repeat(86)),
        "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )
    .unwrap();
    let local_path = std::env::temp_dir().join(format!("tapid-package-{}.tgz", std::process::id()));
    let local_url = format!("file://{}", local_path.display());
    package.set_artifact_url(&local_url).unwrap_err();
    assert!(lockfile.insert_package(package).is_ok());
}

#[test]
fn constructors_validate_domain_values() {
    assert_eq!(PackageName::from_str("tapid").unwrap().to_string(), "tapid");
    assert_eq!(
        "1.2.3".parse::<PackageVersion>().unwrap().to_string(),
        "1.2.3"
    );
    assert_eq!(
        "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .parse::<ArtifactDigest>()
            .unwrap()
            .to_string(),
        "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
}

#[test]
fn preserves_case_sensitive_sha512_integrity_values() {
    let integrity = format!("sha512-{}", "AbCdEfGh".repeat(10) + "AbCdEf");
    let mut lockfile =
        Lockfile::new("sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
    lockfile
        .insert_package(
            LockedPackage::new(
                "https://registry.example.test",
                "tapid",
                "1.0.0",
                &integrity,
                "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .unwrap(),
        )
        .unwrap();
    assert!(lockfile.to_json().unwrap().contains(&integrity));
}

#[test]
fn rejects_invalid_root_manifest_digest() {
    assert!(Lockfile::new("not-a-digest").is_err());
}

#[test]
fn rejects_invalid_artifact_urls_at_construction_time() {
    let mut package = package_fixture();
    assert!(package.set_artifact_url("file:///tmp/package.tgz").is_err());
}

fn package_fixture() -> LockedPackage {
    LockedPackage::new(
        "https://registry.example.test",
        "tapid",
        "1.0.0",
        &format!("sha512-{}", "A".repeat(86)),
        "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )
    .unwrap()
}
