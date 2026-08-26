use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{Map, Value};

use crate::{
    CanonicalError, KeyRing, SIGNATURE_ALGORITHM, VerificationError, digest, validate_key_id,
};

/// Returns RFC 8785 JCS UTF-8 bytes for a release manifest without its signature.
pub fn signing_bytes(manifest: &Value) -> Result<Vec<u8>, CanonicalError> {
    let mut unsigned = manifest
        .as_object()
        .cloned()
        .ok_or_else(|| CanonicalError("release manifest must be a JSON object".into()))?;
    unsigned.remove("signature");
    crate::canonical_json(&Value::Object(unsigned)).map(|s| s.into_bytes())
}

/// Signs a schema-shaped release manifest using the v1 signature fields.
pub fn sign(
    mut manifest: Value,
    key_id: impl Into<String>,
    secret_key: &[u8; 32],
) -> Result<Value, CanonicalError> {
    let key_id = key_id.into();
    validate_key_id(&key_id).map_err(|e| CanonicalError(e.to_string()))?;
    let bytes = signing_bytes(&manifest)?;
    let signature = SigningKey::from_bytes(secret_key).sign(&bytes);
    let mut detached = Map::new();
    detached.insert(
        "algorithm".into(),
        Value::String(SIGNATURE_ALGORITHM.into()),
    );
    detached.insert("key_id".into(), Value::String(key_id));
    detached.insert("signed_digest".into(), Value::String(digest(&bytes)?));
    detached.insert(
        "value".into(),
        Value::String(BASE64.encode(signature.to_bytes())),
    );
    manifest
        .as_object_mut()
        .ok_or_else(|| CanonicalError("release manifest must be a JSON object".into()))?
        .insert("signature".into(), Value::Object(detached));
    Ok(manifest)
}

/// Verifies a release manifest against the explicitly trusted keyring.
pub fn verify(manifest: &Value, keyring: &KeyRing) -> Result<(), VerificationError> {
    let object = manifest.as_object().ok_or_else(|| {
        VerificationError::InvalidEnvelope("release manifest must be an object".into())
    })?;
    let signature = object
        .get("signature")
        .and_then(Value::as_object)
        .ok_or(VerificationError::Unsigned)?;
    if signature.keys().any(|field| {
        !matches!(
            field.as_str(),
            "algorithm" | "key_id" | "signed_digest" | "value"
        )
    }) {
        return Err(VerificationError::InvalidEnvelope(
            "signature contains unknown fields".into(),
        ));
    }
    let algorithm = string_field(signature, "algorithm")?;
    let key_id = string_field(signature, "key_id")?;
    let signed_digest = string_field(signature, "signed_digest")?;
    let value = string_field(signature, "value")?;
    if algorithm != SIGNATURE_ALGORITHM {
        return Err(VerificationError::UnsupportedAlgorithm(algorithm));
    }
    validate_key_id(&key_id)?;
    if signed_digest.len() != 71
        || !signed_digest.starts_with("sha256-")
        || !signed_digest[7..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(VerificationError::InvalidEnvelope(
            "signature.signed_digest must be a sha256 digest".into(),
        ));
    }
    let key = keyring
        .get_for_release(&key_id)
        .ok_or_else(|| VerificationError::UnknownKeyId(key_id.clone()))?;
    if key.algorithm != algorithm {
        return Err(VerificationError::UnsupportedAlgorithm(
            key.algorithm.clone(),
        ));
    }
    let bytes =
        signing_bytes(manifest).map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))?;
    if digest(&bytes).map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))?
        != signed_digest
    {
        return Err(VerificationError::ManifestDigestMismatch);
    }
    let signature_bytes = BASE64
        .decode(value)
        .map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))?;
    if signature_bytes.len() != 64 {
        return Err(VerificationError::InvalidEnvelope(
            "signature.value must decode to 64 bytes".into(),
        ));
    }
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))?;
    VerifyingKey::from_bytes(&key.public_key)
        .map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))?
        .verify(&bytes, &signature)
        .map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))
}

fn string_field(object: &Map<String, Value>, field: &str) -> Result<String, VerificationError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            VerificationError::InvalidEnvelope(format!("signature.{field} must be a string"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KeyRing, TrustedKey};
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    fn manifest() -> Value {
        json!({
            "schema": "tapid-release-manifest-v1",
            "product": "tapid",
            "version": "0.0.6",
            "tag": "v0.0.6",
            "commit": "0123456789abcdef0123456789abcdef01234567",
            "created_at": "2026-08-25T10:00:00Z",
            "expires_at": "2026-09-25T10:00:00Z",
            "artifacts": [{
                "name": "tapid-0.0.6-x86_64-unknown-linux-gnu.tar.gz",
                "target": "x86_64-unknown-linux-gnu",
                "url": "https://github.com/LimeTip/tapid/releases/download/v0.0.6/tapid-0.0.6-x86_64-unknown-linux-gnu.tar.gz",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "size": 1234
            }]
        })
    }

    fn keyring(secret: &[u8; 32]) -> KeyRing {
        let mut ring = KeyRing::new();
        ring.insert(TrustedKey {
            key_id: "release-key-1".into(),
            algorithm: SIGNATURE_ALGORITHM.into(),
            public_key: SigningKey::from_bytes(secret).verifying_key().to_bytes(),
        })
        .unwrap();
        ring
    }

    #[test]
    fn signs_and_verifies_schema_shaped_manifest() {
        let secret = [7u8; 32];
        let signed = sign(manifest(), "release-key-1", &secret).unwrap();
        assert!(
            signed["signature"]["signed_digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256-")
        );
        assert!(verify(&signed, &keyring(&secret)).is_ok());
    }

    #[test]
    fn rejects_manifest_digest_and_key_tampering() {
        let secret = [7u8; 32];
        let mut signed = sign(manifest(), "release-key-1", &secret).unwrap();
        signed["artifacts"][0]["size"] = json!(1235);
        assert_eq!(
            verify(&signed, &keyring(&secret)),
            Err(VerificationError::ManifestDigestMismatch)
        );

        let mut relabeled = sign(manifest(), "release-key-1", &secret).unwrap();
        relabeled["signature"]["key_id"] = json!("release-key-2");
        assert_eq!(
            verify(&relabeled, &keyring(&secret)),
            Err(VerificationError::UnknownKeyId("release-key-2".into()))
        );
    }

    #[test]
    fn rejects_unsupported_algorithm() {
        let secret = [7u8; 32];
        let mut signed = sign(manifest(), "release-key-1", &secret).unwrap();
        signed["signature"]["algorithm"] = json!("rsa-pss");
        assert_eq!(
            verify(&signed, &keyring(&secret)),
            Err(VerificationError::UnsupportedAlgorithm("rsa-pss".into()))
        );
    }
}
