use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tapid_core::{ArtifactDigest, PackageName, PackageVersion, PlatformContext, RegistryOrigin};

use crate::PackageInstanceWire;

pub const SECURITY_CONTRACT_V2: &str = "tapid-security-v2";

pub const SECURITY_CONTRACT_V1: &str = "tapid-security-v1";
pub const MAX_SECURITY_BYTES: usize = 262_144;

/// An exact graph occurrence, including alias spelling and resolved peer origins.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecuritySubject {
    #[serde(with = "ExactPackage")]
    pub package: PackageInstanceWire,
    pub requested_name: String,
    /// Stable graph identifier using normalized relative slash-separated segments.
    /// Absolute paths, backslashes, colons, and empty/dot segments are invalid.
    pub placement: String,
    #[serde(deserialize_with = "unique_peers")]
    pub peers: BTreeMap<String, PackageInstanceWire>,
    pub platform: SecurityPlatform,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityPlatform {
    pub os: Option<String>,
    pub cpu: Option<String>,
    pub libc: Option<String>,
}

/// Advisory evidence intentionally has no artifact digest or byte-analysis claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SecurityCheck {
    Advisory,
    Artifact { digest: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "visibility", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceAudience {
    Public,
    Tenant { tenant: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityRequest {
    pub version: String,
    pub subject: SecuritySubject,
    pub check: SecurityCheck,
    /// Nonempty, unique coverage identifiers, interpreted by the named scanner.
    pub coverage: Vec<String>,
    pub audience: EvidenceAudience,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityStatus {
    Findings,
    NoKnownFindings,
    NotScanned,
    Unavailable,
    Stale,
    Partial,
    Unsupported,
    InvalidEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityFinding {
    pub id: String,
    pub summary: String,
    pub severity: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityResultClaims<F> {
    pub version: String,
    pub request: SecurityRequest,
    pub issuer: String,
    /// Monotonically increasing per issuer and exact request, including withdrawals.
    pub sequence: u64,
    pub status: SecurityStatus,
    pub scanner: String,
    pub scanner_version: String,
    pub rule_version: String,
    pub scope: SecurityScope,
    pub coverage: Vec<String>,
    pub source_revision: String,
    /// UTC Unix seconds. No floats, negative values, or implicit local time.
    pub checked_at: u64,
    pub issued_at: u64,
    pub expires_at: u64,
    pub findings: Vec<F>,
    pub limitations: Vec<String>,
}

/// Stable codes, serialized identically by native and hosted consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityReason {
    InputTooLarge,
    Malformed,
    UnsupportedVersion,
    InvalidRequest,
    SubjectMismatch,
    DigestMismatch,
    UnknownIssuer,
    UntrustedKey,
    InvalidSignature,
    InvalidEnvelope,
    InvalidResult,
    Expired,
    FutureEvidence,
    Replay,
    IncompleteCoverage,
}

impl SecurityRequest {
    /// Checks v1 identity, portable placement, context and coverage invariants.
    pub fn validate(&self) -> Result<(), SecurityReason> {
        self.validate_for(SECURITY_CONTRACT_V1)
    }

    pub fn validate_v2(&self) -> Result<(), SecurityReason> {
        self.validate_for(SECURITY_CONTRACT_V2)?;
        if matches!(self.check, SecurityCheck::Advisory)
            && self
                .coverage
                .iter()
                .any(|c| !matches!(c.as_str(), "advisory" | "reported_malware"))
        {
            return Err(SecurityReason::InvalidRequest);
        }
        Ok(())
    }

    fn validate_for(&self, contract: &str) -> Result<(), SecurityReason> {
        if self.version != contract {
            return Err(SecurityReason::UnsupportedVersion);
        }
        let subject = &self.subject;
        if !valid_package(&subject.package)
            || subject.requested_name.parse::<PackageName>().is_err()
            || !valid_placement(&subject.placement)
            || subject.peers.len() > 128
            || subject.peers.iter().any(|(name, package)| {
                name.parse::<PackageName>().is_err() || !valid_package(package)
            })
            || [
                &subject.platform.os,
                &subject.platform.cpu,
                &subject.platform.libc,
            ]
            .into_iter()
            .flatten()
            .any(|value| !text(value))
            || PlatformContext::new(
                subject.platform.os.as_deref(),
                subject.platform.cpu.as_deref(),
                subject.platform.libc.as_deref(),
            )
            .is_err()
            || !valid_coverage(&self.coverage)
            || matches!(&self.audience, EvidenceAudience::Tenant { tenant } if !text(tenant))
        {
            return Err(SecurityReason::InvalidRequest);
        }
        if let SecurityCheck::Artifact { digest } = &self.check
            && digest
                .parse::<ArtifactDigest>()
                .map(|d| d.to_string())
                .ok()
                .as_deref()
                != Some(digest.as_str())
        {
            return Err(SecurityReason::InvalidRequest);
        }
        Ok(())
    }
}

fn valid_placement(value: &str) -> bool {
    text(value)
        && !value.starts_with('/')
        && !value.contains(['\\', ':'])
        && !value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

fn valid_package(package: &PackageInstanceWire) -> bool {
    package
        .registry
        .parse::<RegistryOrigin>()
        .map(|o| o.to_string())
        .ok()
        .as_deref()
        == Some(package.registry.as_str())
        && package.name.parse::<PackageName>().is_ok()
        && package
            .version
            .parse::<PackageVersion>()
            .map(|v| v.to_string())
            .ok()
            .as_deref()
            == Some(package.version.as_str())
}

pub(crate) fn text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

pub(crate) fn valid_coverage(values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= 128
        && values
            .iter()
            .enumerate()
            .all(|(i, v)| text(v) && !values[..i].contains(v))
}

/// The v1 finding schema remains closed; v2 metadata is never silently discarded.
pub type SecurityResult = SecurityResultClaims<SecurityFinding>;
pub type SecurityResultV2 = SecurityResultClaims<SecurityFindingV2>;

impl SecurityResult {
    pub fn validate(&self) -> Result<(), SecurityReason> {
        if self.version != SECURITY_CONTRACT_V1 {
            return Err(SecurityReason::UnsupportedVersion);
        }
        self.request.validate()?;
        if self
            .findings
            .iter()
            .any(|f| !text(&f.id) || !text(&f.summary) || !text(&f.severity))
        {
            return Err(SecurityReason::InvalidResult);
        }
        self.validate_metadata(SECURITY_CONTRACT_V1, self.findings.len())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportClassification {
    Advisory,
    ReportedMalware,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportState {
    Active,
    Withdrawn,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityFix {
    /// Exact replacement version at the same origin/name; not a remediation plan.
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityFindingV2 {
    pub id: String,
    pub revision: String,
    pub classification: ReportClassification,
    pub state: ReportState,
    pub summary: String,
    pub severity: Option<String>,
    pub fixes: Vec<SecurityFix>,
}

impl SecurityResultV2 {
    pub fn validate(&self) -> Result<(), SecurityReason> {
        let active = self
            .findings
            .iter()
            .filter(|f| f.state == ReportState::Active)
            .count();
        self.validate_metadata(SECURITY_CONTRACT_V2, active)?;
        for (i, finding) in self.findings.iter().enumerate() {
            let coverage = match finding.classification {
                ReportClassification::Advisory => "advisory",
                ReportClassification::ReportedMalware => "reported_malware",
            };
            if !text(&finding.id)
                || !text(&finding.revision)
                || !text(&finding.summary)
                || finding.severity.as_ref().is_some_and(|v| !text(v))
                || finding.fixes.len() > 128
                || finding.fixes.iter().any(|f| {
                    f.version
                        .parse::<PackageVersion>()
                        .map(|v| v.to_string())
                        .ok()
                        .as_deref()
                        != Some(f.version.as_str())
                })
                || self.findings[..i].iter().any(|f| f.id == finding.id)
                || (self.scope == SecurityScope::PackageVersion
                    && !self.request.coverage.iter().any(|c| c == coverage))
                || (self.scope == SecurityScope::PackageVersion
                    && finding.state == ReportState::Active
                    && !self.coverage.iter().any(|c| c == coverage))
                || (finding.state == ReportState::Withdrawn
                    && self.status != SecurityStatus::InvalidEvidence)
            {
                return Err(SecurityReason::InvalidResult);
            }
        }
        Ok(())
    }
}

impl<F> SecurityResultClaims<F> {
    fn validate_metadata(
        &self,
        contract: &str,
        active_findings: usize,
    ) -> Result<(), SecurityReason> {
        if self.version != contract {
            return Err(SecurityReason::UnsupportedVersion);
        }
        if contract == SECURITY_CONTRACT_V2 {
            self.request.validate_v2()?;
        } else {
            self.request.validate()?;
        }
        if [
            &self.issuer,
            &self.scanner,
            &self.scanner_version,
            &self.rule_version,
            &self.source_revision,
        ]
        .iter()
        .any(|v| !text(v))
            || !matches!(
                (&self.request.check, self.scope),
                (SecurityCheck::Advisory, SecurityScope::PackageVersion)
                    | (SecurityCheck::Artifact { .. }, SecurityScope::ArtifactBytes)
            )
            || [
                self.sequence,
                self.checked_at,
                self.issued_at,
                self.expires_at,
            ]
            .iter()
            .any(|v| *v > 9_007_199_254_740_991)
            || (matches!(
                self.status,
                SecurityStatus::NotScanned
                    | SecurityStatus::Unavailable
                    | SecurityStatus::Unsupported
                    | SecurityStatus::InvalidEvidence
            ) && (!self.coverage.is_empty() || active_findings != 0))
            || self.sequence == 0
            || self.checked_at > self.issued_at
            || self.issued_at >= self.expires_at
            || self.coverage.len() > 128
            || self.coverage.iter().enumerate().any(|(i, v)| {
                !text(v) || self.coverage[..i].contains(v) || !self.request.coverage.contains(v)
            })
            || self.findings.len() > 256
            || self.limitations.len() > 128
            || self.limitations.iter().any(|v| !text(v))
            || (self.status == SecurityStatus::Findings && active_findings == 0)
            || (self.status == SecurityStatus::NoKnownFindings && active_findings != 0)
        {
            return Err(SecurityReason::InvalidResult);
        }
        if matches!(
            self.status,
            SecurityStatus::Findings | SecurityStatus::NoKnownFindings
        ) && self
            .request
            .coverage
            .iter()
            .any(|c| !self.coverage.contains(c))
        {
            return Err(SecurityReason::IncompleteCoverage);
        }
        if matches!(
            self.status,
            SecurityStatus::Partial
                | SecurityStatus::NotScanned
                | SecurityStatus::Unavailable
                | SecurityStatus::Stale
                | SecurityStatus::Unsupported
                | SecurityStatus::InvalidEvidence
        ) && self.limitations.is_empty()
        {
            return Err(SecurityReason::InvalidResult);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityScope {
    PackageVersion,
    ArtifactBytes,
}

/// Normalized claims returned by the hosted authority's authenticated
/// introspection. Deserializing these claims does not authenticate a credential.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountAuthorization {
    pub version: String,
    pub authority: String,
    pub subject: String,
    pub tenant: String,
    pub audience: String,
    pub scopes: Vec<SecurityPermission>,
    pub issued_at: u64,
    pub expires_at: u64,
    pub credential_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityPermission {
    ReadPrivate,
    SubmitAnalysis,
    PublishEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityServiceError {
    AuthenticationRequired,
    InvalidCredential,
    CredentialExpired,
    CredentialRevoked,
    WrongAudience,
    Forbidden,
    TenantMismatch,
    NotFound,
    RateLimited,
    Unavailable,
    UnsupportedVersion,
    InvalidRequest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityErrorResponse {
    pub version: String,
    pub code: SecurityServiceError,
    pub retry_after_seconds: Option<u64>,
}

// The established identity DTO tolerates extensions. Evidence v1 is stricter
// because ignoring future identity semantics could change applicability.
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "PackageInstanceWire",
    rename_all = "camelCase",
    deny_unknown_fields
)]
struct ExactPackage {
    registry: String,
    name: String,
    version: String,
}

fn unique_peers<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, PackageInstanceWire>, D::Error> {
    struct Peers;
    #[derive(Deserialize)]
    struct ExactPeer(#[serde(with = "ExactPackage")] PackageInstanceWire);
    impl<'de> serde::de::Visitor<'de> for Peers {
        type Value = BTreeMap<String, PackageInstanceWire>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a unique peer map with at most 128 entries")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut peers = BTreeMap::new();
            while let Some(name) = map.next_key::<String>()? {
                if peers.len() >= 128 || peers.contains_key(&name) {
                    return Err(serde::de::Error::custom("duplicate or excessive peers"));
                }
                let ExactPeer(package) = map.next_value()?;
                peers.insert(name, package);
            }
            Ok(peers)
        }
    }
    deserializer.deserialize_map(Peers)
}

/// Explicit publication intent, authorized separately from analysis submission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidencePublicationRequest {
    pub version: String,
    pub tenant: String,
    pub evidence_digest: String,
    pub expected_sequence: u64,
}
