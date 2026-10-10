use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha512};
use std::process::Command;
use tapid_test_support::TempProject;

fn project() -> TempProject {
    let project = TempProject::new("explain").unwrap();
    let integrity = format!(
        "sha512-{}",
        STANDARD.encode(Sha512::digest(b"archive bytes"))
    );
    project
        .write(
            "metadata.json",
            &serde_json::to_vec(&serde_json::json!({
                "name":"@example/pkg", "versions":{"1.2.3":{
                    "name":"@example/pkg", "version":"1.2.3", "dist":{
                        "integrity":integrity, "tarball":"https://registry.npmjs.org/pkg.tgz",
                        "signatures":[{"keyid":"example", "sig":"claim"}],
                        "attestations":{"url":"https://registry.npmjs.org/attestations"}
                    }
                }}
            }))
            .unwrap(),
        )
        .unwrap();
    project.write("archive.tgz", b"archive bytes").unwrap();
    project
}

fn run(project: &TempProject, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args([
            "explain",
            "@example/pkg@1.2.3",
            "--registry-metadata",
            "metadata.json",
        ])
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn explain_distinguishes_reported_missing_and_verified_evidence_without_mutation() {
    let project = project();
    let before = std::fs::read(project.path().join("metadata.json")).unwrap();
    let output = run(&project, &[]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "Registry-reported integrity",
        "not checked",
        "unverified",
        "Vulnerabilities: unavailable",
        "Publisher identity: not verified",
        "Malware analysis: not performed",
        "Human review: unavailable",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    let output = run(&project, &["--artifact-file", "archive.tgz"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Byte integrity: verified match")
    );
    assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 2);
    assert_eq!(
        std::fs::read(project.path().join("metadata.json")).unwrap(),
        before
    );
}

#[test]
fn explain_json_retains_integrity_mismatch_and_returns_failure() {
    let project = project();
    project.write("archive.tgz", b"tampered").unwrap();
    let output = run(&project, &["--artifact-file", "archive.tgz", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["operation"], "explain");
    assert_eq!(result["outcome"], "failure");
    assert_eq!(result["errors"][0]["code"], "INTEGRITY_MISMATCH");
    assert_eq!(result["data"]["byte_integrity"]["status"], "mismatch");
    assert!(result["data"]["byte_integrity"]["checked_at"].is_string());
    assert_eq!(result["data"]["vulnerabilities"]["status"], "unavailable");
    assert_eq!(result["changes"]["state"], "unchanged");
}

#[test]
fn explain_missing_evidence_is_unknown_in_json() {
    let project = project();
    project.write("metadata.json", br#"{"name":"@example/pkg","versions":{"1.2.3":{"name":"@example/pkg","version":"1.2.3"}}}"#).unwrap();
    let output = run(&project, &["--json"]);
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["registry_integrity"]["status"], "missing");
    assert!(result["data"]["registry_integrity"]["value"].is_null());
    assert_eq!(result["data"]["provenance"]["status"], "missing");
    assert_eq!(result["data"]["byte_integrity"]["status"], "not_checked");
    assert!(result["data"]["source_timestamp"].is_null());
    assert_eq!(result["data"]["source_timestamp_status"], "missing");
    assert!(result["data"]["published_at"].is_null());
    assert_eq!(result["data"]["freshness"], "unknown");
    let output = run(&project, &["--json", "--artifact-file", "archive.tgz"]);
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["data"]["byte_integrity"]["status"],
        "missing_expected_integrity"
    );
    assert!(result["data"]["byte_integrity"]["actual"].is_string());
}

#[test]
fn explain_observation_times_do_not_establish_evidence_freshness() {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    let project = project();
    let path = project.path().join("metadata.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    metadata["time"] =
        serde_json::json!({"1.2.3":"2020-01-01T00:00:00Z", "modified":"2021-02-03T04:05:06Z"});
    project
        .write("metadata.json", &serde_json::to_vec(&metadata).unwrap())
        .unwrap();
    let before = OffsetDateTime::now_utc();
    let output = run(&project, &["--json", "--artifact-file", "archive.tgz"]);
    let after = OffsetDateTime::now_utc();
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let data = &result["data"];
    assert!(data["metadata_fetched_at"].is_null());
    let read_at =
        OffsetDateTime::parse(data["metadata_read_at"].as_str().unwrap(), &Rfc3339).unwrap();
    let checked_at = OffsetDateTime::parse(
        data["byte_integrity"]["checked_at"].as_str().unwrap(),
        &Rfc3339,
    )
    .unwrap();
    assert!(before <= read_at && read_at <= checked_at && checked_at <= after);
    assert_eq!(data["source_timestamp"], "2021-02-03T04:05:06Z");
    assert_eq!(data["source_timestamp_status"], "reported");
    assert_eq!(data["published_at"], "2020-01-01T00:00:00Z");
    assert_eq!(data["freshness"], "unknown");
    let output = run(&project, &[]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Metadata snapshot read at:"));
    assert!(text.contains("Registry-reported publication time: 2020-01-01T00:00:00Z"));
    assert!(text.contains("Evidence freshness: unknown"));
    let output = run(&project, &["--json"]);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result["data"]["byte_integrity"]["checked_at"].is_null());
}

#[test]
fn explain_private_registry_missing_credential_fails_before_network() {
    let project = project();
    project
        .write(
            "tapid.toml",
            br#"[registries."@example"]
url = "https://private.example"
token-env = "TAPID_TEST_EXPLAIN_TOKEN"
"#,
        )
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .env_remove("TAPID_TEST_EXPLAIN_TOKEN")
        .args(["--json", "explain", "@example/pkg@1.2.3"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["errors"][0]["code"], "REGISTRY_AUTH_MISSING");
    assert_eq!(result["errors"][0]["phase"], "operation");
    // A snapshot records the configured identity without consulting credentials.
    let output = run(&project, &["--json"]);
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["registry"], "https://private.example");
}

#[test]
fn explain_bounds_untrusted_metadata_without_echoing_signature_content() {
    let project = project();
    let url = format!("https://registry.npmjs.org/{}", "a".repeat(6000));
    project.write("metadata.json", &serde_json::to_vec(&serde_json::json!({
        "name":"@example/pkg", "versions":{"1.2.3":{
            "name":"@example/pkg", "version":"1.2.3", "dist":{
                "tarball":url, "signatures":[{"keyid":"\u{1b}[31mATTACK", "sig":"untrusted instruction"}],
                "attestations":{"url":url}
            }
        }}
    })).unwrap()).unwrap();
    let output = run(&project, &["--json"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["artifact"].as_str().unwrap().len(), 4096);
    assert!(
        result["truncated_fields"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("/data/artifact"))
    );
    assert!(!String::from_utf8(output.stdout).unwrap().contains("ATTACK"));
}

#[test]
fn explain_rejects_ranges_missing_versions_and_malformed_evidence() {
    let project = project();
    for spec in [
        "foo",
        "foo@latest",
        "foo@^1",
        "foo@",
        "jsr:@example/pkg@1.2.3",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .current_dir(project.path())
            .args(["explain", spec, "--json"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stderr.is_empty());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["errors"][0]["code"], "INVALID_REQUEST");
    }
    project.write("metadata.json", br#"{"name":"@example/pkg","versions":{"1.2.3":{"name":"@example/pkg","version":"1.2.3","dist":{"integrity":"sha512-bad"}}}}"#).unwrap();
    let output = run(&project, &["--json"]);
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["errors"][0]["code"], "REGISTRY_METADATA_INVALID");
}
