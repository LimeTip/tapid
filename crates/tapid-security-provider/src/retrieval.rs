use std::io::Read;
use tapid_attestations::{
    EvidenceFreshness, SecurityTrust, VerifiedSecurityEvidenceV2, verify_security_evidence_v2,
};
use tapid_protocol::{SecurityReason, SecurityRequest};

/// Disclosure consent covers the entire request, including peers, placement,
/// audience and requested checks. Public report visibility is not consent.
/// This is independent of service credential authorization.
#[derive(Default)]
pub enum Disclosure<'a> {
    #[default]
    Denied,
    Exact(&'a SecurityRequest),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrievalError {
    NotAuthorized,
    Unavailable,
    Evidence(SecurityReason),
}

/// Retrieve raw envelopes through an explicitly supplied adapter. No default
/// network adapter, implicit credentials, environment keys or project I/O exist.
/// The adapter returns a decompressed stream. Retrieval reads at most the wire
/// limit plus one byte, before parsing or accepting any evidence. The adapter
/// must enforce transport timeouts; this function does not interrupt a read.
///
/// The caller must load durable issuer/request replay floors into `freshness`
/// and persist the accepted sequence/digest before evidence affects policy.
/// Successful verification alone does not establish complete audit coverage.
pub fn retrieve<R: Read>(
    request: &SecurityRequest,
    disclosure: &Disclosure<'_>,
    trust: &SecurityTrust<'_>,
    freshness: &EvidenceFreshness<'_>,
    transport: &mut impl FnMut(&SecurityRequest) -> Result<R, RetrievalError>,
) -> Result<VerifiedSecurityEvidenceV2, RetrievalError> {
    if !matches!(disclosure, Disclosure::Exact(allowed) if *allowed == request) {
        return Err(RetrievalError::NotAuthorized);
    }
    request.validate_v2().map_err(RetrievalError::Evidence)?;
    let stream = transport(request)?;
    let mut bytes = Vec::new();
    stream
        .take(tapid_protocol::MAX_SECURITY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| RetrievalError::Unavailable)?;
    verify_security_evidence_v2(&bytes, request, trust, freshness).map_err(RetrievalError::Evidence)
}
