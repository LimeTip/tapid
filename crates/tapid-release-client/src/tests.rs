use super::*;
use ed25519_dalek::SigningKey;
use serde_json::json;
use tapid_signatures::{release, KeyRing, TrustedKey};
use std::collections::BTreeMap;

const NOW: &str = "2026-08-27T12:00:00Z";
const TARGET: &str = "x86_64-unknown-linux-gnu";
const SECRET: [u8; 32] = [7; 32];
fn keyring() -> KeyRing { let mut r = KeyRing::new(); r.insert(TrustedKey { key_id: "release-key-1".into(), algorithm: "ed25519".into(), public_key: SigningKey::from_bytes(&SECRET).verifying_key().to_bytes() }).unwrap(); r }
fn manifest() -> serde_json::Value { json!({"schema":"tapid-release-manifest-v1","product":"tapid","version":"0.0.6","tag":"v0.0.6","commit":"0123456789abcdef0123456789abcdef01234567","created_at":"2026-08-27T10:00:00Z","expires_at":"2026-09-27T10:00:00Z","artifacts":[{"name":"tapid-0.0.6-x86_64-unknown-linux-gnu.tar.gz","target":TARGET,"url":"https://example.test/tapid.tar.gz","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":5}]}) }
fn signed() -> Vec<u8> { serde_json::to_vec(&release::sign(manifest(), "release-key-1", &SECRET).unwrap()).unwrap() }
fn channel(urls: &[&str]) -> Vec<u8> { serde_json::to_vec(&json!({"channel": "stable", "manifests": urls})).unwrap() }
fn digest(bytes: &[u8]) -> String { use sha2::{Digest, Sha256}; format!("{:x}", Sha256::digest(bytes)) }
struct Fake { responses: BTreeMap<String, Result<Vec<u8>, String>>, calls: Vec<String> }
impl Fetcher for Fake {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.calls.push(url.into());
        self.responses.remove(url).unwrap_or_else(|| Err("missing".into()))
    }

    fn fetch_with_limit(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        let body = self.fetch(url)?;
        if body.len() > max_bytes {
            return Err("response exceeds maximum size".into());
        }
        Ok(body)
    }

    fn fetch_metadata_with_limit(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, Error> {
        let body = self.fetch(url).map_err(Error::Fetch)?;
        if body.len() > max_bytes {
            return Err(Error::InvalidManifest("response exceeds maximum size".into()));
        }
        Ok(body)
    }
}

#[test]
fn discovery_preserves_manifest_rejections_before_later_valid_candidates() {
    let mut stale = manifest();
    stale["expires_at"] = json!("2026-08-27T11:59:59Z");
    let stale = serde_json::to_vec(&release::sign(stale, "release-key-1", &SECRET).unwrap()).unwrap();
    let mut tampered: serde_json::Value = serde_json::from_slice(&signed()).unwrap();
    tampered["commit"] = json!("a".repeat(40));
    let cases = [
        (b"not JSON".to_vec(), TARGET, "malformed"),
        (stale, TARGET, "stale"),
        (signed(), "aarch64-unknown-linux-gnu", "target"),
        (serde_json::to_vec(&tampered).unwrap(), TARGET, "signature"),
    ];
    for (body, target, case) in cases {
        let mut f = Fake {
            responses: [
                ("https://one.test/index".into(), Ok(channel(&["https://one.test/bad", "https://one.test/good"]))),
                ("https://one.test/bad".into(), Ok(body)),
                ("https://one.test/good".into(), Ok(signed())),
                ("https://two.test/index".into(), Ok(channel(&["https://two.test/good"]))),
                ("https://two.test/good".into(), Ok(signed())),
            ].into_iter().collect(),
            calls: vec![],
        };
        let result = discover(&mut f, &["https://one.test/index", "https://two.test/index"], &keyring(), target, NOW, None);
        assert!(matches!((&result, case),
            (Err(Error::InvalidManifest(_)), "malformed")
            | (Err(Error::StaleMetadata), "stale")
            | (Err(Error::TargetNotFound(_)), "target")
            | (Err(Error::Signature(_)), "signature")
        ), "{case}: {result:?}");
        assert_eq!(f.calls, ["https://one.test/index", "https://one.test/bad"]);
    }
}

