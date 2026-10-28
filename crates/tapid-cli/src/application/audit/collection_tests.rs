use super::*;
use std::io::Cursor;
use tapid_signatures::{KeyRing, TrustEnvelope, TrustedKey};

#[test]
fn signed_collection_retains_exact_keys_and_preserves_project_and_store() {
    let project = tapid_test_support::TempProject::new("audit-signed").unwrap();
    let home = tapid_test_support::TempHome::new("audit-signed").unwrap();
    let manifest = br#"{"name":"audit","version":"1.0.0","dependencies":{"actual":"1.2.3"}}"#;
    project.write("package.json", manifest).unwrap();
    let mut lock = Lockfile::new(&digest_bytes(manifest)).unwrap();
    let mut package = tapid_lockfile::LockedPackage::new_with_provenance(
        "https://registry.example",
        "actual",
        "1.2.3",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let nested = tapid_lockfile::LockedPackage::new_with_provenance(
        "https://private.example",
        "actual",
        "1.2.3",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let nested_key = nested.key();
    package
        .add_alias_dependency("renamed", &nested_key)
        .unwrap();
    lock.insert_package(nested).unwrap();
    let key = package.key();
    lock.insert_package(package).unwrap();
    lock.set_roots([key.clone()]).unwrap();
    let bytes = lock.to_json().unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    project
        .write("node_modules/sentinel", b"installed")
        .unwrap();
    project
        .write(".tapid-lifecycle-journal.json", b"pending")
        .unwrap();
    home.write("tapid/store/sentinel", b"store").unwrap();
    let mut keys = KeyRing::new();
    let public = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    keys.insert(TrustedKey {
        key_id: "fixture".into(),
        algorithm: "ed25519".into(),
        public_key: std::array::from_fn(|i| {
            u8::from_str_radix(&public[i * 2..i * 2 + 2], 16).unwrap()
        }),
    })
    .unwrap();
    let trust = tapid_attestations::SecurityTrust {
        issuer: "fixture.example",
        keys: &keys,
        valid_until: 3000,
    };
    let freshness = tapid_attestations::EvidenceFreshness {
        now: 1100,
        max_age: 500,
        minimum_sequence: 0,
        previous_digest: None,
    };
    let mut recorded = 0;
    let evidence_status = std::cell::Cell::new("findings");
    let fail_private = std::cell::Cell::new(false);
    let forbid_transport = std::cell::Cell::new(false);
    let mut transport = |request: &tapid_protocol::SecurityRequest| {
        assert!(
            !forbid_transport.get(),
            "unreadable replay state must prevent transport"
        );
        if fail_private.get() && request.subject.package.registry == "https://private.example" {
            return Err(tapid_security_provider::RetrievalError::Unavailable);
        }
        let mut claims = serde_json::json!({
            "version":"tapid-security-v2", "request":request, "issuer":"fixture.example",
            "scope":"package_version", "status":"findings", "coverage":["advisory","reported_malware"],
            "limitations":[], "checkedAt":1000, "issuedAt":1010, "expiresAt":2000,
            "sequence":3, "scanner":"fixture", "scannerVersion":"1", "ruleVersion":"1", "sourceRevision":"reports:3",
            "findings":[{"id":"REPORT-1","revision":"2","classification":"advisory","state":"active","summary":"Example advisory","severity":"high","fixes":[{"version":"1.2.4"}]}]
        });
        claims["status"] = serde_json::json!(evidence_status.get());
        match evidence_status.get() {
            "no_known_findings" => claims["findings"] = serde_json::json!([]),
            "stale" => claims["limitations"] = serde_json::json!(["refresh unavailable"]),
            "invalid_evidence" => {
                claims["limitations"] = serde_json::json!(["withdrawn"]);
                claims["coverage"] = serde_json::json!([]);
                claims["findings"][0]["state"] = serde_json::json!("withdrawn");
            }
            _ => {}
        }
        let seed = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
        let seed =
            std::array::from_fn(|i| u8::from_str_radix(&seed[i * 2..i * 2 + 2], 16).unwrap());
        Ok(Box::new(Cursor::new(
            serde_json::to_vec(
                &TrustEnvelope::unsigned("tapid-security-v2", "version-scope", claims)
                    .sign("fixture", &seed)
                    .unwrap(),
            )
            .unwrap(),
        )) as Box<dyn std::io::Read>)
    };
    let mut record = |_: &str, _: &tapid_protocol::SecurityRequest, sequence, _: &str| {
        assert_eq!(sequence, 3);
        recorded += 1;
        Ok(())
    };
    let mut deny = |_: &tapid_protocol::SecurityRequest| false;
    let mut unavailable = |_: &tapid_protocol::SecurityRequest| -> Result<
        Box<dyn std::io::Read>,
        tapid_security_provider::RetrievalError,
    > { Err(tapid_security_provider::RetrievalError::Unavailable) };
    let mut malformed = |_: &tapid_protocol::SecurityRequest| -> Result<
        Box<dyn std::io::Read>,
        tapid_security_provider::RetrievalError,
    > { Ok(Box::new(Cursor::new(b"invalid"))) };
    let mut reject_record = |_: &str, _: &tapid_protocol::SecurityRequest, _: u64, _: &str| {
        Err(tapid_security_provider::RetrievalError::StateUnavailable)
    };
    #[cfg(unix)]
    let store = tapid_security_provider::ReplayStore::open(&home.path().join("floors")).unwrap();
    let mut provider = Provider {
        trust,
        freshness: &mut |_| tapid_attestations::EvidenceFreshness {
            now: freshness.now,
            max_age: freshness.max_age,
            minimum_sequence: 0,
            previous_digest: None,
        },
        authorize: &mut |_| true,
        transport: &mut transport,
        record: &mut record,
        #[cfg(unix)]
        replay: None,
        threshold: tapid_policy::AuditThreshold::Any,
    };
    let report = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
    assert_eq!(report.outcome, tapid_policy::AuditOutcome::Findings);
    assert_eq!(report.evidence.len(), 2);
    assert!(report.evidence.iter().any(|e| e.key == key));
    assert!(report.evidence.iter().any(|e| e.key == nested_key
        && e.result.request.subject.requested_name == "renamed"
        && e.result.request.subject.package.registry == "https://private.example"));
    assert_eq!(report.evidence[0].result.findings[0].id, "REPORT-1");
    assert_eq!(
        report.evidence[0].result.findings[0].fixes[0].version,
        "1.2.4"
    );
    let nested_evidence = report
        .evidence
        .iter()
        .find(|e| e.key == nested_key)
        .unwrap();
    assert!(!nested_evidence.paths_truncated);
    assert_eq!(nested_evidence.paths[0].steps.len(), 2);
    assert_eq!(
        nested_evidence.paths[0].steps[1].via.as_deref(),
        Some("renamed")
    );
    assert!(report.failures.is_empty());
    let (value, status) = crate::output::json::audit_value(project.path(), Ok(report));
    assert_eq!(status, 1);
    assert_eq!(
        value["data"]["evidence"][0]["result"]["issuer"],
        "fixture.example"
    );
    assert_eq!(value["data"]["evidence"][0]["result"]["checkedAt"], 1000);
    assert_eq!(
        value["data"]["findings"][0]["report"]["classification"],
        "advisory"
    );
    assert_eq!(value["data"]["coverage"]["complete"], true);
    #[cfg(unix)]
    {
        provider.replay = Some(&store);
        let checked = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
        assert_eq!(checked.evidence.len(), 2);
        let request = &checked.evidence[0].result.request;
        let floor = store.load("fixture.example", request).unwrap().unwrap();
        assert_eq!(floor.0, 3);
        store
            .record("fixture.example", request, 4, &floor.1)
            .unwrap();
        let checked = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
        assert_eq!(checked.evidence.len(), 1);
        assert_eq!(checked.failures.len(), 1);
        assert_eq!(checked.outcome, tapid_policy::AuditOutcome::Incomplete);
        // A corrupt floor must fail before disclosing this occurrence.
        for entry in std::fs::read_dir(home.path().join("floors")).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "floor")
            {
                std::fs::write(path, b"corrupt").unwrap();
            }
        }
        forbid_transport.set(true);
        let checked = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
        assert!(checked.evidence.is_empty());
        assert_eq!(checked.failures.len(), 2);
        assert!(
            checked
                .failures
                .iter()
                .all(|failure| failure.code == "state_unavailable")
        );
        provider.replay = None;
        forbid_transport.set(false);
    }
    // The same signed provider and policy path handles imported placements.
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{
        "":serde_json::from_slice::<serde_json::Value>(manifest).unwrap(),
        "node_modules/actual":{
            "version":"1.2.3", "resolved":"https://registry.example/actual/-/actual-1.2.3.tgz",
            "integrity":format!("sha512-{}==", "A".repeat(86)),
            "dependencies":{"renamed":"npm:actual@1.2.3"}
        },
        "node_modules/renamed":{
            "name":"actual", "version":"1.2.3", "resolved":"https://private.example/actual/-/actual-1.2.3.tgz",
            "integrity":format!("sha512-{}==", "A".repeat(86))
        }
    }});
    let imported = ImportedNpmLockfile::import(
        &npm.to_string(),
        std::str::from_utf8(manifest).unwrap(),
        &digest_bytes(manifest),
    )
    .unwrap()
    .to_json()
    .unwrap();
    project.write("tapid.lock", imported.as_bytes()).unwrap();
    let imported_report = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
    assert_eq!(
        imported_report.outcome,
        tapid_policy::AuditOutcome::Findings
    );
    assert_eq!(imported_report.requestable_occurrences, Some(2));
    let renamed = imported_report
        .evidence
        .iter()
        .find(|entry| entry.key == "node_modules/renamed")
        .unwrap();
    assert_eq!(
        renamed.result.request.subject.package.registry,
        "https://private.example"
    );
    assert_eq!(renamed.result.request.subject.requested_name, "renamed");
    assert_eq!(renamed.paths[0].steps.len(), 2);
    assert_eq!(renamed.paths[0].steps[1].via.as_deref(), Some("renamed"));
    assert_eq!(
        std::fs::read(project.path().join("tapid.lock")).unwrap(),
        imported.as_bytes()
    );
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    for (status, expected) in [
        (
            "no_known_findings",
            tapid_policy::AuditOutcome::BelowThreshold,
        ),
        ("stale", tapid_policy::AuditOutcome::Incomplete),
        ("invalid_evidence", tapid_policy::AuditOutcome::Incomplete),
    ] {
        evidence_status.set(status);
        let checked = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
        assert_eq!(checked.outcome, expected, "{status}");
        assert_eq!(
            checked.evidence[0].result.status,
            match status {
                "no_known_findings" => tapid_protocol::SecurityStatus::NoKnownFindings,
                "stale" => tapid_protocol::SecurityStatus::Stale,
                _ => tapid_protocol::SecurityStatus::InvalidEvidence,
            }
        );
    }
    evidence_status.set("findings");
    fail_private.set(true);
    let partial = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
    assert_eq!(partial.outcome, tapid_policy::AuditOutcome::Incomplete);
    assert_eq!(partial.evidence.len(), 1);
    assert_eq!(partial.failures.len(), 1);
    assert_eq!(partial.failures[0].key, nested_key);
    fail_private.set(false);
    provider.threshold = tapid_policy::AuditThreshold::Critical;
    assert_eq!(
        inspect_with_provider(project.path(), Some(&mut provider))
            .unwrap()
            .outcome,
        tapid_policy::AuditOutcome::BelowThreshold
    );
    provider.record = &mut reject_record;
    let failed = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
    assert_eq!(failed.outcome, tapid_policy::AuditOutcome::Incomplete);
    assert!(failed.evidence.is_empty());
    assert_eq!(failed.failures[0].code, "state_unavailable");
    provider.transport = &mut malformed;
    let failed = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
    assert!(failed.evidence.is_empty());
    assert_eq!(failed.failures[0].code, "invalid_evidence");
    provider.transport = &mut unavailable;
    let failed = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
    assert_eq!(failed.failures[0].code, "unavailable");
    let (failed_json, failed_status) = crate::output::json::audit_value(project.path(), Ok(failed));
    assert_eq!(failed_status, 3);
    assert!(failed_json["data"]["live_requests"].is_null());
    provider.authorize = &mut deny;
    let failed = inspect_with_provider(project.path(), Some(&mut provider)).unwrap();
    assert_eq!(failed.failures[0].code, "not_authorized");
    assert_eq!(failed.outcome, tapid_policy::AuditOutcome::Incomplete);
    assert_eq!(recorded, 13);
    for (path, expected) in [
        ("package.json", manifest.as_slice()),
        ("tapid.lock", bytes.as_bytes()),
        ("node_modules/sentinel", b"installed"),
        (".tapid-lifecycle-journal.json", b"pending"),
    ] {
        assert_eq!(std::fs::read(project.path().join(path)).unwrap(), expected);
    }
    assert_eq!(
        std::fs::read(home.path().join("tapid/store/sentinel")).unwrap(),
        b"store"
    );
}
