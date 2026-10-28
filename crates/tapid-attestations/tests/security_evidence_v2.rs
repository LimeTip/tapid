// The frozen reader also defines hosted DTOs exercised by the v1 suite.
#[allow(dead_code)]
#[path = "support/security_v1_consumer.rs"]
mod previous_consumer;

#[path = "support/security_v2_consumer.rs"]
mod current_consumer;

use serde_json::{Value, json};
use tapid_attestations::{
    EvidenceFreshness, SecurityTrust, VerifiedSecurityEvidenceV2, verify_security_evidence,
    verify_security_evidence_v2,
};
use tapid_protocol::{
    MAX_SECURITY_BYTES, ReportClassification, ReportState, SecurityReason, SecurityRequest,
    SecurityResultV2, SecurityStatus,
};
use tapid_signatures::{KeyRing, TrustEnvelope, TrustedKey};

// RFC 8032 public test material. Never a production issuer root.
fn hex<const N: usize>(s: &str) -> [u8; N] {
    std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}
fn keys() -> KeyRing {
    let mut keys = KeyRing::new();
    keys.insert(TrustedKey {
        key_id: "fixture".into(),
        algorithm: "ed25519".into(),
        public_key: hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
    })
    .unwrap();
    keys
}
fn request() -> SecurityRequest {
    let mut request: Value =
        serde_json::from_slice(include_bytes!("fixtures/security-request-v1.json")).unwrap();
    request["version"] = json!("tapid-security-v2");
    request["check"] = json!({"kind":"advisory"});
    request["coverage"] = json!(["advisory", "reported_malware"]);
    serde_json::from_value(request).unwrap()
}
fn result() -> SecurityResultV2 {
    serde_json::from_value(json!({
        "version":"tapid-security-v2", "request": request(), "issuer":"fixture.example",
        "sequence":3, "status":"findings", "scanner":"fixture", "scannerVersion":"1",
        "ruleVersion":"1", "scope":"package_version", "coverage": ["advisory", "reported_malware"],
        "sourceRevision":"reports:3", "checkedAt":1000, "issuedAt":1010, "expiresAt":2000,
        "findings":[{"id":"REPORT-1", "revision":"2", "classification":"advisory",
            "state":"active", "summary":"Example advisory", "severity":"high",
            "fixes":[{"version":"1.2.4"}]}], "limitations":[]
    }))
    .unwrap()
}
fn sign(value: &SecurityResultV2) -> Vec<u8> {
    let binding = match &value.request.check {
        tapid_protocol::SecurityCheck::Advisory => "version-scope",
        tapid_protocol::SecurityCheck::Artifact { digest } => digest,
    };
    serde_json::to_vec(
        &TrustEnvelope::unsigned("tapid-security-v2", binding, json!(value))
            .sign(
                "fixture",
                &hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"),
            )
            .unwrap(),
    )
    .unwrap()
}
fn freshness() -> EvidenceFreshness<'static> {
    EvidenceFreshness {
        now: 1100,
        max_age: 500,
        minimum_sequence: 0,
        previous_digest: None,
    }
}
fn check(
    bytes: &[u8],
    request: &SecurityRequest,
    freshness: &EvidenceFreshness<'_>,
) -> Result<VerifiedSecurityEvidenceV2, SecurityReason> {
    verify_security_evidence_v2(
        bytes,
        request,
        &SecurityTrust {
            issuer: "fixture.example",
            keys: &keys(),
            valid_until: 3000,
        },
        freshness,
    )
}

