use crate::Decision;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Checked-in requirements. This is configuration, not a security evidence format.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityPolicy {
    version: u32,
    rules: Vec<SecurityRule>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct SecurityRule {
    id: String,
    kind: RuleKind,
    mode: RuleMode,
    sources: Vec<String>,
    max_age_seconds: u32,
    status: StatusRequirement,
    threshold: Option<Severity>,
    profile: Option<String>,
    revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuleKind {
    Malware,
    Vulnerability,
    ContentAnalysis,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Severity {
    Low,
    Moderate,
    High,
    Critical,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuleMode {
    Advisory,
    Required,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StatusRequirement {
    Cached,
    Current,
}

/// Result when no authenticated evidence consumer is available. No result here
/// grants execution authority or claims that selected content has been scanned.
#[derive(Clone, Debug, Serialize)]
pub struct SecurityDecision {
    pub decision: Decision,
    pub reason: &'static str,
    pub rules: Vec<String>,
}

impl SecurityPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 || self.rules.is_empty() || self.rules.len() > 128 {
            return Err("unsupported security version or invalid rule count");
        }
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            let kind_valid = match rule.kind {
                RuleKind::Malware => {
                    rule.threshold.is_none() && rule.profile.is_none() && rule.revision.is_none()
                }
                RuleKind::Vulnerability => {
                    rule.threshold.is_some() && rule.profile.is_none() && rule.revision.is_none()
                }
                RuleKind::ContentAnalysis => {
                    rule.threshold.is_none()
                        && rule.profile.as_deref().is_some_and(valid_label)
                        && rule.revision.as_deref().is_some_and(valid_label)
                }
            };
            if !valid_label(&rule.id)
                || !kind_valid
                || !ids.insert(&rule.id)
                || rule.sources.is_empty()
                || rule.sources.len() > 32
                || rule.sources.iter().any(|source| !valid_label(source))
                || rule.sources.iter().collect::<BTreeSet<_>>().len() != rule.sources.len()
                || rule.max_age_seconds == 0
            {
                return Err("invalid security rule");
            }
        }
        Ok(())
    }

    /// Explicitly unavailable evidence cannot satisfy any required rule.
    /// This conservative gate must be replaced by shared authenticated audit
    /// evidence evaluation before evidence-backed installation can be supported.
    pub fn unavailable_evidence(&self) -> Result<SecurityDecision, &'static str> {
        self.validate()?;
        Ok(SecurityDecision {
            decision: if self
                .rules
                .iter()
                .any(|rule| matches!(rule.mode, RuleMode::Required))
            {
                Decision::Deny
            } else {
                Decision::Warn
            },
            reason: "POLICY_SECURITY_EVIDENCE_UNAVAILABLE",
            rules: self.rules.iter().map(|rule| rule.id.clone()).collect(),
        })
    }
}

fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
