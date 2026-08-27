use sha2::{Digest, Sha256};
use std::fmt;
use tapid_policy::{Decision, Evidence, PolicyDecision, ReasonCode};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ScriptHash(String);
impl ScriptHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for ScriptHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn normalize_script(script: &str) -> String {
    script
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned()
}
pub fn normalized_script_hash(script: &str) -> ScriptHash {
    let digest = Sha256::digest(normalize_script(script).as_bytes());
    ScriptHash(format!("sha256-{}", hex_lower(&digest)))
}
fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerRequest {
    pub artifact_digest: String,
    pub script: String,
    pub unattended: bool,
    pub os: String,
}
impl RunnerRequest {
    pub fn script_hash(&self) -> ScriptHash {
        normalized_script_hash(&self.script)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Approval {
    pub artifact_digest: String,
    pub script_hash: ScriptHash,
}
impl Approval {
    pub fn for_request(request: &RunnerRequest) -> Self {
        Self {
            artifact_digest: request.artifact_digest.clone(),
            script_hash: request.script_hash(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    ArtifactDigestMismatch,
    ScriptHashMismatch,
    MissingApproval,
    UnsupportedOs,
    PolicyDenied,
    RequestMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerPlan {
    policy: PolicyDecision,
    approval_required: bool,
    containment_available: bool,
    request: RunnerRequest,
}
impl RunnerPlan {
    pub fn policy(&self) -> &PolicyDecision {
        &self.policy
    }
    pub fn approval_required(&self) -> bool {
        self.approval_required
    }
    pub fn containment_available(&self) -> bool {
        self.containment_available
    }
}
impl RunnerPlan {
    pub fn validate_approval(
        &self,
        request: &RunnerRequest,
        approval: Option<&Approval>,
    ) -> Result<(), ValidationError> {
        if request.artifact_digest != self.request.artifact_digest {
            return Err(ValidationError::ArtifactDigestMismatch);
        }
        if request.script_hash() != self.request.script_hash() {
            return Err(ValidationError::ScriptHashMismatch);
        }
        if request.unattended != self.request.unattended || request.os != self.request.os {
            return Err(ValidationError::RequestMismatch);
        }
        if self.policy.decision() == Decision::Deny {
            return Err(ValidationError::PolicyDenied);
        }
        let Some(approval) = approval else {
            return if self.approval_required {
                Err(ValidationError::MissingApproval)
            } else {
                Ok(())
            };
        };
        if approval.artifact_digest != request.artifact_digest {
            return Err(ValidationError::ArtifactDigestMismatch);
        }
        if approval.script_hash != request.script_hash() {
            return Err(ValidationError::ScriptHashMismatch);
        }
        Ok(())
    }
}

pub fn plan(request: &RunnerRequest, evidence: Vec<Evidence>) -> RunnerPlan {
    let mut policy = PolicyDecision::from_evidence(evidence, request.unattended);
    if !matches!(request.os.as_str(), "linux" | "macos" | "windows") {
        policy = PolicyDecision::new(
            Decision::Deny,
            vec![ReasonCode::UnsupportedOs],
            policy.evidence().to_vec(),
        );
    }
    let approval_required = !matches!(policy.decision(), Decision::Allow);
    RunnerPlan {
        policy,
        approval_required,
        containment_available: false,
        request: request.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_policy::{Capability, Evidence};
    fn request() -> RunnerRequest {
        RunnerRequest {
            artifact_digest: "sha256-aaaa".into(),
            script: " echo hi\r\n".into(),
            unattended: false,
            os: "linux".into(),
        }
    }
    #[test]
    fn approval_binds_exact_artifact_and_normalized_script() {
        let r = request();
        let p = plan(&r, vec![]);
        assert!(
            p.validate_approval(&r, Some(&Approval::for_request(&r)))
                .is_ok()
        );
        let mut changed = r.clone();
        changed.artifact_digest = "sha256-bbbb".into();
        assert_eq!(
            p.validate_approval(&changed, Some(&Approval::for_request(&r))),
            Err(ValidationError::ArtifactDigestMismatch)
        );
    }
    #[test]
    fn changed_script_cannot_reuse_approval() {
        let r = request();
        let p = plan(&r, vec![]);
        let approval = Approval::for_request(&r);
        let mut changed = r.clone();
        changed.script.push_str(" && whoami");
        assert_eq!(
            p.validate_approval(&changed, Some(&approval)),
            Err(ValidationError::ScriptHashMismatch)
        );
    }
    #[test]
    fn unattended_promptable_evidence_fails_closed() {
        let r = RunnerRequest {
            unattended: true,
            ..request()
        };
        let p = plan(
            &r,
            vec![Evidence::inferred(Capability::Network, "heuristic")],
        );
        assert_eq!(p.policy().decision(), Decision::Deny);
        assert!(p.approval_required());
    }
    #[test]
    fn approval_cannot_override_policy_denial() {
        let cases = [
            (
                RunnerRequest {
                    unattended: true,
                    ..request()
                },
                vec![Evidence::inferred(Capability::Network, "heuristic")],
            ),
            (
                request(),
                vec![Evidence::ambiguous(Capability::Network, "unclear")],
            ),
            (
                RunnerRequest {
                    os: "plan9".into(),
                    ..request()
                },
                vec![],
            ),
        ];
        for (r, evidence) in cases {
            let p = plan(&r, evidence);
            assert_eq!(p.policy().decision(), Decision::Deny);
            assert_eq!(
                p.validate_approval(&r, Some(&Approval::for_request(&r))),
                Err(ValidationError::PolicyDenied)
            );
        }
    }

    #[test]
    fn unsupported_os_is_explicit_and_not_containment() {
        let r = RunnerRequest {
            os: "plan9".into(),
            ..request()
        };
        let p = plan(&r, vec![]);
        assert_eq!(p.policy().decision(), Decision::Deny);
        assert!(p.policy().reasons().contains(&ReasonCode::UnsupportedOs));
        assert!(!p.containment_available());
    }
}
