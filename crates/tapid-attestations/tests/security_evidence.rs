#[path = "support/security_v1_consumer.rs"]
mod previous_consumer;

use serde_json::{Value, json};
use tapid_attestations::{EvidenceFreshness, SecurityTrust, verify_security_evidence};
use tapid_protocol::{
    MAX_SECURITY_BYTES, SecurityReason, SecurityRequest, SecurityResult, SecurityStatus,
};
use tapid_signatures::{KeyRing, TrustEnvelope, TrustedKey};

const REQUEST: &[u8] = include_bytes!("fixtures/security-request-v1.json");
// RFC 8032 test vector 1. Public fixture material, never a production trust root.
fn hex<const N: usize>(s: &str) -> [u8; N] {
    std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}
fn keys() -> KeyRing {
    let mut ring = KeyRing::new();
    ring.insert(TrustedKey {
        key_id: "fixture-1".into(),
        algorithm: "ed25519".into(),
        public_key: hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
    })
    .unwrap();
    ring
}
fn request() -> SecurityRequest {
    serde_json::from_slice(REQUEST).unwrap()
}
fn result() -> SecurityResult {
    serde_json::from_value(json!({
        "version": "tapid-security-v1", "request": request(), "issuer": "fixture.example",
        "sequence": 3, "status": "no_known_findings", "scanner": "fixture-static",
        "scannerVersion": "1", "ruleVersion": "2026-10-18", "scope": "artifact_bytes",
        "coverage": ["archive-static", "install-scripts"], "sourceRevision": "rules:abc123",
        "checkedAt": 1000, "issuedAt": 1010, "expiresAt": 2000,
        "findings": [], "limitations": ["no runtime execution"]
    }))
    .unwrap()
}
fn envelope(value: &SecurityResult) -> TrustEnvelope {
    let digest = match &value.request.check {
        tapid_protocol::SecurityCheck::Artifact { digest } => digest.as_str(),
        _ => "version-scope",
    };
    TrustEnvelope::unsigned("tapid-security-v1", digest, json!(value))
}
fn sign(envelope: TrustEnvelope) -> Vec<u8> {
    serde_json::to_vec(
        &envelope
            .sign(
                "fixture-1",
                &hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"),
            )
            .unwrap(),
    )
    .unwrap()
}
fn check(bytes: &[u8]) -> Result<SecurityStatus, SecurityReason> {
    let ring = keys();
    verify_security_evidence(
        bytes,
        &request(),
        &SecurityTrust {
            issuer: "fixture.example",
            keys: &ring,
            valid_until: 3000,
        },
        &freshness(),
    )
    .map(|v| v.result().status)
}
fn freshness() -> EvidenceFreshness<'static> {
    EvidenceFreshness {
        now: 1100,
        max_age: 500,
        minimum_sequence: 0,
        previous_digest: None,
    }
}

#[test]
fn malformed_response_is_invalid_evidence() {
    assert_eq!(check(b"{}"), Err(SecurityReason::Malformed));
}
#[test]
fn authentic_applicable_result_preserves_alias_origin_and_context() {
    let bytes = sign(envelope(&result()));
    assert_eq!(check(&bytes), Ok(SecurityStatus::NoKnownFindings));
}

#[test]
fn advisory_cannot_claim_byte_analysis() {
    let mut value = result();
    value.request.check = tapid_protocol::SecurityCheck::Advisory;
    assert_eq!(value.validate(), Err(SecurityReason::InvalidResult));
}

#[test]
fn origin_alias_graph_and_audience_are_exact() {
    let original = result();
    for edit in [
        "origin",
        "name",
        "version",
        "alias",
        "placement",
        "peer",
        "platform",
        "tenant",
    ] {
        let mut value = original.clone();
        match edit {
            "origin" => {
                value.request.subject.package.registry = "https://registry.npmjs.org".into()
            }
            "name" => value.request.subject.package.name = "other".into(),
            "version" => value.request.subject.package.version = "1.2.4".into(),
            "alias" => value.request.subject.requested_name = "other".into(),
            "placement" => value.request.subject.placement = "nested/node_modules/alias".into(),
            "peer" => {
                value
                    .request
                    .subject
                    .peers
                    .get_mut("peer")
                    .unwrap()
                    .registry = "https://registry.npmjs.org".into()
            }
            "platform" => value.request.subject.platform.os = Some("darwin".into()),
            "tenant" => {
                value.request.audience = tapid_protocol::EvidenceAudience::Tenant {
                    tenant: "private".into(),
                }
            }
            _ => unreachable!(),
        }
        assert_eq!(
            check(&sign(envelope(&value))),
            Err(SecurityReason::SubjectMismatch),
            "{edit}"
        );
    }
}

