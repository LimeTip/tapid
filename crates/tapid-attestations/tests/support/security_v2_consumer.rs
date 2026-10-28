//! Independent v2 conformance consumer. No current protocol DTO imports.
//! This fixture does not establish deployed Cloud/Web compatibility.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tapid_signatures::{KeyRing, TrustEnvelope};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: String,
    pub subject: String,
    pub artifact_digest: String,
    pub claims: ResultV2,
    pub signature: Signature,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub algorithm: String,
    pub key_id: String,
    pub subject: String,
    pub artifact_digest: String,
    pub value: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Package {
    pub registry: String,
    pub name: String,
    pub version: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Subject {
    pub package: Package,
    pub requested_name: String,
    pub placement: String,
    pub peers: BTreeMap<String, Package>,
    pub platform: Platform,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Platform {
    pub os: Option<String>,
    pub cpu: Option<String>,
    pub libc: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Check {
    Advisory,
    Artifact { digest: String },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "visibility", rename_all = "snake_case", deny_unknown_fields)]
pub enum Audience {
    Public,
    Tenant { tenant: String },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub version: String,
    pub subject: Subject,
    pub check: Check,
    pub coverage: Vec<String>,
    pub audience: Audience,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Findings,
    NoKnownFindings,
    NotScanned,
    Unavailable,
    Stale,
    Partial,
    Unsupported,
    InvalidEvidence,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    PackageVersion,
    ArtifactBytes,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Finding {
    pub id: String,
    pub summary: String,
    pub revision: String,
    pub classification: Classification,
    pub state: State,
    pub severity: Option<String>,
    pub fixes: Vec<Fix>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Advisory,
    ReportedMalware,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Active,
    Withdrawn,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fix {
    pub version: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultV2 {
    pub version: String,
    pub request: Request,
    pub issuer: String,
    pub sequence: u64,
    pub status: Status,
    pub scanner: String,
    pub scanner_version: String,
    pub rule_version: String,
    pub scope: Scope,
    pub coverage: Vec<String>,
    pub source_revision: String,
    pub checked_at: u64,
    pub issued_at: u64,
    pub expires_at: u64,
    pub findings: Vec<Finding>,
    pub limitations: Vec<String>,
}
pub fn verify(bytes: &[u8], keys: &KeyRing) -> Envelope {
    let envelope: Envelope = serde_json::from_slice(bytes).unwrap();
    assert_eq!(envelope.version, "tapid-trust-envelope-v1");
    assert_eq!(envelope.subject, "tapid-security-v2");
    assert_eq!(envelope.claims.version, "tapid-security-v2");
    assert_eq!(envelope.claims.request.version, "tapid-security-v2");
    assert_eq!(envelope.claims.issuer, "fixture.example");
    let binding = match &envelope.claims.request.check {
        Check::Advisory => {
            assert!(matches!(envelope.claims.scope, Scope::PackageVersion));
            "version-scope"
        }
        Check::Artifact { digest } => {
            assert!(matches!(envelope.claims.scope, Scope::ArtifactBytes));
            digest
        }
    };
    assert_eq!(envelope.artifact_digest, binding);
    serde_json::from_slice::<TrustEnvelope>(bytes)
        .unwrap()
        .verify_with_keyring(keys)
        .unwrap();
    envelope
}
