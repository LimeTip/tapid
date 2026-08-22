//! Typed claims for artifact-bound trust attestations.
#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::json;
use tapid_signatures::TrustEnvelope;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

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
        let issued_at = parse_canonical_timestamp(&self.issued_at, "issued_at")?;
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err("confidence must be between 0 and 1");
        }
        if let Some(expiry) = &self.expires_at {
            let expires_at = parse_canonical_timestamp(expiry, "expires_at")?;
            if expires_at <= issued_at {
                return Err("expires_at must be after issued_at");
            }
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

fn parse_canonical_timestamp(
    value: &str,
    field: &'static str,
) -> Result<OffsetDateTime, &'static str> {
    let timestamp = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| timestamp_error(field))?;
    let canonical = timestamp
        .format(&Rfc3339)
        .map_err(|_| timestamp_error(field))?;
    if canonical != value {
        return Err(timestamp_error(field));
    }
    Ok(timestamp)
}

fn timestamp_error(field: &str) -> &'static str {
    match field {
        "issued_at" => "issued_at must be a canonical RFC 3339 timestamp",
        "expires_at" => "expires_at must be a canonical RFC 3339 timestamp",
        _ => "timestamp must be a canonical RFC 3339 timestamp",
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

    #[test]
    fn timestamps_with_offsets_are_compared_as_instants() {
        let mut c = claims();
        c.issued_at = "2026-01-01T01:00:00+01:00".into();
        c.expires_at = Some("2026-01-01T00:30:00Z".into());
        assert!(c.validate().is_ok());
    }

    #[test]
    fn equal_instants_are_not_a_valid_expiry() {
        let mut c = claims();
        c.issued_at = "2026-01-01T00:00:00Z".into();
        c.expires_at = Some("2026-01-01T01:00:00+01:00".into());
        assert_eq!(c.validate(), Err("expires_at must be after issued_at"));
    }

    #[test]
    fn malformed_timestamp_is_rejected() {
        let mut c = claims();
        c.issued_at = "not-a-timestamp".into();
        assert_eq!(
            c.validate(),
            Err("issued_at must be a canonical RFC 3339 timestamp")
        );
    }

    #[test]
    fn timestamp_without_timezone_is_rejected() {
        let mut c = claims();
        c.issued_at = "2026-01-01T00:00:00".into();
        assert_eq!(
            c.validate(),
            Err("issued_at must be a canonical RFC 3339 timestamp")
        );
    }

    #[test]
    fn non_canonical_timestamp_is_rejected() {
        let mut c = claims();
        c.issued_at = "2026-01-01 00:00:00Z".into();
        assert_eq!(
            c.validate(),
            Err("issued_at must be a canonical RFC 3339 timestamp")
        );
    }

    #[test]
    fn canonical_timestamp_round_trip_is_accepted() {
        let mut c = claims();
        c.issued_at = "2026-01-01T00:00:00.123456789Z".into();
        c.expires_at = Some("2026-01-01T02:00:00.123456789+01:00".into());
        assert!(c.validate().is_ok());
    }
}
