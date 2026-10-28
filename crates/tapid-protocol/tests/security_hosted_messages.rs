use tapid_protocol::{
    EvidencePublicationRequest, SECURITY_CONTRACT_V1, SecurityErrorResponse, SecurityReason,
    SecurityServiceError,
};

fn publication() -> EvidencePublicationRequest {
    EvidencePublicationRequest {
        version: SECURITY_CONTRACT_V1.into(),
        tenant: "org-1".into(),
        evidence_digest: format!("sha256-{}", "a".repeat(64)),
        expected_sequence: 3,
    }
}

#[test]
fn publication_requires_canonical_digest_tenant_and_positive_safe_sequence() {
    let mut value = publication();
    assert_eq!(value.validate(), Ok(()));
    for (tenant, valid) in [
        ("".into(), false),
        ("  ".into(), false),
        ("org\n1".into(), false),
        ("a".repeat(4096), true),
        ("a".repeat(4097), false),
        ("é".repeat(2048), true),
        ("é".repeat(2049), false),
    ] {
        value = publication();
        value.tenant = tenant;
        assert_eq!(
            value.validate(),
            if valid {
                Ok(())
            } else {
                Err(SecurityReason::InvalidRequest)
            }
        );
    }
    for digest in [
        "".into(),
        "sha256-no".into(),
        format!("sha256-{}", "A".repeat(64)),
        format!("sha256-{}", "a".repeat(63)),
    ] {
        value = publication();
        value.evidence_digest = digest;
        assert_eq!(value.validate(), Err(SecurityReason::InvalidRequest));
    }
    for (sequence, valid) in [
        (0, false),
        (1, true),
        (9_007_199_254_740_991, true),
        (9_007_199_254_740_992, false),
    ] {
        value = publication();
        value.expected_sequence = sequence;
        assert_eq!(
            value.validate(),
            if valid {
                Ok(())
            } else {
                Err(SecurityReason::InvalidRequest)
            }
        );
    }
    value.version = "future".into();
    value.tenant.clear();
    assert_eq!(value.validate(), Err(SecurityReason::UnsupportedVersion));
}

#[test]
fn error_retry_metadata_is_optional_and_safe_with_version_precedence() {
    let mut value = SecurityErrorResponse {
        version: SECURITY_CONTRACT_V1.into(),
        code: SecurityServiceError::Unavailable,
        retry_after_seconds: None,
    };
    for retry in [None, Some(0), Some(9_007_199_254_740_991)] {
        value.retry_after_seconds = retry;
        assert_eq!(value.validate(), Ok(()));
    }
    value.retry_after_seconds = Some(9_007_199_254_740_992);
    assert_eq!(value.validate(), Err(SecurityReason::InvalidRequest));
    value.version = "future".into();
    assert_eq!(value.validate(), Err(SecurityReason::UnsupportedVersion));
}
