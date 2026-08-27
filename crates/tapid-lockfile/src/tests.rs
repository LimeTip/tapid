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
fn replay_validation_requires_current_root_manifest_digest() {
    let digest = "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let lock = Lockfile::new(digest).unwrap();
    assert!(lock.validate_replay(digest).is_ok());
    let error = lock
        .validate_replay("sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        .unwrap_err();
    assert!(error.to_string().contains("root manifest digest mismatch"));
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

#[test]
fn same_name_and_version_from_two_registries_are_distinct() {
    let mut lockfile =
        Lockfile::new("sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
    for registry in ["https://one.example", "https://two.example"] {
        lockfile
            .insert_package(
                LockedPackage::new(
                    registry,
                    "tapid",
                    "1.0.0",
                    &format!("sha512-{}", "A".repeat(86)),
                    "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                )
                .unwrap(),
            )
            .unwrap();
    }
    assert_eq!(lockfile.packages().len(), 2);
}

#[test]
fn peer_and_platform_contexts_change_key_deterministically() {
    let peer = tapid_core::PeerContext::default()
        .with("react".parse().unwrap(), "18.2.0".parse().unwrap());
    let platform =
        tapid_core::PlatformContext::new(Some("linux"), Some("x86_64"), Some("gnu")).unwrap();
    let package = LockedPackage::new_with_context(
        "https://registry.example.test",
        "tapid",
        "1.0.0",
        &format!("sha512-{}", "A".repeat(86)),
        "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        &peer,
        &platform,
    )
    .unwrap();
    assert_eq!(
        package.key(),
        "https://registry.example.test|tapid@1.0.0|peer=name=react;version=18.2.0|platform=os=linux;cpu=x86_64;libc=gnu"
    );
}

#[test]
fn package_key_roundtrips_contexts_without_loss() {
    let peer = tapid_core::PeerContext::default()
        .with("foo-bar".parse().unwrap(), "1.2.3".parse().unwrap());
    let platform =
        tapid_core::PlatformContext::new(Some("linux"), Some("x86_64"), Some("musl")).unwrap();
    let package = LockedPackage::new_with_context(
        "https://registry.example.test",
        "tapid",
        "1.0.0",
        &format!("sha512-{}", "A".repeat(86)),
        "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        &peer,
        &platform,
    )
    .unwrap();
    let key = package.key();
    let parsed = key.parse::<super::LockfilePackageKey>().unwrap();
    assert_eq!(parsed.to_string(), key);
    assert!(key.contains("foo%2Dbar"));
}

#[test]
fn package_key_distinguishes_platform_component_boundaries() {
    let first = tapid_core::PlatformContext::new(Some("linux"), Some("x86_64"), None).unwrap();
    let second = tapid_core::PlatformContext::new(Some("linux"), None, Some("x86_64")).unwrap();
    let make = |platform: &tapid_core::PlatformContext| {
        LockedPackage::new_with_context(
            "https://registry.example.test",
            "tapid",
            "1.0.0",
            &format!("sha512-{}", "A".repeat(86)),
            "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &tapid_core::PeerContext::default(),
            platform,
        )
        .unwrap()
    };
    assert_ne!(make(&first).key(), make(&second).key());
}

#[test]
fn lockfile_v1_is_not_implicitly_accepted() {
    let input = r#"{"lockfileVersion":1,"rootManifestDigest":"sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","resolverVersion":"0","linkerVersion":"0","packages":{}}"#;
    assert!(Lockfile::from_json(input).is_err());
}
