use std::io::{Cursor, Read};
use tapid_attestations::{EvidenceFreshness, SecurityTrust};
use tapid_protocol::SecurityRequest;
use tapid_security_provider::{Disclosure, RetrievalError, retrieve};
use tapid_signatures::{KeyRing, TrustedKey};

// Frozen signed conformance input, shared byte-for-byte with the verifier's v2
// fixture. Kept locally so packaged tests do not depend on a sibling checkout.
const ENVELOPE: &[u8] = include_bytes!("fixtures/advisory-v2.json");

fn request() -> SecurityRequest {
    serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(ENVELOPE).unwrap()["claims"]["request"].clone(),
    )
    .unwrap()
}

fn keys() -> KeyRing {
    let mut keys = KeyRing::new();
    let hex = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    keys.insert(TrustedKey {
        key_id: "fixture".into(),
        algorithm: "ed25519".into(),
        public_key: std::array::from_fn(|i| {
            u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()
        }),
    })
    .unwrap();
    keys
}

#[test]
fn retrieval_requires_exact_disclosure_before_transport_and_preserves_signed_reports() {
    let request = request();
    let keys = keys();
    let trust = SecurityTrust {
        issuer: "fixture.example",
        keys: &keys,
        valid_until: 3000,
    };
    let freshness = EvidenceFreshness {
        now: 1100,
        max_age: 500,
        minimum_sequence: 0,
        previous_digest: None,
    };
    let mut calls = 0;
    let mut transport = |sent: &SecurityRequest| {
        calls += 1;
        assert_eq!(sent, &request);
        Ok(Cursor::new(ENVELOPE))
    };
    assert!(matches!(
        retrieve(
            &request,
            &Disclosure::Denied,
            &trust,
            &freshness,
            &mut transport
        ),
        Err(RetrievalError::NotAuthorized)
    ));
    let mut other = request.clone();
    other.subject.requested_name = "different".into();
    assert!(matches!(
        retrieve(
            &request,
            &Disclosure::Exact(&other),
            &trust,
            &freshness,
            &mut transport
        ),
        Err(RetrievalError::NotAuthorized)
    ));
    let verified = retrieve(
        &request,
        &Disclosure::Exact(&request),
        &trust,
        &freshness,
        &mut transport,
    )
    .unwrap();
    assert_eq!(verified.result().findings[0].id, "REPORT-1");
    assert_eq!(verified.result().findings[0].revision, "2");
    assert_eq!(verified.result().findings[0].fixes[0].version, "1.2.4");
    assert_eq!(calls, 1);
}

#[test]
fn oversized_stream_is_bounded_before_verification() {
    let request = request();
    let keys = keys();
    let trust = SecurityTrust {
        issuer: "fixture.example",
        keys: &keys,
        valid_until: 3000,
    };
    let freshness = EvidenceFreshness {
        now: 1100,
        max_age: 500,
        minimum_sequence: 0,
        previous_digest: None,
    };
    let bytes_read = std::rc::Rc::new(std::cell::Cell::new(0));
    struct Endless(std::rc::Rc<std::cell::Cell<usize>>);
    impl Read for Endless {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            buffer.fill(b' ');
            self.0.set(self.0.get() + buffer.len());
            Ok(buffer.len())
        }
    }
    let result = retrieve(
        &request,
        &Disclosure::Exact(&request),
        &trust,
        &freshness,
        &mut |_| Ok(Endless(bytes_read.clone())),
    );
    assert!(matches!(
        result,
        Err(RetrievalError::Evidence(
            tapid_protocol::SecurityReason::InputTooLarge
        ))
    ));
    assert_eq!(bytes_read.get(), tapid_protocol::MAX_SECURITY_BYTES + 1);
}