#[test]
fn signed_v2_advisory_preserves_classification_and_fix_and_is_rejected_by_v1() {
    let bytes = sign(&result());
    let verified = check(&bytes, &request(), &freshness()).unwrap();
    assert_eq!(
        verified.result().findings[0].classification,
        ReportClassification::Advisory
    );
    assert_eq!(verified.result().findings[0].fixes[0].version, "1.2.4");
    assert_eq!(verified.result().findings[0].id, "REPORT-1");
    assert_eq!(verified.result().findings[0].revision, "2");
    assert!(serde_json::from_slice::<previous_consumer::Envelope>(&bytes).is_err());
    let independent = current_consumer::verify(&bytes, &keys());
    assert_eq!(independent.claims.findings[0].id, "REPORT-1");
    assert_eq!(independent.claims.findings[0].revision, "2");
    assert_eq!(
        verify_security_evidence(
            &bytes,
            &request(),
            &SecurityTrust {
                issuer: "fixture.example",
                keys: &keys(),
                valid_until: 3000
            },
            &freshness()
        )
        .unwrap_err(),
        SecurityReason::UnsupportedVersion
    );
}

#[test]
fn withdrawn_revision_is_explicit_and_cannot_resurrect_an_older_active_result() {
    let active = sign(&result());
    let mut withdrawn = result();
    withdrawn.sequence = 4;
    withdrawn.status = SecurityStatus::InvalidEvidence;
    withdrawn.coverage.clear();
    withdrawn.findings[0].state = ReportState::Withdrawn;
    withdrawn.findings[0].revision = "3".into();
    withdrawn
        .limitations
        .push("Report withdrawn by issuer".into());
    let bytes = sign(&withdrawn);
    let verified = check(&bytes, &request(), &freshness()).unwrap();
    assert_eq!(verified.result().status, SecurityStatus::InvalidEvidence);
    assert_eq!(verified.result().findings[0].state, ReportState::Withdrawn);
    let floor = EvidenceFreshness {
        minimum_sequence: 4,
        previous_digest: Some(verified.envelope_digest()),
        ..freshness()
    };
    assert_eq!(
        check(&active, &request(), &floor).unwrap_err(),
        SecurityReason::Replay
    );
    assert!(check(&bytes, &request(), &floor).is_ok());
    withdrawn.status = SecurityStatus::NoKnownFindings;
    withdrawn.coverage = request().coverage;
    assert_eq!(withdrawn.validate(), Err(SecurityReason::InvalidResult));
}

#[test]
fn registry_coverage_cannot_be_an_unrecognized_scanner_label() {
    let mut request = request();
    request.coverage = vec!["some-other-scan".into()];
    assert_eq!(request.validate_v2(), Err(SecurityReason::InvalidRequest));
}

#[test]
fn frozen_v1_stays_usable_and_v2_refuses_silent_downgrade() {
    let bytes = include_bytes!("fixtures/advisory-v1.json");
    let mut keys = keys();
    keys.insert(TrustedKey {
        key_id: "fixture-1".into(),
        algorithm: "ed25519".into(),
        public_key: hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
    })
    .unwrap();
    previous_consumer::verify(bytes, &keys);
    assert_eq!(
        check(bytes, &request(), &freshness()).unwrap_err(),
        SecurityReason::InvalidEnvelope
    );
}