#[test]
fn verifies_signed_manifest_and_matching_artifact() { let bytes = b"hello"; let mut v = manifest(); v["artifacts"][0]["sha256"] = json!(digest(bytes)); let body = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); let r = ReleaseManifest::parse_and_verify(&body, &keyring(), TARGET, NOW, None).unwrap(); assert_eq!(r.artifact().unwrap().size, 5); }
#[test]
fn rejects_invalid_signature_unknown_key_and_stale_metadata() { let mut v = serde_json::from_slice::<serde_json::Value>(&signed()).unwrap(); v["signature"]["value"] = json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="); assert!(matches!(ReleaseManifest::parse_and_verify(&serde_json::to_vec(&v).unwrap(), &keyring(), TARGET, NOW, None), Err(Error::Signature(_)))); let mut v = serde_json::from_slice::<serde_json::Value>(&signed()).unwrap(); v["signature"]["key_id"] = json!("unknown"); assert!(matches!(ReleaseManifest::parse_and_verify(&serde_json::to_vec(&v).unwrap(), &keyring(), TARGET, NOW, None), Err(Error::Signature(_)))); let mut v = manifest(); v["expires_at"] = json!("2026-08-27T11:59:59Z"); let body = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); assert!(matches!(ReleaseManifest::parse_and_verify(&body, &keyring(), TARGET, NOW, None), Err(Error::StaleMetadata))); }
#[test]
fn rejects_unknown_fields_non_https_and_wrong_target() { let mut v = manifest(); v["future"] = json!(true); let b = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); assert!(matches!(ReleaseManifest::parse_and_verify(&b, &keyring(), TARGET, NOW, None), Err(Error::InvalidManifest(_)))); let mut v = manifest(); v["artifacts"][0]["url"] = json!("http://example.test/a"); let b = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); assert!(matches!(ReleaseManifest::parse_and_verify(&b, &keyring(), TARGET, NOW, None), Err(Error::InvalidManifest(_)))); assert!(matches!(ReleaseManifest::parse_and_verify(&signed(), &keyring(), "aarch64-unknown-linux-gnu", NOW, None), Err(Error::TargetNotFound(_)))); }
#[test]
fn verifies_artifact_hash_and_size() { let mut v = manifest(); v["artifacts"][0]["sha256"] = json!(digest(b"hello")); let b = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); let r = ReleaseManifest::parse_and_verify(&b, &keyring(), TARGET, NOW, None).unwrap(); assert!(r.verify_artifact(b"hello").is_ok()); assert!(matches!(r.verify_artifact(b"tampered"), Err(Error::ArtifactSizeMismatch { .. }))); }
#[test]
fn follows_channel_index_and_falls_back_in_order() { let mut v = manifest(); v["artifacts"][0]["sha256"] = json!(digest(b"hello")); let body = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); let mut f = Fake { responses: [("https://one.test/stable.json".into(), Ok(channel(&["https://one.test/manifest"]))), ("https://one.test/manifest".into(), Err("outage".into())), ("https://two.test/stable.json".into(), Ok(channel(&["https://two.test/manifest"]))), ("https://two.test/manifest".into(), Ok(body))].into_iter().collect(), calls: vec![] }; let r = discover(&mut f, &["https://one.test/stable.json", "https://two.test/stable.json"], &keyring(), TARGET, NOW, None).unwrap(); assert_eq!(r.artifact().unwrap().url, "https://example.test/tapid.tar.gz"); assert_eq!(f.calls, vec!["https://one.test/stable.json", "https://one.test/manifest", "https://two.test/stable.json", "https://two.test/manifest"]); }

#[test]
fn rejects_invalid_channel_index() { let mut f = Fake { responses: [("https://example.test/stable.json".into(), Ok(serde_json::to_vec(&json!({"channel": "beta", "manifests": ["https://example.test/manifest"]})).unwrap()))].into_iter().collect(), calls: vec![] }; let err = discover(&mut f, &["https://example.test/stable.json"], &keyring(), TARGET, NOW, None).unwrap_err(); assert!(matches!(err, Error::InvalidManifest(_))); }