#[test]
fn digest_and_signature_bind_the_actual_payload() {
    let mut changed = envelope(&result());
    changed.artifact_digest =
        "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
    assert_eq!(check(&sign(changed)), Err(SecurityReason::DigestMismatch));
    let bytes = sign(envelope(&result()));
    let mut changed: Value = serde_json::from_slice(&bytes).unwrap();
    changed["claims"]["scannerVersion"] = json!("tampered");
    assert_eq!(
        check(&serde_json::to_vec(&changed).unwrap()),
        Err(SecurityReason::InvalidSignature)
    );
    assert_eq!(
        check(&serde_json::to_vec(&envelope(&result())).unwrap()),
        Err(SecurityReason::Malformed)
    );
}

#[test]
fn unknown_version_issuer_and_revoked_keys_fail_closed() {
    let mut value = result();
    value.version = "tapid-security-v2".into();
    assert_eq!(
        check(&sign(envelope(&value))),
        Err(SecurityReason::UnsupportedVersion)
    );
    value = result();
    value.issuer = "unknown.example".into();
    assert_eq!(
        check(&sign(envelope(&value))),
        Err(SecurityReason::UnknownIssuer)
    );
    let bytes = sign(envelope(&result()));
    let empty = KeyRing::new();
    assert_eq!(
        verify_security_evidence(
            &bytes,
            &request(),
            &SecurityTrust {
                issuer: "fixture.example",
                keys: &empty,
                valid_until: 3000,
            },
            &freshness()
        )
        .unwrap_err(),
        SecurityReason::UntrustedKey
    );
}

#[test]
fn freshness_and_replay_require_trusted_clock_and_persisted_state() {
    let bytes = sign(envelope(&result()));
    let ring = keys();
    let trust = SecurityTrust {
        issuer: "fixture.example",
        keys: &ring,
        valid_until: 3000,
    };
    let accepted = verify_security_evidence(&bytes, &request(), &trust, &freshness()).unwrap();
    let mut fresh = freshness();
    fresh.minimum_sequence = 3;
    assert_eq!(
        verify_security_evidence(&bytes, &request(), &trust, &fresh).unwrap_err(),
        SecurityReason::Replay
    );
    fresh.previous_digest = Some(accepted.envelope_digest());
    assert!(verify_security_evidence(&bytes, &request(), &trust, &fresh).is_ok());
    let mut equivocation = result();
    equivocation
        .limitations
        .push("changed at same sequence".into());
    assert_eq!(
        verify_security_evidence(&sign(envelope(&equivocation)), &request(), &trust, &fresh)
            .unwrap_err(),
        SecurityReason::Replay
    );
    fresh.minimum_sequence = 4;
    assert_eq!(
        verify_security_evidence(&bytes, &request(), &trust, &fresh).unwrap_err(),
        SecurityReason::Replay
    );
    fresh = freshness();
    fresh.now = 1005;
    assert_eq!(
        verify_security_evidence(&bytes, &request(), &trust, &fresh).unwrap_err(),
        SecurityReason::FutureEvidence
    );
    fresh.now = 2000;
    assert_eq!(
        verify_security_evidence(&bytes, &request(), &trust, &fresh).unwrap_err(),
        SecurityReason::Expired
    );
    fresh.now = 1600;
    assert_eq!(
        verify_security_evidence(&bytes, &request(), &trust, &fresh).unwrap_err(),
        SecurityReason::Expired
    );
    fresh = freshness();
    let expired_trust = SecurityTrust {
        valid_until: fresh.now,
        ..trust
    };
    assert_eq!(
        verify_security_evidence(&bytes, &request(), &expired_trust, &fresh).unwrap_err(),
        SecurityReason::Expired
    );
}

#[test]
fn missing_coverage_never_means_no_known_findings() {
    let mut value = result();
    value.coverage.pop();
    assert_eq!(
        check(&sign(envelope(&value))),
        Err(SecurityReason::IncompleteCoverage)
    );
    value.status = SecurityStatus::Partial;
    assert_eq!(check(&sign(envelope(&value))), Ok(SecurityStatus::Partial));
}

