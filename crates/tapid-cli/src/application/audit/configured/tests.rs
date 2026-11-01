use super::*;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Cursor, os::unix::fs::MetadataExt, path::PathBuf};
use tapid_lockfile::{LockedPackage, Lockfile, RegistryIntegrityProvenance};
use tapid_security_provider::RetrievalError;

// Compare complete trees without following links. Access times are deliberately
// excluded: read-only inspection may update them on the host filesystem.
#[derive(Debug, Eq, PartialEq)]
struct SnapshotEntry {
    mode: u32,
    owner: (u32, u32),
    identity: (u64, u64, u64),
    modified: (i64, i64),
    changed: (i64, i64),
    bytes: Vec<u8>,
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, SnapshotEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, SnapshotEntry>) {
        let meta = fs::symlink_metadata(path).unwrap();
        let bytes = if meta.is_file() {
            fs::read(path).unwrap()
        } else if meta.file_type().is_symlink() {
            use std::os::unix::ffi::OsStrExt;
            fs::read_link(path).unwrap().as_os_str().as_bytes().to_vec()
        } else {
            assert!(meta.is_dir(), "unexpected fixture type");
            Vec::new()
        };
        entries.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            SnapshotEntry {
                mode: meta.mode(),
                owner: (meta.uid(), meta.gid()),
                identity: (meta.dev(), meta.ino(), meta.nlink()),
                modified: (meta.mtime(), meta.mtime_nsec()),
                changed: (meta.ctime(), meta.ctime_nsec()),
                bytes,
            },
        );
        if meta.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn signed_response(
    request: &tapid_protocol::SecurityRequest,
    scenario: &str,
    now: u64,
) -> Result<Box<dyn std::io::Read>, RetrievalError> {
    if scenario == "unavailable" {
        return Err(RetrievalError::Unavailable);
    }
    if scenario == "malformed" {
        return Ok(Box::new(Cursor::new(b"invalid")) as Box<dyn std::io::Read>);
    }
    let findings = if matches!(scenario, "findings" | "withdrawn" | "stale" | "tampered") {
        serde_json::json!([{
            "id":if request.subject.requested_name == "renamed" { "REPORT-MALWARE" } else { "REPORT-1" },"revision":"2","classification":if request.subject.requested_name == "renamed" { "reported_malware" } else { "advisory" },"state":if scenario == "withdrawn" { "withdrawn" } else { "active" },
            "summary":"fixture","severity":"high","fixes":[{"version":"1.0.1"}]
        }])
    } else {
        serde_json::json!([])
    };
    let claims = serde_json::json!({
        "version":"tapid-security-v2","request":request,"issuer":"fixture.example",
        "scope":"package_version","status":match scenario { "withdrawn" => "invalid_evidence", "tampered" => "findings", _ => scenario },"coverage":if scenario == "withdrawn" { vec![] } else { vec!["advisory","reported_malware"] },
        "limitations":match scenario { "stale" => vec!["refresh unavailable"], "withdrawn" => vec!["report withdrawn"], _ => vec![] },"checkedAt":now,"issuedAt":now,"expiresAt":now+600,
        "sequence":3,"scanner":"fixture","scannerVersion":"1","ruleVersion":"1",
        "sourceRevision":"reports:3","findings":findings
    });
    let seed: [u8; 32] =
        hex::decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
            .unwrap()
            .try_into()
            .unwrap();
    let mut envelope =
        tapid_signatures::TrustEnvelope::unsigned("tapid-security-v2", "version-scope", claims)
            .sign("fixture", &seed)
            .unwrap();
    if scenario == "tampered" {
        envelope.claims["sourceRevision"] = serde_json::json!("forged");
    }
    Ok(Box::new(Cursor::new(serde_json::to_vec(&envelope).unwrap())) as Box<dyn std::io::Read>)
}

