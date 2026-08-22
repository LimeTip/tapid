//! Portable transparency inclusion evidence.
//!
//! An inclusion record is evidence about a log checkpoint and proof path. It
//! intentionally contains no registry URL, lookup promise, or availability
//! assertion.
#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::json;
use tapid_signatures::{TrustEnvelope, canonical_json};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransparencyInclusionRecord {
    pub envelope_digest: String,
    pub log_id: String,
    pub tree_size: u64,
    pub leaf_index: u64,
    pub inclusion_path: Vec<String>,
    pub witnessed_at: String,
}

impl TransparencyInclusionRecord {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.envelope_digest.is_empty() || self.log_id.is_empty() || self.witnessed_at.is_empty()
        {
            return Err("digest, log_id, and witnessed_at are required");
        }
        if self.leaf_index >= self.tree_size {
            return Err("leaf_index must be less than tree_size");
        }
        if self.inclusion_path.iter().any(|h| h.is_empty()) {
            return Err("inclusion path entries must be non-empty");
        }
        Ok(())
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, tapid_signatures::CanonicalError> {
        canonical_json(&json!(self)).map(String::into_bytes)
    }
    pub fn binds_to(
        &self,
        envelope: &TrustEnvelope,
    ) -> Result<bool, tapid_signatures::CanonicalError> {
        Ok(self.envelope_digest == envelope.envelope_digest()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> TransparencyInclusionRecord {
        TransparencyInclusionRecord {
            envelope_digest: "sha256-envelope".into(),
            log_id: "log-2026".into(),
            tree_size: 3,
            leaf_index: 1,
            inclusion_path: vec!["aa".into(), "bb".into()],
            witnessed_at: "2026-01-01T00:00:00Z".into(),
        }
    }
    #[test]
    fn canonical_record_is_deterministic() {
        let r = record();
        assert_eq!(r.canonical_bytes().unwrap(), br#"{"envelope_digest":"sha256-envelope","inclusion_path":["aa","bb"],"leaf_index":1,"log_id":"log-2026","tree_size":3,"witnessed_at":"2026-01-01T00:00:00Z"}"#);
    }
    #[test]
    fn invalid_leaf_is_rejected() {
        let mut r = record();
        r.leaf_index = 3;
        assert!(r.validate().is_err());
    }
    #[test]
    fn record_does_not_claim_registry_availability() {
        let text = String::from_utf8(record().canonical_bytes().unwrap()).unwrap();
        assert!(!text.contains("registry"));
        assert!(!text.contains("url"));
    }
}
