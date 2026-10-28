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

#[test]
fn evidence_is_withheld_until_replay_state_is_recorded() {
    use tapid_security_provider::retrieve_recorded;
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
    let mut recorded = Vec::new();
    for writable in [false, true] {
        let result = retrieve_recorded(
            &request,
            &Disclosure::Exact(&request),
            &trust,
            &freshness,
            &mut |_| Ok(Cursor::new(ENVELOPE)),
            &mut |issuer, subject, sequence, digest| {
                assert_eq!(issuer, "fixture.example");
                assert_eq!(subject, &request);
                assert_eq!(sequence, 3);
                assert!(digest.starts_with("sha256-"));
                if writable {
                    recorded.push((sequence, digest.to_owned()));
                    Ok(())
                } else {
                    Err(RetrievalError::StateUnavailable)
                }
            },
        );
        if writable {
            let evidence = result.unwrap();
            assert_eq!(recorded[0].1, evidence.envelope_digest());
            assert_eq!(evidence.result().findings[0].id, "REPORT-1");
        } else {
            assert!(matches!(result, Err(RetrievalError::StateUnavailable)));
            assert!(recorded.is_empty());
        }
    }
}

#[test]
fn denied_or_invalid_evidence_never_advances_replay_state() {
    use tapid_security_provider::retrieve_recorded;
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
    let mut requests = 0;
    let mut transport = |_: &SecurityRequest| {
        requests += 1;
        Ok(Cursor::new(b"invalid envelope"))
    };
    let mut record = |_: &str, _: &SecurityRequest, _: u64, _: &str| {
        panic!("unverified evidence must not change replay state")
    };
    assert!(matches!(
        retrieve_recorded(
            &request,
            &Disclosure::Denied,
            &trust,
            &freshness,
            &mut transport,
            &mut record,
        ),
        Err(RetrievalError::NotAuthorized)
    ));
    assert!(matches!(
        retrieve_recorded(
            &request,
            &Disclosure::Exact(&request),
            &trust,
            &freshness,
            &mut transport,
            &mut record,
        ),
        Err(RetrievalError::Evidence(
            tapid_protocol::SecurityReason::Malformed
        ))
    ));
    assert_eq!(requests, 1);
}

#[test]
fn collection_retains_verified_findings_when_another_request_fails() {
    use tapid_security_provider::{CollectionInput, collect_recorded};
    let first = request();
    let mut second = first.clone();
    second.subject.requested_name = "another-alias".into();
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
    let inputs = [&first, &second].map(|request| CollectionInput {
        request,
        disclosure: Disclosure::Exact(request),
        freshness: &freshness,
    });
    let mut recorded = 0;
    let collection = collect_recorded(
        &inputs,
        &trust,
        &mut |sent| {
            if sent == &first {
                Ok(Cursor::new(ENVELOPE))
            } else {
                Err(RetrievalError::Unavailable)
            }
        },
        &mut |_, _, _, _| {
            recorded += 1;
            Ok(())
        },
    );
    assert_eq!(collection.results.len(), 2);
    assert_eq!(
        collection.results[0].as_ref().unwrap().result().findings[0].id,
        "REPORT-1"
    );
    assert!(matches!(
        collection.results[1],
        Err(RetrievalError::Unavailable)
    ));
    assert_eq!(collection.not_checked, 0);
    assert!(!collection.complete());
    assert_eq!(recorded, 1);
}

#[test]
fn collection_caps_requests_and_reports_unchecked_tail() {
    use tapid_security_provider::{CollectionInput, collect_recorded};
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
    let inputs = (0..1002)
        .map(|_| CollectionInput {
            request: &request,
            disclosure: Disclosure::Exact(&request),
            freshness: &freshness,
        })
        .collect::<Vec<_>>();
    let mut calls = 0;
    let collection = collect_recorded(
        &inputs,
        &trust,
        &mut |_| {
            calls += 1;
            Err::<Cursor<&[u8]>, _>(RetrievalError::Unavailable)
        },
        &mut |_, _, _, _| panic!("failed requests must not persist state"),
    );
    assert_eq!(calls, 1000);
    assert_eq!(collection.results.len(), 1000);
    assert_eq!(collection.not_checked, 2);
    assert!(!collection.complete());
}

#[test]
fn collection_bounds_aggregate_decompressed_reads_including_rejected_responses() {
    use tapid_security_provider::{CollectionInput, collect_recorded};
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
    let inputs = (0..140)
        .map(|_| CollectionInput {
            request: &request,
            disclosure: Disclosure::Exact(&request),
            freshness: &freshness,
        })
        .collect::<Vec<_>>();
    let read = std::rc::Rc::new(std::cell::Cell::new(0));
    struct Endless(std::rc::Rc<std::cell::Cell<usize>>);
    impl Read for Endless {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            buffer.fill(b' ');
            self.0.set(self.0.get() + buffer.len());
            Ok(buffer.len())
        }
    }
    let collection = collect_recorded(
        &inputs,
        &trust,
        &mut |_| Ok(Endless(read.clone())),
        &mut |_, _, _, _| panic!("invalid bodies cannot advance replay state"),
    );
    assert_eq!(read.get(), 32 * 1024 * 1024);
    assert_eq!(collection.results.len(), 128);
    assert_eq!(collection.not_checked, 12);
    assert_eq!(
        collection.limit,
        Some(tapid_security_provider::CollectionLimit::ResponseBytes)
    );
    assert!(!collection.complete());
}

