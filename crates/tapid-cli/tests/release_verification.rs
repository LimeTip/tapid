use base64::Engine;
use ed25519_dalek::SigningKey;
use std::{fs, process::Command};
use tapid_signatures::{TrustEnvelope, digest};
use tapid_test_support::{TempHome, TempProject};

#[test]
fn native_verifier_authenticates_old_records_and_rejects_invalid_inputs() {
    let project = TempProject::new("native-release-verifier").unwrap();
    let home = TempHome::new("native-release-verifier").unwrap();
    let record = project
        .write("record.tsv", b"immutable record bytes\n")
        .unwrap();
    let secret = [7; 32];
    let public = SigningKey::from_bytes(&secret).verifying_key().to_bytes();
    let keyring = project
        .write(
            "keyring.json",
            &serde_json::to_vec(&serde_json::json!({
                "version": tapid_signatures::KEY_RING_VERSION,
                "keys": [{"key_id": "fixture", "algorithm": "ed25519",
                    "public_key": base64::engine::general_purpose::STANDARD.encode(public),
                    "fingerprint": digest(&public).unwrap()}]
            }))
            .unwrap(),
        )
        .unwrap();
    let sidecar = project.path().join("record.tsv.sig");
    let verify = |custom_key: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .env_clear()
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("TAPID_RELEASE_KEYRING", &keyring)
            .arg("__verify-release-record")
            .arg(&record)
            .arg(&sidecar);
        if custom_key {
            command.arg("--keyring").arg(&keyring);
        }
        command.output().unwrap()
    };
    let sign = |claims| {
        let envelope = TrustEnvelope::unsigned(
            "tapid-release-v1",
            digest(b"immutable record bytes\n").unwrap(),
            claims,
        )
        .sign("fixture", &secret)
        .unwrap();
        fs::write(&sidecar, serde_json::to_vec(&envelope).unwrap()).unwrap();
    };
    let claims = serde_json::json!({"schema": "tapid-release-v1-immutable-signature", "created_at": "2000-01-01T00:00:00Z"});
    sign(claims.clone());
    let success = verify(true);
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    assert!(
        !verify(false).status.success(),
        "production keyring must reject fixture key"
    );
    fs::write(&record, b"tampered\n").unwrap();
    assert!(!verify(true).status.success());
    fs::write(&record, b"immutable record bytes\n").unwrap();
    for invalid in [
        serde_json::json!({"schema": "tapid-release-v1-immutable-signature", "created_at": "2999-01-01T00:00:00Z"}),
        serde_json::json!({"schema": "tapid-release-v1-immutable-signature", "created_at": "2000-01-01T00:00:00Z", "expires_at": "2001-01-01T00:00:00Z"}),
    ] {
        sign(invalid);
        assert!(!verify(true).status.success());
    }
    sign(claims);
    fs::write(&sidecar, b"not JSON").unwrap();
    assert!(!verify(true).status.success());
    fs::write(&sidecar, vec![b' '; 256 * 1024 + 1]).unwrap();
    assert!(!verify(true).status.success());
    let help = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&help.stdout).contains("__verify-release-record"));
}