#[test]
fn exact_request_context_remains_bound_in_v2() {
    for field in [
        "registry",
        "name",
        "version",
        "alias",
        "placement",
        "peer",
        "platform",
        "audience",
        "coverage",
    ] {
        let mut changed = result();
        match field {
            "registry" => {
                changed.request.subject.package.registry = "https://registry.npmjs.org".into()
            }
            "name" => changed.request.subject.package.name = "other".into(),
            "version" => changed.request.subject.package.version = "2.0.0".into(),
            "alias" => changed.request.subject.requested_name = "other".into(),
            "placement" => changed.request.subject.placement = "another/node_modules/alias".into(),
            "peer" => {
                changed
                    .request
                    .subject
                    .peers
                    .get_mut("peer")
                    .unwrap()
                    .registry = "https://registry.npmjs.org".into()
            }
            "platform" => changed.request.subject.platform.os = Some("darwin".into()),
            "audience" => {
                changed.request.audience = tapid_protocol::EvidenceAudience::Tenant {
                    tenant: "other".into(),
                }
            }
            "coverage" => {
                changed.request.coverage = vec!["advisory".into()];
                changed.coverage = changed.request.coverage.clone();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            check(&sign(&changed), &request(), &freshness()).unwrap_err(),
            SecurityReason::SubjectMismatch,
            "{field}"
        );
    }
}

#[test]
fn v2_rejects_tampering_expiry_future_evidence_and_equivocation() {
    let bytes = sign(&result());
    let mut tampered: Value = serde_json::from_slice(&bytes).unwrap();
    tampered["claims"]["findings"][0]["revision"] = json!("forged");
    assert_eq!(
        check(
            &serde_json::to_vec(&tampered).unwrap(),
            &request(),
            &freshness()
        )
        .unwrap_err(),
        SecurityReason::InvalidSignature
    );
    let accepted = check(&bytes, &request(), &freshness()).unwrap();
    let mut changed = result();
    changed.findings[0].revision = "new".into();
    let floor = EvidenceFreshness {
        minimum_sequence: 3,
        previous_digest: Some(accepted.envelope_digest()),
        ..freshness()
    };
    assert_eq!(
        check(&sign(&changed), &request(), &floor).unwrap_err(),
        SecurityReason::Replay
    );
    for (now, expected) in [
        (2000, SecurityReason::Expired),
        (1600, SecurityReason::Expired),
        (1005, SecurityReason::FutureEvidence),
    ] {
        assert_eq!(
            check(
                &bytes,
                &request(),
                &EvidenceFreshness { now, ..freshness() }
            )
            .unwrap_err(),
            expected
        );
    }
    changed.issuer = "other.example".into();
    assert_eq!(
        check(&sign(&changed), &request(), &freshness()).unwrap_err(),
        SecurityReason::UnknownIssuer
    );
}

#[test]
fn v2_parsing_is_bounded_and_closed_at_every_new_field() {
    assert_eq!(
        check(
            &vec![b' '; MAX_SECURITY_BYTES + 1],
            &request(),
            &freshness()
        )
        .unwrap_err(),
        SecurityReason::InputTooLarge
    );
    assert_eq!(
        check(b"{}", &request(), &freshness()).unwrap_err(),
        SecurityReason::Malformed
    );
    for pointer in [
        "/claims/unknown",
        "/claims/findings/0/unknown",
        "/claims/findings/0/fixes/0/unknown",
    ] {
        let mut value: Value = serde_json::from_slice(&sign(&result())).unwrap();
        // JSON pointer insertion needs the parent object for a new key.
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        value
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), json!(true));
        assert_eq!(
            check(
                &serde_json::to_vec(&value).unwrap(),
                &request(),
                &freshness()
            )
            .unwrap_err(),
            SecurityReason::Malformed,
            "{pointer}"
        );
    }
    for (field, value) in [
        ("classification", json!("new_kind")),
        ("state", json!("new_state")),
    ] {
        let mut body = json!(result());
        body["findings"][0][field] = value;
        assert!(serde_json::from_value::<SecurityResultV2>(body).is_err());
    }
}

#[test]
fn advisory_and_malware_coverage_are_independent_and_unknown_metadata_is_preserved() {
    let mut value = result();
    value.findings[0].classification = ReportClassification::ReportedMalware;
    value.findings[0].severity = None;
    value.findings[0].fixes.clear();
    let accepted = check(&sign(&value), &request(), &freshness()).unwrap();
    assert_eq!(
        accepted.result().findings[0].classification,
        ReportClassification::ReportedMalware
    );
    assert_eq!(accepted.result().findings[0].severity, None);
    value.coverage = vec!["advisory".into()];
    assert_eq!(value.validate(), Err(SecurityReason::IncompleteCoverage));
    value.request.coverage = value.coverage.clone();
    assert_eq!(value.validate(), Err(SecurityReason::InvalidResult));
}