#[test]
fn raw_adapter_cannot_bypass_applicability_signatures_freshness_or_replay() {
    use tapid_protocol::SecurityReason;
    let original = request();
    let keys = keys();
    let trust = SecurityTrust {
        issuer: "fixture.example",
        keys: &keys,
        valid_until: 3000,
    };
    let cases = [
        (1100, 0, false, ENVELOPE.to_vec(), None),
        (
            2100,
            0,
            false,
            ENVELOPE.to_vec(),
            Some(SecurityReason::Expired),
        ),
        (
            1100,
            4,
            false,
            ENVELOPE.to_vec(),
            Some(SecurityReason::Replay),
        ),
        (
            1100,
            0,
            true,
            ENVELOPE.to_vec(),
            Some(SecurityReason::SubjectMismatch),
        ),
        (
            1100,
            0,
            false,
            b"not JSON".to_vec(),
            Some(SecurityReason::Malformed),
        ),
    ];
    for (now, minimum_sequence, changed_identity, bytes, expected) in cases {
        let mut request = original.clone();
        if changed_identity {
            request.subject.requested_name = "another-alias".into();
        }
        let freshness = EvidenceFreshness {
            now,
            max_age: 500,
            minimum_sequence,
            previous_digest: None,
        };
        let result = retrieve(
            &request,
            &Disclosure::Exact(&request),
            &trust,
            &freshness,
            &mut |_| Ok(Cursor::new(&bytes)),
        );
        match expected {
            None => assert!(result.is_ok()),
            Some(reason) => assert!(
                matches!(result, Err(RetrievalError::Evidence(actual)) if actual == reason),
                "expected {reason:?}, got {result:?}"
            ),
        }
    }
    let mut tampered: serde_json::Value = serde_json::from_slice(ENVELOPE).unwrap();
    tampered["claims"]["findings"][0]["revision"] = serde_json::json!("forged");
    let freshness = EvidenceFreshness {
        now: 1100,
        max_age: 500,
        minimum_sequence: 0,
        previous_digest: None,
    };
    let result = retrieve(
        &original,
        &Disclosure::Exact(&original),
        &trust,
        &freshness,
        &mut |_| Ok(Cursor::new(serde_json::to_vec(&tampered).unwrap())),
    );
    assert!(matches!(
        result,
        Err(RetrievalError::Evidence(SecurityReason::InvalidSignature))
    ));
}

#[test]
fn invalid_requests_and_audience_changes_do_not_reach_transport() {
    let original = request();
    let keys = keys();
    let trust = SecurityTrust {
        issuer: "fixture.example",
        keys: &keys,
        valid_until: 3000,
    };
    let freshness = EvidenceFreshness {
        now: 1100,
        max_age: 500,
        minimum_sequence: 0,
        previous_digest: None,
    };
    let mut request = original.clone();
    request.audience = tapid_protocol::EvidenceAudience::Tenant {
        tenant: "private-team".into(),
    };
    let mut transport = |_: &SecurityRequest| -> Result<Cursor<Vec<u8>>, RetrievalError> {
        panic!("unauthorized request")
    };
    assert!(matches!(
        retrieve(
            &request,
            &Disclosure::Exact(&original),
            &trust,
            &freshness,
            &mut transport
        ),
        Err(RetrievalError::NotAuthorized)
    ));
    request.subject.placement = "/private/project/path".into();
    assert!(matches!(
        retrieve(
            &request,
            &Disclosure::Exact(&request),
            &trust,
            &freshness,
            &mut transport
        ),
        Err(RetrievalError::Evidence(
            tapid_protocol::SecurityReason::InvalidRequest
        ))
    ));
}

#[test]
fn response_read_failures_never_return_partial_evidence() {
    let request = request();
    let keys = keys();
    let trust = SecurityTrust {
        issuer: "fixture.example",
        keys: &keys,
        valid_until: 3000,
    };
    let freshness = EvidenceFreshness {
        now: 1100,
        max_age: 500,
        minimum_sequence: 0,
        previous_digest: None,
    };
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("private transport detail"))
        }
    }
    assert!(matches!(
        retrieve(
            &request,
            &Disclosure::Exact(&request),
            &trust,
            &freshness,
            &mut |_| Ok(Broken)
        ),
        Err(RetrievalError::Unavailable)
    ));
}
