use tapid_protocol::{ReportClassification, ReportState, SecurityFindingV2};

/// Advisory severity floor. Malware and unknown severities always meet it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AuditThreshold {
    #[default]
    Any,
    Low,
    Moderate,
    High,
    Critical,
}

impl AuditThreshold {
    fn rank(self) -> u8 {
        match self {
            Self::Any => 0,
            Self::Low => 1,
            Self::Moderate => 2,
            Self::High => 3,
            Self::Critical => 4,
        }
    }

    fn matches(self, finding: &SecurityFindingV2) -> bool {
        if finding.state == ReportState::Withdrawn {
            return false;
        }
        if finding.classification == ReportClassification::ReportedMalware {
            return true;
        }
        let rank = match finding.severity.as_deref() {
            Some("low") => 1,
            Some("moderate") => 2,
            Some("high") => 3,
            Some("critical") => 4,
            // The wire permits provider-defined severity strings. Unknown
            // values cannot silently fall below a configured floor.
            _ => return true,
        };
        rank >= self.rank()
    }
}

/// Complete collection is required even when known findings already fail policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditOutcome {
    BelowThreshold,
    Findings,
    Incomplete,
}

impl AuditOutcome {
    /// Stable audit process outcome. Invalid invocation is handled separately
    /// by the CLI with exit 2, before policy evaluation.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::BelowThreshold => 0,
            Self::Findings => 1,
            Self::Incomplete => 3,
        }
    }
}

/// Evaluate already verified reports; this does not establish evidence validity
/// or collection completeness. Callers must retain coverage independently.
pub fn evaluate_audit<'a>(
    findings: impl IntoIterator<Item = &'a SecurityFindingV2>,
    complete: bool,
    threshold: AuditThreshold,
) -> AuditOutcome {
    if !complete {
        AuditOutcome::Incomplete
    } else if findings.into_iter().any(|f| threshold.matches(f)) {
        AuditOutcome::Findings
    } else {
        AuditOutcome::BelowThreshold
    }
}