#[test]
fn bounded_and_strict_parsing_rejects_ambiguous_input() {
    assert_eq!(
        check(&vec![b' '; MAX_SECURITY_BYTES + 1]),
        Err(SecurityReason::InputTooLarge)
    );
    for bytes in [
        b"[]".as_slice(),
        b"null",
        b"{\"version\":1}",
        b"{\"version\":\"a\",\"version\":\"b\"}",
    ] {
        assert_eq!(check(bytes), Err(SecurityReason::Malformed));
    }
    let mut value: Value = serde_json::from_slice(&sign(envelope(&result()))).unwrap();
    value["claims"]["unknown"] = json!(true);
    assert_eq!(
        check(&serde_json::to_vec(&value).unwrap()),
        Err(SecurityReason::Malformed)
    );
    let mut result = result();
    result.findings = (0..257)
        .map(|_| tapid_protocol::SecurityFinding {
            id: "F".into(),
            summary: "finding".into(),
            severity: "high".into(),
        })
        .collect();
    assert_eq!(
        check(&sign(envelope(&result))),
        Err(SecurityReason::InvalidResult)
    );
}

#[test]
fn frozen_v1_fixtures_are_usable_without_cloud_or_accounts() {
    let artifact = include_bytes!("fixtures/artifact-v1.json");
    assert_eq!(check(artifact), Ok(SecurityStatus::NoKnownFindings));
    let advisory = include_bytes!("fixtures/advisory-v1.json");
    let mut requested = request();
    requested.check = tapid_protocol::SecurityCheck::Advisory;
    let ring = keys();
    let accepted = verify_security_evidence(
        advisory,
        &requested,
        &SecurityTrust {
            issuer: "fixture.example",
            keys: &ring,
            valid_until: 3000,
        },
        &freshness(),
    )
    .unwrap();
    assert_eq!(
        accepted.result().scope,
        tapid_protocol::SecurityScope::PackageVersion
    );
    for bytes in [artifact.as_slice(), advisory.as_slice()] {
        let previous = previous_consumer::verify(bytes, &ring);
        let original: Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(json!(previous), original);
        let current: tapid_signatures::TrustEnvelope = serde_json::from_slice(bytes).unwrap();
        let current_result: SecurityResult = serde_json::from_value(current.claims).unwrap();
        assert_eq!(json!(current_result), original["claims"]);
    }
    let emitted = sign(envelope(&result()));
    previous_consumer::verify(&emitted, &ring);
}

#[test]
fn duplicate_peer_keys_and_unknown_identity_fields_are_malformed() {
    let fixture = std::str::from_utf8(include_bytes!("fixtures/artifact-v1.json")).unwrap();
    let duplicate = fixture.replace("\"peers\": {", "\"peers\": {\"peer\": {\"registry\":\"https://registry.example\",\"name\":\"peer\",\"version\":\"9.0.0\"},");
    assert_eq!(check(duplicate.as_bytes()), Err(SecurityReason::Malformed));
    let unknown = fixture.replace(
        "\"name\": \"actual\"",
        "\"unknown\": true, \"name\": \"actual\"",
    );
    assert_eq!(check(unknown.as_bytes()), Err(SecurityReason::Malformed));
}

#[test]
fn unsafe_json_integers_and_unscanned_findings_are_invalid() {
    let mut value = result();
    value.sequence = 9_007_199_254_740_992;
    assert_eq!(value.validate(), Err(SecurityReason::InvalidResult));
    value = result();
    value.status = SecurityStatus::NotScanned;
    value.findings.push(tapid_protocol::SecurityFinding {
        id: "F1".into(),
        summary: "cannot be observed without scanning".into(),
        severity: "high".into(),
    });
    assert_eq!(value.validate(), Err(SecurityReason::InvalidResult));
}

#[test]
fn all_scan_states_stay_distinct_and_require_limitations() {
    for status in [
        SecurityStatus::NotScanned,
        SecurityStatus::Unavailable,
        SecurityStatus::Stale,
        SecurityStatus::Partial,
        SecurityStatus::Unsupported,
        SecurityStatus::InvalidEvidence,
    ] {
        let mut value = result();
        value.status = status;
        value.coverage.clear();
        assert_eq!(check(&sign(envelope(&value))), Ok(status));
        value.limitations.clear();
        assert_eq!(value.validate(), Err(SecurityReason::InvalidResult));
    }
    let mut value = result();
    value.status = SecurityStatus::Findings;
    assert_eq!(value.validate(), Err(SecurityReason::InvalidResult));
    value.findings.push(tapid_protocol::SecurityFinding {
        id: "ADVISORY-1".into(),
        summary: "example issue".into(),
        severity: "high".into(),
    });
    assert_eq!(check(&sign(envelope(&value))), Ok(SecurityStatus::Findings));
}

