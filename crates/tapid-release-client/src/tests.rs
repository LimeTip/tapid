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
fn digest(bytes: &[u8]) -> String { use sha2::{Digest, Sha256}; format!("{:x}", Sha256::digest(bytes)) }
struct Fake { responses: BTreeMap<String, Result<Vec<u8>, String>>, calls: Vec<String> }
impl Fetcher for Fake { fn fetch(&mut self, url: &str) -> Result<Vec<u8>, String> { self.calls.push(url.into()); self.responses.remove(url).unwrap_or_else(|| Err("missing".into())) } }

#[test]
fn verifies_signed_manifest_and_matching_artifact() { let bytes = b"hello"; let mut v = manifest(); v["artifacts"][0]["sha256"] = json!(digest(bytes)); let body = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); let r = ReleaseManifest::parse_and_verify(&body, &keyring(), TARGET, NOW, None).unwrap(); assert_eq!(r.artifact().unwrap().size, 5); }
#[test]
fn rejects_invalid_signature_unknown_key_and_stale_metadata() { let mut v = serde_json::from_slice::<serde_json::Value>(&signed()).unwrap(); v["signature"]["value"] = json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="); assert!(matches!(ReleaseManifest::parse_and_verify(&serde_json::to_vec(&v).unwrap(), &keyring(), TARGET, NOW, None), Err(Error::Signature(_)))); let mut v = serde_json::from_slice::<serde_json::Value>(&signed()).unwrap(); v["signature"]["key_id"] = json!("unknown"); assert!(matches!(ReleaseManifest::parse_and_verify(&serde_json::to_vec(&v).unwrap(), &keyring(), TARGET, NOW, None), Err(Error::Signature(_)))); let mut v = manifest(); v["expires_at"] = json!("2026-08-27T11:59:59Z"); let body = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); assert!(matches!(ReleaseManifest::parse_and_verify(&body, &keyring(), TARGET, NOW, None), Err(Error::StaleMetadata))); }
#[test]
fn rejects_unknown_fields_non_https_and_wrong_target() { let mut v = manifest(); v["future"] = json!(true); let b = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); assert!(matches!(ReleaseManifest::parse_and_verify(&b, &keyring(), TARGET, NOW, None), Err(Error::InvalidManifest(_)))); let mut v = manifest(); v["artifacts"][0]["url"] = json!("http://example.test/a"); let b = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); assert!(matches!(ReleaseManifest::parse_and_verify(&b, &keyring(), TARGET, NOW, None), Err(Error::InvalidManifest(_)))); assert!(matches!(ReleaseManifest::parse_and_verify(&signed(), &keyring(), "aarch64-unknown-linux-gnu", NOW, None), Err(Error::TargetNotFound(_)))); }
#[test]
fn verifies_artifact_hash_and_size() { let mut v = manifest(); v["artifacts"][0]["sha256"] = json!(digest(b"hello")); let b = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); let r = ReleaseManifest::parse_and_verify(&b, &keyring(), TARGET, NOW, None).unwrap(); assert!(r.verify_artifact(b"hello").is_ok()); assert!(matches!(r.verify_artifact(b"tampered"), Err(Error::ArtifactSizeMismatch { .. }))); }
#[test]
fn falls_back_in_order_and_returns_verified_release() { let mut v = manifest(); v["artifacts"][0]["sha256"] = json!(digest(b"hello")); let body = serde_json::to_vec(&release::sign(v, "release-key-1", &SECRET).unwrap()).unwrap(); let mut f = Fake { responses: [("https://one.test/manifest".into(), Err("outage".into())), ("https://two.test/manifest".into(), Ok(body))].into_iter().collect(), calls: vec![] }; let r = discover(&mut f, &["https://one.test/manifest", "https://two.test/manifest"], &keyring(), TARGET, NOW, None).unwrap(); assert_eq!(r.artifact().unwrap().url, "https://example.test/tapid.tar.gz"); assert_eq!(f.calls, vec!["https://one.test/manifest", "https://two.test/manifest"]); }
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
    let err = discover(&mut f, &["https://one.test/manifest", "https://two.test/manifest"], &keyring(), TARGET, NOW, None).unwrap_err();
    assert!(matches!(err, Error::AllEndpointsFailed { attempts: 2 }));
}

fn tempfile_dir() -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("tapid-release-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())); std::fs::create_dir_all(&d).unwrap(); d }