#[test]
fn artifact_evidence_binds_existing_digest_without_claiming_version_only_coverage() {
    let mut value = result();
    let digest = format!("sha256-{}", "a".repeat(64));
    value.request.check = tapid_protocol::SecurityCheck::Artifact {
        digest: digest.clone(),
    };
    value.request.coverage = vec!["archive-static".into()];
    value.coverage = value.request.coverage.clone();
    value.scope = tapid_protocol::SecurityScope::ArtifactBytes;
    assert!(check(&sign(&value), &value.request, &freshness()).is_ok());
    let mut wrong_binding: Value = serde_json::from_slice(&sign(&value)).unwrap();
    wrong_binding["artifact_digest"] = json!("version-scope");
    let envelope: TrustEnvelope = serde_json::from_value(wrong_binding).unwrap();
    let bytes = serde_json::to_vec(
        &envelope
            .sign(
                "fixture",
                &hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        check(&bytes, &value.request, &freshness()).unwrap_err(),
        SecurityReason::DigestMismatch
    );
    value.scope = tapid_protocol::SecurityScope::PackageVersion;
    assert_eq!(value.validate(), Err(SecurityReason::InvalidResult));
}

#[test]
fn v2_rejects_ambiguous_reports_invalid_fixes_and_resource_overflow() {
    for case in [
        "duplicate",
        "empty_revision",
        "unsafe_integer",
        "invalid_fix",
        "excess_findings",
        "excess_fixes",
        "control_text",
        "contradictory_status",
    ] {
        let mut value = result();
        match case {
            "duplicate" => value.findings.push(value.findings[0].clone()),
            "empty_revision" => value.findings[0].revision.clear(),
            "unsafe_integer" => value.sequence = 9_007_199_254_740_992,
            "invalid_fix" => value.findings[0].fixes[0].version = "latest".into(),
            "excess_findings" => value.findings = vec![value.findings[0].clone(); 257],
            "excess_fixes" => {
                value.findings[0].fixes = vec![value.findings[0].fixes[0].clone(); 129]
            }
            "control_text" => value.findings[0].summary = "escape\u{001b}[2J".into(),
            "contradictory_status" => value.status = SecurityStatus::NoKnownFindings,
            _ => unreachable!(),
        }
        assert_eq!(
            value.validate(),
            Err(SecurityReason::InvalidResult),
            "{case}"
        );
    }
}

#[test]
fn frozen_v2_fixture_agrees_with_independent_consumer_on_report_identity_and_revision() {
    let bytes = include_bytes!("fixtures/advisory-v2.json");
    let current = check(bytes, &request(), &freshness()).unwrap();
    let independent = current_consumer::verify(bytes, &keys());
    assert_eq!(current.result().findings[0].id, "REPORT-1");
    assert_eq!(independent.claims.findings[0].id, "REPORT-1");
    assert_eq!(current.result().findings[0].revision, "2");
    assert_eq!(independent.claims.findings[0].revision, "2");
    assert!(matches!(
        independent.claims.findings[0].classification,
        current_consumer::Classification::Advisory
    ));
    assert_eq!(independent.claims.findings[0].fixes[0].version, "1.2.4");
}

#[test]
fn v2_requires_current_independently_configured_issuer_trust() {
    let bytes = sign(&result());
    let keyring = keys();
    for (keys, valid_until, expected) in [
        (&KeyRing::new(), 3000, SecurityReason::UntrustedKey),
        (&keyring, 1100, SecurityReason::Expired),
    ] {
        assert_eq!(
            verify_security_evidence_v2(
                &bytes,
                &request(),
                &SecurityTrust {
                    issuer: "fixture.example",
                    keys,
                    valid_until,
                },
                &freshness()
            )
            .unwrap_err(),
            expected
        );
    }
}

#[test]
fn size_limit_precedes_request_validation_for_both_contracts() {
    let mut invalid = request();
    invalid.version = "unsupported".into();
    let bytes = vec![b' '; MAX_SECURITY_BYTES + 1];
    let keyring = keys();
    let trust = SecurityTrust {
        issuer: "fixture.example",
        keys: &keyring,
        valid_until: 3000,
    };
    assert_eq!(
        verify_security_evidence(&bytes, &invalid, &trust, &freshness()).unwrap_err(),
        SecurityReason::InputTooLarge
    );
    assert_eq!(
        verify_security_evidence_v2(&bytes, &invalid, &trust, &freshness()).unwrap_err(),
        SecurityReason::InputTooLarge
    );
}