#[test]
fn invalid_origins_digests_and_contexts_fail_request_validation() {
    for origin in [
        "https://registry.example/",
        "http://registry.example",
        "https://user:pass@registry.example",
        "file:///archive",
    ] {
        let mut request = request();
        request.subject.package.registry = origin.into();
        assert_eq!(request.validate(), Err(SecurityReason::InvalidRequest));
    }
    let mut request = request();
    request.coverage.push(request.coverage[0].clone());
    assert_eq!(request.validate(), Err(SecurityReason::InvalidRequest));
    request = self::request();
    request.subject.platform.os = Some("bad|context".into());
    assert_eq!(request.validate(), Err(SecurityReason::InvalidRequest));
    request = self::request();
    request.check = tapid_protocol::SecurityCheck::Artifact {
        digest: "sha256-no".into(),
    };
    assert_eq!(request.validate(), Err(SecurityReason::InvalidRequest));
}

#[test]
fn hosted_claims_and_publication_use_the_same_closed_v1_contract() {
    use tapid_protocol::{AccountAuthorization, EvidencePublicationRequest, SecurityErrorResponse};
    let claims = json!({"version":"tapid-security-v1", "authority":"https://accounts.example",
        "subject":"user-1", "tenant":"org-1", "audience":"https://cloud.example",
        "scopes":["read_private", "submit_analysis"], "issuedAt":1000, "expiresAt":2000,
        "credentialId":"credential-1"});
    let parsed: AccountAuthorization = serde_json::from_value(claims.clone()).unwrap();
    let previous: previous_consumer::AccountAuthorization =
        serde_json::from_value(json!(parsed)).unwrap();
    assert_eq!(json!(previous), claims);
    let publication = json!({"version":"tapid-security-v1", "tenant":"org-1",
        "evidenceDigest":"sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "expectedSequence":3});
    let parsed: EvidencePublicationRequest = serde_json::from_value(publication.clone()).unwrap();
    let previous: previous_consumer::Publication = serde_json::from_value(json!(parsed)).unwrap();
    assert_eq!(json!(previous), publication);
    let error = json!({"version":"tapid-security-v1", "code":"credential_revoked", "retryAfterSeconds":null});
    let parsed: SecurityErrorResponse = serde_json::from_value(error.clone()).unwrap();
    let previous: previous_consumer::ErrorResponse = serde_json::from_value(json!(parsed)).unwrap();
    assert_eq!(json!(previous), error);
    let mut invalid = claims;
    invalid["scopes"] = json!(["administrator"]);
    assert!(serde_json::from_value::<AccountAuthorization>(invalid).is_err());
}

#[test]
fn placements_are_portable_graph_identifiers() {
    for placement in [
        "/home/alice/app/node_modules/alias",
        "C:/Users/alice/app/node_modules/alias",
        "C:\\Users\\alice\\app\\node_modules\\alias",
        "C:app/node_modules/alias",
        "\\\\server\\share\\node_modules\\alias",
        "//server/share/node_modules/alias",
        "\\rooted\\node_modules\\alias",
        "root\\node_modules\\alias",
        "root/../node_modules/alias",
        "root/./node_modules/alias",
        "root//node_modules/alias",
        "root/node_modules/alias/",
    ] {
        let mut request = request();
        request.subject.placement = placement.into();
        assert_eq!(
            request.validate(),
            Err(SecurityReason::InvalidRequest),
            "{placement}"
        );
    }
    for placement in [
        "root",
        "root/node_modules/alias",
        "packages/member/node_modules/@scope/alias",
    ] {
        let mut request = request();
        request.subject.placement = placement.into();
        assert_eq!(request.validate(), Ok(()), "{placement}");
    }
}

#[test]
fn every_present_platform_value_obeys_the_wire_byte_limit() {
    for field in ["os", "cpu", "libc"] {
        for (value, expected) in [
            ("a".repeat(4096), Ok(())),
            ("a".repeat(4097), Err(SecurityReason::InvalidRequest)),
            ("é".repeat(2048), Ok(())),
            ("é".repeat(2049), Err(SecurityReason::InvalidRequest)),
        ] {
            let mut request = request();
            match field {
                "os" => request.subject.platform.os = Some(value),
                "cpu" => request.subject.platform.cpu = Some(value),
                "libc" => request.subject.platform.libc = Some(value),
                _ => unreachable!(),
            }
            assert_eq!(request.validate(), expected, "{field}");
        }
    }
    let mut request = request();
    request.subject.platform.os = None;
    request.subject.platform.cpu = None;
    request.subject.platform.libc = None;
    assert_eq!(request.validate(), Ok(()));
}
