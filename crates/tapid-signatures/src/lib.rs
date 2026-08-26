//! Canonical, artifact-bound trust envelopes with Ed25519 signing and verification.
#![deny(unsafe_code)]

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt};

pub mod release;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ENVELOPE_VERSION: &str = "tapid-trust-envelope-v1";
pub const DIGEST_ALGORITHM: &str = "sha256";
pub const SIGNATURE_ALGORITHM: &str = "ed25519";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedKey {
    pub key_id: String,
    pub algorithm: String,
    pub public_key: [u8; 32],
}

#[derive(Clone, Debug, Default)]
pub struct KeyRing {
    keys: BTreeMap<String, TrustedKey>,
}

impl KeyRing {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: TrustedKey) -> Result<(), VerificationError> {
        validate_key_id(&key.key_id)?;
        if key.algorithm != SIGNATURE_ALGORITHM {
            return Err(VerificationError::UnsupportedAlgorithm(key.algorithm));
        }
        self.keys.insert(key.key_id.clone(), key);
        Ok(())
    }

    pub(crate) fn get_for_release(&self, key_id: &str) -> Option<&TrustedKey> {
        self.keys.get(key_id)
    }

    fn get(&self, key_id: &str) -> Result<&TrustedKey, VerificationError> {
        self.keys
            .get(key_id)
            .ok_or_else(|| VerificationError::UnknownKeyId(key_id.to_owned()))
    }
}