#[test]
fn tries_manifest_urls_in_index_order_and_bounds_fan_out() { let mut v = manifest(); v["artifacts"][0]["sha256"] = json!(digest(b"hello")); let body = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); let mut responses = BTreeMap::new(); responses.insert("https://example.test/stable.json".into(), Ok(channel(&["https://example.test/first", "https://example.test/second"]))); responses.insert("https://example.test/first".into(), Err("invalid".into())); responses.insert("https://example.test/second".into(), Ok(body)); let mut f = Fake { responses, calls: vec![] }; assert!(discover(&mut f, &["https://example.test/stable.json"], &keyring(), TARGET, NOW, None).is_ok()); assert_eq!(f.calls, vec!["https://example.test/stable.json", "https://example.test/first", "https://example.test/second"]); let too_many: Vec<_> = (0..17).map(|_| "https://example.test/manifest").collect(); let mut f = Fake { responses: [("https://example.test/stable.json".into(), Ok(channel(&too_many)))].into_iter().collect(), calls: vec![] }; assert!(discover(&mut f, &["https://example.test/stable.json"], &keyring(), TARGET, NOW, None).is_err()); assert_eq!(f.calls, vec!["https://example.test/stable.json"]); }
struct LimitAwareFake {
    responses: BTreeMap<String, Result<Vec<u8>, String>>,
    calls: Vec<(String, usize)>,
}
impl Fetcher for LimitAwareFake {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>, String> {
        panic!("unbounded fetch used for {url}");
    }

    fn fetch_with_limit(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        self.calls.push((url.into(), max_bytes));
        self.responses.remove(url).unwrap_or_else(|| Err("missing".into()))
    }