#[test]
fn collection_explains_its_limit_and_never_calls_empty_inventory_complete() {
    use tapid_security_provider::{CollectionInput, CollectionLimit, collect_recorded};
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
    let empty = collect_recorded::<Cursor<&[u8]>>(
        &[],
        &trust,
        &mut |_| panic!("empty collection cannot make requests"),
        &mut |_, _, _, _| panic!("empty collection cannot record evidence"),
    );
    assert!(!empty.complete());
    assert_eq!(empty.limit, None);
    let inputs = (0..1001)
        .map(|_| CollectionInput {
            request: &request,
            disclosure: Disclosure::Denied,
            freshness: &freshness,
        })
        .collect::<Vec<_>>();
    let denied = collect_recorded::<Cursor<&[u8]>>(
        &inputs,
        &trust,
        &mut |_| panic!("denied collection cannot make requests"),
        &mut |_, _, _, _| panic!("denied collection cannot record evidence"),
    );
    assert_eq!(denied.limit, Some(CollectionLimit::Requests));
    assert_eq!(denied.not_checked, 1);
    assert!(
        denied
            .results
            .iter()
            .all(|result| matches!(result, Err(RetrievalError::NotAuthorized)))
    );
}

#[test]
fn collection_completeness_requires_current_full_signed_coverage_and_recording() {
    use tapid_security_provider::{CollectionInput, collect_recorded};
    use tapid_signatures::TrustEnvelope;
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
    for (status, limitations, writable, expected) in [
        ("findings", vec![], true, true),
        ("no_known_findings", vec![], true, true),
        ("partial", vec!["malware source unavailable"], true, false),
        ("stale", vec!["refresh unavailable"], true, false),
        ("invalid_evidence", vec!["report withdrawn"], true, false),
        ("findings", vec!["limited provider coverage"], true, false),
        ("findings", vec![], false, false),
    ] {
        let mut envelope: serde_json::Value = serde_json::from_slice(ENVELOPE).unwrap();
        let claims = &mut envelope["claims"];
        claims["status"] = serde_json::json!(status);
        claims["limitations"] = serde_json::json!(limitations);
        if status == "no_known_findings" {
            claims["findings"] = serde_json::json!([]);
        }
        if status == "invalid_evidence" {
            claims["coverage"] = serde_json::json!([]);
            claims["findings"][0]["state"] = serde_json::json!("withdrawn");
            claims["findings"][0]["revision"] = serde_json::json!("3");
        }
        let seed = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
        let seed =
            std::array::from_fn(|i| u8::from_str_radix(&seed[2 * i..2 * i + 2], 16).unwrap());
        let bytes = serde_json::to_vec(
            &TrustEnvelope::unsigned("tapid-security-v2", "version-scope", claims.clone())
                .sign("fixture", &seed)
                .unwrap(),
        )
        .unwrap();
        let inputs = [CollectionInput {
            request: &request,
            disclosure: Disclosure::Exact(&request),
            freshness: &freshness,
        }];
        let collection = collect_recorded(
            &inputs,
            &trust,
            &mut |_| Ok(Cursor::new(&bytes)),
            &mut |_, _, _, _| {
                if writable {
                    Ok(())
                } else {
                    Err(RetrievalError::StateUnavailable)
                }
            },
        );
        assert_eq!(
            collection.complete(),
            expected,
            "status={status}, writable={writable}"
        );
        if writable {
            assert!(collection.results[0].is_ok(), "{:?}", collection.results[0]);
            if status == "invalid_evidence" {
                let evidence = collection.results[0].as_ref().unwrap();
                assert_eq!(evidence.result().findings[0].id, "REPORT-1");
                assert_eq!(evidence.result().findings[0].revision, "3");
                assert_eq!(
                    evidence.result().findings[0].state,
                    tapid_protocol::ReportState::Withdrawn
                );
            }
        } else {
            assert!(matches!(
                collection.results[0],
                Err(RetrievalError::StateUnavailable)
            ));
        }
    }
}

#[cfg(unix)]
#[test]
fn retrieval_commits_a_durable_floor_before_returning_evidence() {
    use tapid_security_provider::{ReplayStore, retrieve_recorded};
    let home = tapid_test_support::TempHome::new("recorded-security").unwrap();
    let root = home.path().join("floors");
    let store = ReplayStore::open(&root).unwrap();
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
    let evidence = retrieve_recorded(
        &request,
        &Disclosure::Exact(&request),
        &trust,
        &freshness,
        &mut |_| Ok(Cursor::new(ENVELOPE)),
        &mut |issuer, request, sequence, digest| store.record(issuer, request, sequence, digest),
    )
    .unwrap();
    assert_eq!(
        ReplayStore::open(&root)
            .unwrap()
            .load(trust.issuer, &request)
            .unwrap(),
        Some((
            evidence.result().sequence,
            evidence.envelope_digest().to_owned()
        ))
    );
    // A concurrent advance after the verifier's freshness snapshot must still
    // withhold the older signed response at the recording gate.
    store
        .record(
            trust.issuer,
            &request,
            evidence.result().sequence + 1,
            evidence.envelope_digest(),
        )
        .unwrap();
    assert!(matches!(
        retrieve_recorded(
            &request,
            &Disclosure::Exact(&request),
            &trust,
            &freshness,
            &mut |_| Ok(Cursor::new(ENVELOPE)),
            &mut |issuer, request, sequence, digest| store
                .record(issuer, request, sequence, digest)
        ),
        Err(RetrievalError::StateUnavailable)
    ));
}