fn validate_key_id(key_id: &str) -> Result<(), VerificationError> {
    if key_id.is_empty()
        || key_id.len() > 128
        || !key_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    {
        return Err(VerificationError::InvalidKeyId(key_id.to_owned()));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DetachedSignature {
    pub algorithm: String,
    pub key_id: String,
    /// The exact envelope subject and digest the signature claims to cover.
    pub subject: String,
    pub artifact_digest: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrustEnvelope {
    pub version: String,
    pub subject: String,
    pub artifact_digest: String,
    pub claims: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<DetachedSignature>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerificationError {
    Unsigned,
    SubjectMismatch,
    ArtifactDigestMismatch,
    UnsupportedAlgorithm(String),
    UnknownKeyId(String),
    KeyIdMismatch,
    InvalidKeyId(String),
    ManifestDigestMismatch,
    PublicKeyRequired,
    InvalidEnvelope(String),
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsigned => write!(
                f,
                "envelope is unsigned; cryptographic verification is unavailable"
            ),
            Self::SubjectMismatch => write!(f, "signature subject does not match envelope subject"),
            Self::ArtifactDigestMismatch => {
                write!(f, "signature artifact digest does not match envelope")
            }
            Self::UnsupportedAlgorithm(a) => write!(f, "unsupported signature algorithm: {a}"),
            Self::UnknownKeyId(k) => write!(f, "unknown trusted key ID: {k}"),
            Self::KeyIdMismatch => write!(f, "signature key ID does not match trusted key"),
            Self::InvalidKeyId(k) => write!(f, "invalid key ID: {k}"),
            Self::ManifestDigestMismatch => {
                write!(f, "release manifest digest does not match signature")
            }
            Self::PublicKeyRequired => {
                write!(f, "a trusted public key is required for verification")
            }
            Self::InvalidEnvelope(e) => write!(f, "invalid envelope: {e}"),
        }
    }
}

impl TrustEnvelope {
    pub fn unsigned(
        subject: impl Into<String>,
        artifact_digest: impl Into<String>,
        claims: Value,
    ) -> Self {
        Self {
            version: ENVELOPE_VERSION.into(),
            subject: subject.into(),
            artifact_digest: artifact_digest.into(),
            claims,
            signature: None,
        }
    }

    /// Canonical unsigned envelope bytes for inspection and digesting.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        self.signing_bytes_with_context(None, None)
    }

    fn signing_bytes_with_context(
        &self,
        algorithm: Option<&str>,
        key_id: Option<&str>,
    ) -> Result<Vec<u8>, CanonicalError> {
        let mut value = Map::new();
        value.insert(
            "artifact_digest".into(),
            Value::String(self.artifact_digest.clone()),
        );
        value.insert("claims".into(), self.claims.clone());
        value.insert("subject".into(), Value::String(self.subject.clone()));
        value.insert("version".into(), Value::String(self.version.clone()));
        if let (Some(algorithm), Some(key_id)) = (algorithm, key_id) {
            let mut context = Map::new();
            context.insert("algorithm".into(), Value::String(algorithm.into()));
            context.insert("key_id".into(), Value::String(key_id.into()));
            value.insert("signature_context".into(), Value::Object(context));
        }
        canonical_json(&Value::Object(value)).map(|s| s.into_bytes())
    }

    /// Signs the canonical envelope payload with an Ed25519 seed.
    pub fn sign(
        &self,
        key_id: impl Into<String>,
        secret_key: &[u8; 32],
    ) -> Result<Self, CanonicalError> {
        let key_id = key_id.into();
        validate_key_id(&key_id).map_err(|e| CanonicalError(e.to_string()))?;
        let signing_key = SigningKey::from_bytes(secret_key);
        let signature = signing_key
            .sign(&self.signing_bytes_with_context(Some(SIGNATURE_ALGORITHM), Some(&key_id))?);
        let mut signed = self.clone();
        signed.signature = Some(DetachedSignature {
            algorithm: SIGNATURE_ALGORITHM.into(),
            key_id,
            subject: self.subject.clone(),
            artifact_digest: self.artifact_digest.clone(),
            value: BASE64.encode(signature.to_bytes()),
        });
        Ok(signed)
    }

    /// Verifies against a caller-owned trusted keyring.
    pub fn verify_with_keyring(&self, keyring: &KeyRing) -> Result<(), VerificationError> {
        let signature = self.signature.as_ref().ok_or(VerificationError::Unsigned)?;
        validate_key_id(&signature.key_id)?;
        let key = keyring.get(&signature.key_id)?;
        self.verify_with_trusted_key(key)
    }

    /// Low-level verification with a key whose identity was established by the caller.
    pub fn verify_with_public_key(&self, public_key: &[u8; 32]) -> Result<(), VerificationError> {
        let signature = self.signature.as_ref().ok_or(VerificationError::Unsigned)?;
        let key = TrustedKey {
            key_id: signature.key_id.clone(),
            algorithm: signature.algorithm.clone(),
            public_key: *public_key,
        };
        self.verify_with_trusted_key(&key)
    }

    fn verify_with_trusted_key(&self, key: &TrustedKey) -> Result<(), VerificationError> {
        let detached = self.signature.as_ref().ok_or(VerificationError::Unsigned)?;
        validate_key_id(&detached.key_id)?;
        if detached.key_id != key.key_id {
            return Err(VerificationError::KeyIdMismatch);
        }
        if detached.algorithm != key.algorithm || detached.algorithm != SIGNATURE_ALGORITHM {
            return Err(VerificationError::UnsupportedAlgorithm(
                detached.algorithm.clone(),
            ));
        }
        if detached.subject != self.subject {
            return Err(VerificationError::SubjectMismatch);
        }
        if detached.artifact_digest != self.artifact_digest {
            return Err(VerificationError::ArtifactDigestMismatch);
        }
        let verifying_key = VerifyingKey::from_bytes(&key.public_key)
            .map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))?;
        let signature_bytes = BASE64
            .decode(&detached.value)
            .map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))?;
        let crypto_signature = Signature::from_slice(&signature_bytes)
            .map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))?;
        verifying_key
            .verify(
                &self
                    .signing_bytes_with_context(Some(&detached.algorithm), Some(&detached.key_id))
                    .map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))?,
                &crypto_signature,
            )
            .map_err(|e| VerificationError::InvalidEnvelope(e.to_string()))
    }

    pub fn envelope_digest(&self) -> Result<String, CanonicalError> {
        digest(&self.signing_bytes()?)
    }

    /// Validates binding fields, but never treats an unsigned or unsupported
    /// signature as valid. No cryptographic implementation is implied here.
    pub fn verify(&self) -> Result<(), VerificationError> {
        let signature = self.signature.as_ref().ok_or(VerificationError::Unsigned)?;
        validate_key_id(&signature.key_id)?;
        if signature.algorithm != SIGNATURE_ALGORITHM {
            return Err(VerificationError::UnsupportedAlgorithm(
                signature.algorithm.clone(),
            ));
        }
        if signature.subject != self.subject {
            return Err(VerificationError::SubjectMismatch);
        }
        if signature.artifact_digest != self.artifact_digest {
            return Err(VerificationError::ArtifactDigestMismatch);
        }
        Err(VerificationError::PublicKeyRequired)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalError(String);
impl fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for CanonicalError {}

/// RFC 8785 JSON Canonicalization Scheme serialization.
pub fn canonical_json(value: &Value) -> Result<String, CanonicalError> {
    serde_jcs::to_string(value).map_err(|e| CanonicalError(e.to_string()))
}

pub fn digest(bytes: &[u8]) -> Result<String, CanonicalError> {
    let mut h = Sha256::new();
    h.update(bytes);
    Ok(format!(
        "sha256-{}",
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_order_is_deterministic() {
        let a = serde_json::json!({"z":1,"a":{"y":true,"x":null}});
        let b = serde_json::json!({"a":{"x":null,"y":true},"z":1});
        assert_eq!(
            canonical_json(&a).unwrap(),
            r#"{"a":{"x":null,"y":true},"z":1}"#
        );
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }
    #[test]
    fn golden_signing_bytes_and_digest() {
        let e = TrustEnvelope::unsigned(
            "audit",
            "sha256-artifact",
            serde_json::json!({"finding":"none"}),
        );
        assert_eq!(
            String::from_utf8(e.signing_bytes().unwrap()).unwrap(),
            r#"{"artifact_digest":"sha256-artifact","claims":{"finding":"none"},"subject":"audit","version":"tapid-trust-envelope-v1"}"#
        );
        assert_eq!(
            e.envelope_digest().unwrap(),
            "sha256-9fe8071f73cbfb3a9dbb180cf2417adc26212a65708e6c7d92199bc4a09068c3"
        );
    }
    #[test]
    fn unsigned_is_not_verified() {
        let e = TrustEnvelope::unsigned("audit", "sha256-a", Value::Null);
        assert_eq!(e.verify(), Err(VerificationError::Unsigned));
    }
    #[test]
    fn tampered_binding_is_rejected() {
        let mut e = TrustEnvelope::unsigned("audit", "sha256-a", Value::Null);
        e.signature = Some(DetachedSignature {
            algorithm: "future".into(),
            key_id: "k".into(),
            subject: "other".into(),
            artifact_digest: "sha256-a".into(),
            value: "x".into(),
        });
        assert_eq!(
            e.verify(),
            Err(VerificationError::UnsupportedAlgorithm("future".into()))
        );
    }

    #[test]
    fn signs_and_verifies_with_ed25519() {
        let envelope = TrustEnvelope::unsigned(
            "release-manifest",
            "sha256-artifact",
            serde_json::json!({"version":"0.0.6"}),
        );
        let secret_key = [7u8; 32];
        let signed = envelope.sign("release-key-1", &secret_key).unwrap();
        assert_eq!(signed.signature.as_ref().unwrap().algorithm, "ed25519");
        let public_key = SigningKey::from_bytes(&secret_key)
            .verifying_key()
            .to_bytes();
        assert!(signed.verify_with_public_key(&public_key).is_ok());
    }

    #[test]
    fn rejects_tampered_claims_and_wrong_key() {
        let envelope = TrustEnvelope::unsigned("release", "sha256-a", Value::Null);
        let secret_key = [9u8; 32];
        let signed = envelope.sign("key-1", &secret_key).unwrap();
        let mut tampered = signed.clone();
        tampered.claims = serde_json::json!({"version":"0.0.7"});
        let public_key = SigningKey::from_bytes(&secret_key)
            .verifying_key()
            .to_bytes();
        let wrong_public_key = SigningKey::from_bytes(&[8u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(tampered.verify_with_public_key(&public_key).is_err());
        assert!(signed.verify_with_public_key(&wrong_public_key).is_err());
    }

    #[test]
    fn rejects_malformed_signature_encoding() {
        let mut envelope = TrustEnvelope::unsigned("release", "sha256-a", Value::Null);
        envelope.signature = Some(DetachedSignature {
            algorithm: "ed25519".into(),
            key_id: "key-1".into(),
            subject: envelope.subject.clone(),
            artifact_digest: envelope.artifact_digest.clone(),
            value: "not-base64".into(),
        });
        assert!(envelope.verify_with_public_key(&[0u8; 32]).is_err());
    }

    #[test]
    fn key_id_is_authenticated_and_invalid_ids_are_rejected() {
        let secret = [7u8; 32];
        let signed = TrustEnvelope::unsigned("release", "sha256-a", Value::Null)
            .sign("release-key-1", &secret)
            .unwrap();
        let public_key = SigningKey::from_bytes(&secret).verifying_key().to_bytes();
        let mut ring = KeyRing::new();
        ring.insert(TrustedKey {
            key_id: "release-key-1".into(),
            algorithm: SIGNATURE_ALGORITHM.into(),
            public_key,
        })
        .unwrap();
        let mut tampered = signed.clone();
        tampered.signature.as_mut().unwrap().key_id = "release-key-2".into();
        assert!(tampered.verify_with_keyring(&ring).is_err());
        assert!(
            ring.insert(TrustedKey {
                key_id: "bad key".into(),
                algorithm: SIGNATURE_ALGORITHM.into(),
                public_key,
            })
            .is_err()
        );
    }

    #[test]
    fn canonicalization_handles_jcs_numbers_and_unicode_keys() {
        let value = serde_json::json!({
            "é": 1e-6,
            "😀": -0.0,
            "nested": {"z": 1.0, "a": 2}
        });
        assert_eq!(
            canonical_json(&value).unwrap(),
            r#"{"nested":{"a":2,"z":1},"é":0.000001,"😀":0}"#
        );
    }
}
