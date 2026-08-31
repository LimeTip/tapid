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

    let roots = lockfile.packages().keys().cloned().collect::<Vec<_>>();
    lockfile.set_roots(roots).unwrap();
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
fn replay_validation_accepts_uppercase_root_manifest_digest() {
    let lowercase = "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let uppercase = format!("sha256-{}", lowercase[7..].to_uppercase());
    let lock_json = format!(
        r#"{{"lockfileVersion":4,"rootManifestDigest":"{uppercase}","resolverVersion":"0","linkerVersion":"0","packages":{{}}}}"#
    );
    let lock = Lockfile::from_json(&lock_json).unwrap();
    assert!(lock.validate_replay(lowercase).is_ok());
}

#[test]
fn exact_roots_use_a_new_schema_version_while_v4_remains_readable() {
    let mut lockfile =
        Lockfile::new("sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
    let package = package_fixture();
    lockfile.insert_package(package.clone()).unwrap();
    lockfile.set_roots([package.key()]).unwrap();
    let current = lockfile.to_json().unwrap();
    let mut current_value: serde_json::Value = serde_json::from_str(&current).unwrap();
    assert_eq!(current_value["lockfileVersion"], 5);

    let mut rootless_current = current_value.clone();
    rootless_current.as_object_mut().unwrap().remove("roots");
    assert!(Lockfile::from_json(&serde_json::to_string(&rootless_current).unwrap()).is_err());

    current_value["lockfileVersion"] = 4.into();
    current_value.as_object_mut().unwrap().remove("roots");
    assert!(Lockfile::from_json(&serde_json::to_string(&current_value).unwrap()).is_ok());
}

#[test]
fn deserialized_roots_must_be_unique_sorted_and_semantically_canonical() {
    let first = package_fixture();
    let second = LockedPackage::new(
        "https://registry.npmjs.org",
        "zeta",
        "2.0.0",
        &format!("sha512-{}", "D".repeat(86)),
        "sha256-dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
    )
    .unwrap();
    let first_key = first.key();
    let second_key = second.key();
    let mut lockfile =
        Lockfile::new("sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
    lockfile.insert_packages([first, second]).unwrap();
    lockfile
        .set_roots([first_key.clone(), second_key.clone()])
        .unwrap();
    let canonical = lockfile.to_json().unwrap();
    assert_eq!(
        Lockfile::from_json(&canonical).unwrap().to_json().unwrap(),
        canonical
    );

    let mut duplicate: serde_json::Value = serde_json::from_str(&canonical).unwrap();
    duplicate["roots"] = serde_json::json!([first_key, first_key]);
    assert!(Lockfile::from_json(&serde_json::to_string(&duplicate).unwrap()).is_err());

    let mut unordered: serde_json::Value = serde_json::from_str(&canonical).unwrap();
    unordered["roots"] = serde_json::json!([second_key, first_key]);
    assert!(Lockfile::from_json(&serde_json::to_string(&unordered).unwrap()).is_err());

    assert!(
        "https://registry.npmjs.org|demo@1.0.0|peer=not-a-context|platform=-"
            .parse::<super::LockfilePackageKey>()
            .is_err()
    );
}

#[test]
fn exact_root_package_keys_roundtrip_and_cannot_dangle() {
    let package = package_fixture();
    let root = package.key();
    let mut lockfile =
        Lockfile::new("sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
    lockfile.insert_package(package).unwrap();
    lockfile.set_roots([root.clone()]).unwrap();

    let replayed = Lockfile::from_json(&lockfile.to_json().unwrap()).unwrap();
    assert_eq!(replayed.roots(), &[root]);
    assert!(
        lockfile
            .set_roots(["https://registry.npmjs.org|missing@1.0.0|peer=-|platform=-"])
            .is_err()
    );
}

#[test]
fn accepts_scoped_https_artifact_paths_without_allowing_credentials() {
    let mut package = package_fixture();
    package
        .set_artifact_url("https://registry.npmjs.org/@alloc/quick-lru/-/quick-lru-5.2.0.tgz")
        .unwrap();

    for unsafe_url in [
        "https://user:secret@registry.npmjs.org/package.tgz",
        "https://registry.npmjs.org/package.tgz?token=secret",
        "https://registry.npmjs.org/package.tgz#fragment",
    ] {
        assert!(
            package.set_artifact_url(unsafe_url).is_err(),
            "accepted {unsafe_url}"
        );
    }
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
fn inserts_mutually_dependent_packages_as_one_batch() {
    let digest = "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let integrity = format!("sha512-{}", "A".repeat(86));
    let mut first = LockedPackage::new(
        "https://registry.example.com",
        "first",
        "1.0.0",
        &integrity,
        "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )
    .unwrap();
    let mut second = LockedPackage::new(
        "https://registry.example.com",
        "second",
        "1.0.0",
        &integrity,
        "sha256-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    )
    .unwrap();
    let first_key = first.key();
    let second_key = second.key();
    first.add_dependency("second", &second_key).unwrap();
    second.add_dependency("first", &first_key).unwrap();

    let mut lockfile = Lockfile::new(digest).unwrap();
    lockfile.insert_packages([first, second]).unwrap();
    lockfile.set_roots([first_key]).unwrap();

    assert_eq!(lockfile.packages().len(), 2);
    assert_eq!(
        Lockfile::from_json(&lockfile.to_json().unwrap()).unwrap(),
        lockfile
    );
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
    let platform = tapid_core::PlatformContext::new(
        Some("linux-gnu"),
        Some("x86_64-unknown"),
        Some("musl-extra"),
    )
    .unwrap();
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
    assert!(key.contains("linux%2Dgnu"));
    assert!(key.contains("x86_64%2Dunknown"));
    assert!(key.contains("musl%2Dextra"));
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
