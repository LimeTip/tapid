//! Canonical, artifact-bound trust envelopes.
//!
//! This crate deliberately does not claim to sign or verify a cryptographic
//! signature yet. It provides the stable bytes and binding contract that a
//! future, fully tested signature implementation must use.
#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fmt;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ENVELOPE_VERSION: &str = "tapid-trust-envelope-v1";
pub const DIGEST_ALGORITHM: &str = "sha256";

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

    /// Bytes bound by a future signature: version, subject, digest, and claims.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        let mut value = Map::new();
        value.insert(
            "artifact_digest".into(),
            Value::String(self.artifact_digest.clone()),
        );
        value.insert("claims".into(), self.claims.clone());
        value.insert("subject".into(), Value::String(self.subject.clone()));
        value.insert("version".into(), Value::String(self.version.clone()));
        canonical_json(&Value::Object(value)).map(|s| s.into_bytes())
    }

    pub fn envelope_digest(&self) -> Result<String, CanonicalError> {
        digest(&self.signing_bytes()?)
    }

    /// Validates binding fields, but never treats an unsigned or unsupported
    /// signature as valid. No cryptographic implementation is implied here.
    pub fn verify(&self) -> Result<(), VerificationError> {
        let signature = self.signature.as_ref().ok_or(VerificationError::Unsigned)?;
        if signature.subject != self.subject {
            return Err(VerificationError::SubjectMismatch);
        }
        if signature.artifact_digest != self.artifact_digest {
            return Err(VerificationError::ArtifactDigestMismatch);
        }
        Err(VerificationError::UnsupportedAlgorithm(
            signature.algorithm.clone(),
        ))
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

/// RFC 8785-shaped JSON canonicalization for the supported JSON subset:
/// object keys are UTF-8 lexicographically sorted, arrays retain order, and
/// serde_json supplies JSON string escaping and number validation.
pub fn canonical_json(value: &Value) -> Result<String, CanonicalError> {
    fn write(v: &Value, out: &mut String) -> Result<(), CanonicalError> {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::String(s) => {
                out.push_str(&serde_json::to_string(s).map_err(|e| CanonicalError(e.to_string()))?)
            }
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out)?;
                }
                out.push(']');
            }
            Value::Object(m) => {
                out.push('{');
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(
                        &serde_json::to_string(k).map_err(|e| CanonicalError(e.to_string()))?,
                    );
                    out.push(':');
                    write(&m[*k], out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }
    let mut out = String::new();
    write(value, &mut out)?;
    Ok(out)
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
        assert_eq!(e.verify(), Err(VerificationError::SubjectMismatch));
    }
}