#[test]
fn configured_signed_collection_preserves_complete_project_and_store_trees() {
    let project = tapid_test_support::TempProject::new("configured-signed").unwrap();
    let home = tapid_test_support::TempHome::new("configured-signed").unwrap();
    let manifest = br#"{"name":"audit","version":"1.0.0","dependencies":{"direct":"1.0.0"}}"#;
    project.write("package.json", manifest).unwrap();
    let mut lock = Lockfile::new(&crate::filesystem::atomic::digest_bytes(manifest)).unwrap();
    let package = |name| {
        LockedPackage::new_with_provenance(
            "https://registry.example",
            name,
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "a".repeat(64)),
            RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap()
    };
    let nested = package("transitive");
    let mut direct = package("direct");
    direct
        .add_alias_dependency("renamed", &nested.key())
        .unwrap();
    let key = direct.key();
    lock.insert_packages([direct, nested]).unwrap();
    lock.set_roots([key]).unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    project
        .write(
            "node_modules/installed/index.js",
            b"throw new Error('must not run')",
        )
        .unwrap();
    project
        .write(
            ".tapid-lifecycle-journal.json",
            b"pending recovery must remain untouched",
        )
        .unwrap();
    fs::create_dir(project.path().join("empty")).unwrap();
    std::os::unix::fs::symlink("installed", project.path().join("node_modules/alias")).unwrap();
    home.write("store/verified/content", b"verified store bytes")
        .unwrap();
    fs::create_dir(home.path().join("store/empty")).unwrap();
    let before_store = snapshot(&home.path().join("store"));
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{
        "":serde_json::from_slice::<serde_json::Value>(manifest).unwrap(),
        "node_modules/direct":{"version":"1.0.0","resolved":"https://registry.example/direct/-/direct-1.0.0.tgz",
            "integrity":format!("sha512-{}==", "A".repeat(86)),"dependencies":{"renamed":"npm:transitive@1.0.0"}},
        "node_modules/renamed":{"name":"transitive","version":"1.0.0","resolved":"https://registry.example/transitive/-/transitive-1.0.0.tgz",
            "integrity":format!("sha512-{}==", "A".repeat(86))}
    }});
    let imported = tapid_lockfile::ImportedNpmLockfile::import(
        &npm.to_string(),
        std::str::from_utf8(manifest).unwrap(),
        &crate::filesystem::atomic::digest_bytes(manifest),
    )
    .unwrap();
    for (format, bytes, inventory) in [
        (
            "native",
            lock.to_json().unwrap(),
            super::super::requests::native(&lock.graph().unwrap()),
        ),
        (
            "imported_npm",
            imported.to_json().unwrap(),
            super::super::requests::imported(&imported.graph().unwrap()),
        ),
    ] {
        project.write("tapid.lock", bytes.as_bytes()).unwrap();
        let before_project = snapshot(project.path());
        let requests = inventory
            .requests
            .into_iter()
            .map(|(_, request)| request)
            .collect::<Vec<_>>();
        home.write("provider.json", br#"{"version":1,"origin":"https://cloud.invalid","headlessCredentialEnv":"AUDIT_TEST_CI","cache":"disabled"}"#).unwrap();
        let public =
            hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
                .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        home.write(
            "trust.json",
            &serde_json::to_vec(&serde_json::json!({
                "version":1,"issuer":"fixture.example","validUntil":now+3600,"maxAge":300,
                "keyring":{"version":"tapid-release-keyring-v1","keys":[{
                    "key_id":"fixture","algorithm":"ed25519",
                    "public_key":base64::engine::general_purpose::STANDARD.encode(&public),
                    "fingerprint":format!("sha256-{:x}",Sha256::digest(&public))
                }]}
            }))
            .unwrap(),
        )
        .unwrap();
        // Exercise real configuration, consent, keyring loading and durable state.
        // Only the external byte-stream transport is replaced; verification is real.
        for scenario in [
            "findings",
            "no_known_findings",
            "withdrawn",
            "stale",
            "malformed",
            "unavailable",
            "tampered",
            "denied",
        ] {
            home.write(
                "consent.json",
                &serde_json::to_vec(&serde_json::json!({
                    "version":1,"origin":"https://cloud.invalid",
                    "requests":if scenario == "denied" { vec![] } else { requests.clone() },
                }))
                .unwrap(),
            )
            .unwrap();
            let report = inspect_configured_using(
                project.path(),
                &home.path().join("provider.json"),
                &home.path().join("consent.json"),
                &home.path().join("trust.json"),
                &home.path().join(format!("replay-{scenario}")),
                CredentialMode::Headless,
                |config, consented, mode| {
                    Ok(Box::new(move |request| {
                        assert_ne!(scenario, "denied", "denied inventory reached transport");
                        config.authorize(
                            request,
                            &consented,
                            mode,
                            "https://cloud.invalid",
                            &mut |name| {
                                assert_eq!(name, "AUDIT_TEST_CI");
                                Some("fixture-ci-token".into())
                            },
                        )?;
                        signed_response(request, scenario, now)
                    }))
                },
            )
            .unwrap();
            assert_eq!(report.format, format);
            if matches!(
                scenario,
                "findings" | "no_known_findings" | "withdrawn" | "stale"
            ) {
                assert_eq!(
                    report.evidence.len(),
                    2,
                    "{scenario}: {:?}",
                    report
                        .failures
                        .iter()
                        .map(|failure| (failure.code, failure.reason))
                        .collect::<Vec<_>>()
                );
                assert!(report.failures.is_empty());
                assert_eq!(
                    report.outcome.exit_code(),
                    match scenario {
                        "findings" => 1,
                        "stale" | "withdrawn" => 3,
                        _ => 0,
                    }
                );
                let nested = report
                    .evidence
                    .iter()
                    .find(|entry| entry.result.request.subject.requested_name == "renamed")
                    .unwrap();
                assert_eq!(nested.paths[0].steps.len(), 2);
                if scenario != "no_known_findings" {
                    assert_eq!(nested.result.findings[0].id, "REPORT-MALWARE");
                    assert_eq!(nested.result.findings[0].revision, "2");
                    assert_eq!(
                        nested.result.findings[0].classification,
                        tapid_protocol::ReportClassification::ReportedMalware
                    );
                }
                let replay = tapid_security_provider::ReplayStore::open(
                    &home.path().join(format!("replay-{scenario}")),
                )
                .unwrap();
                assert_eq!(
                    replay
                        .load("fixture.example", &nested.result.request)
                        .unwrap()
                        .unwrap()
                        .0,
                    3
                );
            } else {
                assert!(report.evidence.is_empty());
                assert_eq!(report.failures.len(), 2);
                assert_eq!(report.outcome.exit_code(), 3);
                assert!(report.failures.iter().all(|failure| failure.code
                    == if matches!(scenario, "malformed" | "tampered") {
                        "invalid_evidence"
                    } else if scenario == "denied" {
                        "not_authorized"
                    } else {
                        "unavailable"
                    }));
            }
            assert_eq!(snapshot(project.path()), before_project, "{scenario}");
            assert_eq!(
                snapshot(&home.path().join("store")),
                before_store,
                "{scenario}"
            );
        }
    }
}
