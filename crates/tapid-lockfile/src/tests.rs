use super::{LockedPackage, Lockfile, LockfilePackageKey, VERSION};
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
fn replay_accepts_equivalent_uppercase_root_manifest_digest() {
    let lowercase = "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let uppercase = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let mut document: serde_json::Value =
        serde_json::from_str(&Lockfile::new(lowercase).unwrap().to_json().unwrap()).unwrap();
    document["rootManifestDigest"] = serde_json::Value::String(uppercase.to_owned());
    let lock = Lockfile::from_json(&serde_json::to_string(&document).unwrap()).unwrap();
    assert_eq!(lock.root_manifest_digest(), lowercase);
    assert!(lock.validate_replay(lowercase).is_ok());
    assert!(lock.validate_replay(uppercase).is_ok());
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
fn package_key_display_round_trips_without_loss() {
    for encoded in [
        concat!(
            "https://registry.example.test",
            "|@scope/pkg@1.0.0|peer=react-dom@18.2.0|platform=linux-x86_64-gnu"
        ),
        concat!(
            "https://registry.example.test",
            "|peer-pkg@1.0.0|peer=-|platform=-"
        ),
    ] {
        let parsed = encoded.parse::<crate::LockfilePackageKey>().unwrap();
        assert_eq!(parsed.to_string(), encoded);
    }
}

#[test]
fn package_key_typed_round_trip_preserves_canonical_identity() {
    let peer = tapid_core::PeerContext::default()
        .with("react-dom".parse().unwrap(), "18.2.0".parse().unwrap());
    let platform =
        tapid_core::PlatformContext::new(Some("linux"), Some("x86_64"), Some("gnu")).unwrap();
    let populated = LockfilePackageKey::new(
        "https://registry.example.test".parse().unwrap(),
        "@scope/pkg".parse().unwrap(),
        "1.0.0".parse().unwrap(),
        &peer,
        &platform,
    );
    let empty = LockfilePackageKey::new(
        "https://registry.example.test".parse().unwrap(),
        "peer-pkg".parse().unwrap(),
        "1.0.0".parse().unwrap(),
        &tapid_core::PeerContext::default(),
        &tapid_core::PlatformContext::new(None, None, None).unwrap(),
    );

    for key in [populated, empty] {
        let parsed = key.to_string().parse::<LockfilePackageKey>().unwrap();
        assert_eq!(parsed, key);
    }
}

#[test]
fn rejects_ambiguous_or_noncanonical_contexts() {
    for encoded in [
        concat!("https://registry.example", "|pkg@1.0.0|peer=|platform=-"),
        concat!("https://registry.example", "|pkg@1.0.0|peer=-|platform="),
        concat!(
            "https://registry.example",
            "|pkg@1.0.0|peer=react@18.2.0,react@19.0.0|platform=-"
        ),
        concat!(
            "https://registry.example",
            "|pkg@1.0.0|peer=-|platform=linux-x86_64-gnu-extra"
        ),
        concat!(
            "https://registry.example",
            "|pkg@1.0.0|peer=-|platform=linux|custom"
        ),
    ] {
        assert!(
            encoded.parse::<crate::LockfilePackageKey>().is_err(),
            "accepted ambiguous key: {encoded}"
        );
    }
    for platform in [
        "linux",
        "linux-x86",
        "linux x86_64-gnu",
        "linux-x86_64-gnu\n",
    ] {
        let encoded = format!("https://registry.example|pkg@1.0.0|peer=-|platform={platform}");
        assert!(
            encoded.parse::<crate::LockfilePackageKey>().is_err(),
            "accepted non-canonical platform key: {encoded:?}"
        );
    }
}

#[test]
fn rejects_malformed_top_level_package_keys_during_json_validation() {
    let digest = "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for (field, value, marker, replacement) in [
        (
            "peerContext",
            "react@18.2.0,react@19.0.0",
            "|peer=-|",
            "|peer=react@18.2.0,react@19.0.0|",
        ),
        (
            "platformContext",
            "linux-x86_64-gnu-extra",
            "|platform=-",
            "|platform=linux-x86_64-gnu-extra",
        ),
    ] {
        let mut lockfile = Lockfile::new(digest).unwrap();
        lockfile.insert_package(package_fixture()).unwrap();
        let mut document: serde_json::Value =
            serde_json::from_str(&lockfile.to_json().unwrap()).unwrap();
        let packages = document["packages"].as_object_mut().unwrap();
        let (key, mut package) = packages
            .iter()
            .next()
            .map(|(key, package)| (key.clone(), package.clone()))
            .unwrap();
        packages.remove(&key);
        package[field] = serde_json::Value::String(value.to_owned());
        let malformed_key = key.replace(marker, replacement);
        packages.insert(malformed_key, package);
        assert!(Lockfile::from_json(&serde_json::to_string(&document).unwrap()).is_err());
    }
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
        "https://registry.example.test|tapid@1.0.0|peer=react@18.2.0|platform=linux-x86_64-gnu"
    );
}

#[test]
fn lockfile_v1_is_not_implicitly_accepted() {
    let input = r#"{"lockfileVersion":1,"rootManifestDigest":"sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","resolverVersion":"0","linkerVersion":"0","packages":{}}"#;
    assert!(Lockfile::from_json(input).is_err());
}
