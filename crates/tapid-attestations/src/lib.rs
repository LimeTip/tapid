//! Typed claims for artifact-bound trust attestations.
#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::json;
use tapid_signatures::TrustEnvelope;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub conclusion: String,
    pub severity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttestationClaims {
    pub issuer: String,
    pub methodology: String,
    pub scope: String,
    pub issued_at: String,
    pub expires_at: Option<String>,
    pub findings: Vec<Finding>,
    pub confidence: f64,
    pub limitations: Vec<String>,
    /// Explicit disclosure, not a claim of independence or endorsement.
    pub payment_disclosure: String,
}

impl AttestationClaims {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.issuer.is_empty()
            || self.methodology.is_empty()
            || self.scope.is_empty()
            || self.issued_at.is_empty()
        {
            return Err("issuer, methodology, scope, and issued_at are required");
        }
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err("confidence must be between 0 and 1");
        }
        if self
            .expires_at
            .as_ref()
            .is_some_and(|expiry| expiry <= &self.issued_at)
        {
            return Err("expires_at must be after issued_at");
        }
        if self.payment_disclosure.is_empty() {
            return Err("payment_disclosure is required");
        }
        Ok(())
    }

    pub fn into_envelope(
        self,
        subject: impl Into<String>,
        artifact_digest: impl Into<String>,
    ) -> Result<TrustEnvelope, &'static str> {
        self.validate()?;
        Ok(TrustEnvelope::unsigned(
            subject,
            artifact_digest,
            json!(self),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn claims() -> AttestationClaims {
        AttestationClaims {
            issuer: "lab.example".into(),
            methodology: "static-v1".into(),
            scope: "artifact bytes".into(),
            issued_at: "2026-01-01T00:00:00Z".into(),
            expires_at: Some("2027-01-01T00:00:00Z".into()),
            findings: vec![Finding {
                id: "F-1".into(),
                conclusion: "pass".into(),
                severity: None,
            }],
            confidence: 0.95,
            limitations: vec!["no runtime test".into()],
            payment_disclosure: "paid engagement".into(),
        }
    }
    #[test]
    fn claims_are_artifact_bound() {
        let e = claims()
            .into_envelope("security-review", "sha256-a")
            .unwrap();
        assert_eq!(e.subject, "security-review");
        assert_eq!(e.artifact_digest, "sha256-a");
        assert!(
            e.signing_bytes()
                .unwrap()
                .windows(b"payment_disclosure".len())
                .any(|w| w == b"payment_disclosure")
        );
    }
    #[test]
    fn invalid_confidence_rejected() {
        let mut c = claims();
        c.confidence = 1.1;
        assert_eq!(c.validate(), Err("confidence must be between 0 and 1"));
    }
    #[test]
    fn expiry_tamper_rejected() {
        let mut c = claims();
        c.expires_at = Some("2025-01-01T00:00:00Z".into());
        assert!(c.validate().is_err());
    }
}
