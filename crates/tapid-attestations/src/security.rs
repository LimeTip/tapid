use serde::Deserialize;
use tapid_protocol::{
    MAX_SECURITY_BYTES, SECURITY_CONTRACT_V1, SecurityCheck, SecurityReason, SecurityRequest,
    SecurityResult,
};
use tapid_signatures::{KeyRing, TrustEnvelope};

/// Caller-owned trust policy. Keys here must be authorized for this issuer only.
/// Revoked keys must be absent. Never use the release keyring as an evidence root.
pub struct SecurityTrust<'a> {
    pub issuer: &'a str,
    pub keys: &'a KeyRing,
    /// Expiry of the independently authenticated trust configuration, Unix seconds.
    pub valid_until: u64,
}

/// Replay state is loaded from trusted local state, scoped to issuer + exact request.
pub struct EvidenceFreshness<'a> {
    pub now: u64,
    pub max_age: u64,
    pub minimum_sequence: u64,
    /// Required at equal sequence, preventing equivocation and repeated refreshes.
    pub previous_digest: Option<&'a str>,
}

/// Only this returned type represents accepted issuer evidence. Authenticity does
/// not establish scanner accuracy or safety. Revalidate cached bytes on every use.
#[derive(Debug)]
pub struct VerifiedSecurityEvidence {
    result: SecurityResult,
    envelope_digest: String,
}

impl VerifiedSecurityEvidence {
    pub fn result(&self) -> &SecurityResult {
        &self.result
    }
    pub fn envelope_digest(&self) -> &str {
        &self.envelope_digest
    }
}

// Strict outer parsing avoids silently accepting envelope extensions. Existing
// generic TrustEnvelope intentionally remains a separate compatibility contract.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecurityEnvelope {
    version: String,
    subject: String,
    artifact_digest: String,
    claims: SecurityResult,
    signature: SecuritySignature,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecuritySignature {
    algorithm: String,
    key_id: String,
    subject: String,
    artifact_digest: String,
    value: String,
}

/// Bounded untrusted JSON -> authenticated, applicable, current evidence.
/// The caller supplies a validated local request, trust root, clock and replay
/// state. This function performs no I/O and never updates replay state on error.
pub fn verify_security_evidence(
    bytes: &[u8],
    request: &SecurityRequest,
    trust: &SecurityTrust<'_>,
    freshness: &EvidenceFreshness<'_>,
) -> Result<VerifiedSecurityEvidence, SecurityReason> {
    if bytes.len() > MAX_SECURITY_BYTES {
        return Err(SecurityReason::InputTooLarge);
    }
    request.validate()?;
    let parsed: SecurityEnvelope =
        serde_json::from_slice(bytes).map_err(|_| SecurityReason::Malformed)?;
    if parsed.version != tapid_signatures::ENVELOPE_VERSION
        || parsed.subject != SECURITY_CONTRACT_V1
    {
        return Err(SecurityReason::InvalidEnvelope);
    }
    parsed.claims.validate()?;
    let signature = tapid_signatures::DetachedSignature {
        algorithm: parsed.signature.algorithm,
        key_id: parsed.signature.key_id,
        subject: parsed.signature.subject,
        artifact_digest: parsed.signature.artifact_digest,
        value: parsed.signature.value,
    };
    // Retain the received claims values, including omitted optional fields,
    // rather than round-tripping through typed serialization before verification.
    let envelope: TrustEnvelope =
        serde_json::from_slice(bytes).map_err(|_| SecurityReason::Malformed)?;
    if parsed.claims.issuer != trust.issuer {
        return Err(SecurityReason::UnknownIssuer);
    }
    if !trust.keys.contains_key(&signature.key_id) {
        return Err(SecurityReason::UntrustedKey);
    }
    envelope
        .verify_with_keyring(trust.keys)
        .map_err(|_| SecurityReason::InvalidSignature)?;
    if &parsed.claims.request != request {
        return Err(SecurityReason::SubjectMismatch);
    }
    let binding = match &request.check {
        SecurityCheck::Advisory => "version-scope",
        SecurityCheck::Artifact { digest } => digest,
    };
    if parsed.artifact_digest != binding {
        return Err(SecurityReason::DigestMismatch);
    }
    let result = parsed.claims;
    if freshness.now >= trust.valid_until
        || freshness.now >= result.expires_at
        || freshness.now.saturating_sub(result.checked_at) > freshness.max_age
    {
        return Err(SecurityReason::Expired);
    }
    if result.issued_at > freshness.now {
        return Err(SecurityReason::FutureEvidence);
    }
    let envelope_digest = envelope
        .envelope_digest()
        .map_err(|_| SecurityReason::InvalidEnvelope)?;
    if result.sequence < freshness.minimum_sequence
        || (result.sequence == freshness.minimum_sequence
            && freshness.previous_digest != Some(envelope_digest.as_str()))
    {
        return Err(SecurityReason::Replay);
    }
    Ok(VerifiedSecurityEvidence {
        result,
        envelope_digest,
    })
}