    fn fetch_metadata_with_limit(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, Error> {
        // Deliberately return oversized fixtures to exercise discover's defensive check.
        self.calls.push((url.into(), max_bytes));
        self.responses.remove(url).unwrap_or_else(|| Err("missing".into())).map_err(Error::Fetch)
    }
}

#[test]
fn typed_response_rejection_is_not_an_outage() {
    struct Rejected;
    impl Fetcher for Rejected {
        fn fetch(&mut self, _: &str) -> Result<Vec<u8>, String> { panic!("unbounded fetch") }
        fn fetch_with_limit(&mut self, _: &str, _: usize) -> Result<Vec<u8>, String> { panic!("untyped fetch") }
        fn fetch_metadata_with_limit(&mut self, _: &str, _: usize) -> Result<Vec<u8>, Error> {
            Err(Error::InvalidManifest("response exceeds maximum size".into()))
        }
    }
    let result = discover(&mut Rejected, &["https://example.test/stable.json"], &keyring(), TARGET, NOW, None);
    assert!(matches!(result, Err(Error::InvalidManifest(_))), "{result:?}");
}

#[test]
fn oversized_received_metadata_is_not_an_outage() {
    for oversized_index in [true, false] {
        let mut f = LimitAwareFake {
            responses: [
                ("https://example.test/stable.json".into(), Ok(if oversized_index {
                    vec![b' '; MAX_CHANNEL_INDEX_BYTES + 1]
                } else {
                    channel(&["https://example.test/manifest"])
                })),
                ("https://example.test/manifest".into(), Ok(vec![b' '; MAX_MANIFEST_BYTES + 1])),
            ].into_iter().collect(),
            calls: vec![],
        };
        let result = discover(&mut f, &["https://example.test/stable.json"], &keyring(), TARGET, NOW, None);
        assert!(matches!(result, Err(Error::InvalidManifest(_))), "{result:?}");
    }
}

#[test]
fn discovery_passes_metadata_limits_to_custom_fetchers() {
    let mut f = LimitAwareFake {
        responses: [
            ("https://example.test/stable.json".into(), Ok(channel(&["https://example.test/manifest"]))),
            ("https://example.test/manifest".into(), Err("invalid".into())),
        ]
        .into_iter()
        .collect(),
        calls: vec![],
    };
    assert!(discover(&mut f, &["https://example.test/stable.json"], &keyring(), TARGET, NOW, None).is_err());
    assert_eq!(
        f.calls,
        vec![
            ("https://example.test/stable.json".into(), super::MAX_CHANNEL_INDEX_BYTES),
            ("https://example.test/manifest".into(), super::MAX_MANIFEST_BYTES),
        ]
    );
}

#[test]
fn ignores_oversized_channel_index_before_parsing() { let body = vec![b" "[0]; super::MAX_CHANNEL_INDEX_BYTES + 1]; let mut f = Fake { responses: [("https://example.test/stable.json".into(), Ok(body))].into_iter().collect(), calls: vec![] }; assert!(discover(&mut f, &["https://example.test/stable.json"], &keyring(), TARGET, NOW, None).is_err()); assert_eq!(f.calls, vec!["https://example.test/stable.json"]); }

#[test]
fn rejects_oversized_manifest_before_parsing() {
    let mut v = manifest();
    v["artifacts"][0]["sha256"] = json!(digest(b"hello"));
    let mut body = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap();
    body.resize(super::MAX_MANIFEST_BYTES + 1, b' ');
    let mut f = Fake {
        responses: [
            ("https://example.test/stable.json".into(), Ok(channel(&["https://example.test/manifest"]))),
            ("https://example.test/manifest".into(), Ok(body)),
        ]
        .into_iter()
        .collect(),
        calls: vec![],
    };
    assert!(discover(&mut f, &["https://example.test/stable.json"], &keyring(), TARGET, NOW, None).is_err());
    assert_eq!(f.calls, vec!["https://example.test/stable.json", "https://example.test/manifest"]);
}

#[test]
fn last_known_good_round_trips_and_replaces_atomically() { let state = LastKnownGood { version: "0.0.6".into(), artifact_sha256: "a".repeat(64) }; let dir = std::env::temp_dir().join(format!("tapid-release-{}", std::process::id())); let _ = std::fs::create_dir_all(&dir); let path = dir.join("state.json"); write_last_known_good(&path, &state).unwrap(); assert_eq!(read_last_known_good(&path).unwrap(), state); let _ = std::fs::remove_dir_all(dir); }

#[test]
fn release_state_rejects_replay_and_downgrade() {
    let dir = tempfile_dir(); let path = dir.join("state.json");
    let state = ReleaseState::new("0.0.6", 6, "a".repeat(64)).unwrap();
    write_release_state(&path, &state).unwrap();
    assert!(matches!(accept_release(&state, "0.0.6", 6, "b".repeat(64)), Err(Error::ReleaseReplay { .. })));
    assert!(matches!(accept_release(&state, "0.0.5", 7, "b".repeat(64)), Err(Error::ReleaseDowngrade { .. })));
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn malformed_state_and_symlink_are_rejected() {
    let dir = tempfile_dir(); let path = dir.join("state.json");
    std::fs::write(&path, b"{\"schema\":\"tapid-release-state-v2\"}").unwrap();
    assert!(matches!(read_release_state(&path), Err(Error::State(_))));
    let target = dir.join("target"); std::fs::write(&target, b"old").unwrap();
    let link = dir.join("link"); std::os::unix::fs::symlink(&target, &link).unwrap();
    let state = ReleaseState::new("0.0.6", 6, "a".repeat(64)).unwrap();
    assert!(matches!(write_release_state(&link, &state), Err(Error::State(_))));
    assert_eq!(std::fs::read(&target).unwrap(), b"old"); let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn interrupted_replacement_preserves_previous_state() {
    let dir = tempfile_dir();
    let path = dir.join("state.json");
    let old = ReleaseState::new("0.0.6", 6, "a".repeat(64)).unwrap();
    write_release_state(&path, &old).unwrap();
    let temp = dir.join(format!(".state.json.tmp-{}", std::process::id()));
    std::fs::write(&temp, b"interrupted").unwrap();
    let newer = ReleaseState::new("0.0.7", 7, "b".repeat(64)).unwrap();
    assert!(write_release_state(&path, &newer).is_err());
    assert_eq!(read_release_state(&path).unwrap(), old);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn all_endpoints_fail_with_deterministic_error() {
    let mut f = Fake { responses: BTreeMap::new(), calls: vec![] };
    let err = discover(&mut f, &["https://one.test/stable.json", "https://two.test/stable.json"], &keyring(), TARGET, NOW, None).unwrap_err();
    assert!(matches!(err, Error::AllEndpointsFailed { attempts: 2 }));
}

fn tempfile_dir() -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("tapid-release-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())); std::fs::create_dir_all(&d).unwrap(); d }
