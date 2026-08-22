use serde::Serialize;
use std::fmt;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Network,
    FilesystemRead,
    FilesystemWrite,
    ProcessSpawn,
    EnvironmentRead,
    CredentialAccess,
    NativeCode,
    Unknown(String),
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Declared,
    Inferred,
    Observed,
    Enforced,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeclaredEvidence {
    pub capability: Capability,
    pub statement: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InferredEvidence {
    pub capability: Capability,
    pub statement: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ObservedEvidence {
    pub capability: Capability,
    pub statement: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EnforcedEvidence {
    pub capability: Capability,
    pub statement: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum Evidence {
    Declared(DeclaredEvidence),
    Inferred(InferredEvidence),
    Observed(ObservedEvidence),
    Enforced(EnforcedEvidence),
    Ambiguous {
        capability: Capability,
        statement: String,
    },
}
impl Evidence {
    pub fn declared(capability: Capability, statement: impl Into<String>) -> Self {
        Self::Declared(DeclaredEvidence {
            capability,
            statement: statement.into(),
        })
    }
    pub fn inferred(capability: Capability, statement: impl Into<String>) -> Self {
        Self::Inferred(InferredEvidence {
            capability,
            statement: statement.into(),
        })
    }
    pub fn observed(capability: Capability, statement: impl Into<String>) -> Self {
        Self::Observed(ObservedEvidence {
            capability,
            statement: statement.into(),
        })
    }
    pub fn enforced(capability: Capability, statement: impl Into<String>) -> Self {
        Self::Enforced(EnforcedEvidence {
            capability,
            statement: statement.into(),
        })
    }
    pub fn ambiguous(capability: Capability, statement: impl Into<String>) -> Self {
        Self::Ambiguous {
            capability,
            statement: statement.into(),
        }
    }
    pub fn kind(&self) -> EvidenceKind {
        match self {
            Self::Declared(_) => EvidenceKind::Declared,
            Self::Inferred(_) => EvidenceKind::Inferred,
            Self::Observed(_) => EvidenceKind::Observed,
            Self::Enforced(_) => EvidenceKind::Enforced,
            Self::Ambiguous { .. } => EvidenceKind::Inferred,
        }
    }
    pub fn capability(&self) -> &Capability {
        match self {
            Self::Declared(v) => &v.capability,
            Self::Inferred(v) => &v.capability,
            Self::Observed(v) => &v.capability,
            Self::Enforced(v) => &v.capability,
            Self::Ambiguous { capability, .. } => capability,
        }
    }
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous { .. })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Warn,
    Prompt,
    Deny,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ReasonCode {
    DeclaredNetworkCapability,
    InferredCapability,
    ObservedCapability,
    EnforcedCapability,
    AmbiguousEvidence,
    UnattendedRequiresPrompt,
    UnsupportedOs,
}
impl ReasonCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeclaredNetworkCapability => "POLICY_DECLARED_NETWORK",
            Self::InferredCapability => "POLICY_INFERRED_CAPABILITY",
            Self::ObservedCapability => "POLICY_OBSERVED_CAPABILITY",
            Self::EnforcedCapability => "POLICY_ENFORCED_CAPABILITY",
            Self::AmbiguousEvidence => "POLICY_AMBIGUOUS_EVIDENCE",
            Self::UnattendedRequiresPrompt => "POLICY_UNATTENDED_FAIL_CLOSED",
            Self::UnsupportedOs => "POLICY_OS_LIMITATION",
        }
    }
}
impl fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PolicyDecision {
    decision: Decision,
    reasons: Vec<ReasonCode>,
    evidence: Vec<Evidence>,
}
impl PolicyDecision {
    pub fn new(decision: Decision, reasons: Vec<ReasonCode>, evidence: Vec<Evidence>) -> Self {
        Self {
            decision,
            reasons,
            evidence,
        }
    }
    pub fn decision(&self) -> Decision {
        self.decision
    }
    pub fn reasons(&self) -> &[ReasonCode] {
        &self.reasons
    }
    pub fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }
    pub fn from_evidence(evidence: Vec<Evidence>, unattended: bool) -> Self {
        let mut reasons = Vec::new();
        let mut decision = Decision::Allow;
        if evidence.iter().any(Evidence::is_ambiguous) {
            decision = Decision::Deny;
            reasons.push(ReasonCode::AmbiguousEvidence);
        } else if unattended
            && evidence
                .iter()
                .any(|e| matches!(e.kind(), EvidenceKind::Inferred | EvidenceKind::Observed))
        {
            decision = Decision::Deny;
            reasons.push(ReasonCode::UnattendedRequiresPrompt);
        } else if evidence
            .iter()
            .any(|e| e.kind() == EvidenceKind::Declared && *e.capability() == Capability::Network)
        {
            decision = Decision::Warn;
            reasons.push(ReasonCode::DeclaredNetworkCapability);
        }
        Self::new(decision, reasons, evidence)
    }
    pub fn human(&self) -> String {
        let label = match self.decision {
            Decision::Allow => "allow",
            Decision::Warn => "warn",
            Decision::Prompt => "prompt",
            Decision::Deny => "deny",
        };
        format!(
            "{label}: {}",
            self.reasons
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("policy decision is serializable")
    }
    pub fn human_json_equivalent(&self) -> String {
        serde_json::to_string(self).expect("policy decision is serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versionless_scaffold_behavior() {
        let evidence = Evidence::declared(Capability::Network, "manifest scripts require network");
        assert_eq!(evidence.kind(), EvidenceKind::Declared);
        let decision = PolicyDecision::new(
            Decision::Warn,
            vec![ReasonCode::DeclaredNetworkCapability],
            vec![evidence],
        );
        assert_eq!(decision.to_json(), decision.human_json_equivalent());
    }
    #[test]
    fn ambiguous_evidence_is_not_allowed() {
        let decision = PolicyDecision::from_evidence(
            vec![Evidence::ambiguous(
                Capability::FilesystemWrite,
                "could write outside package",
            )],
            false,
        );
        assert_eq!(decision.decision(), Decision::Deny);
        assert!(decision.reasons().contains(&ReasonCode::AmbiguousEvidence));
    }
}
