//! Windows launch metadata shared by support checks and enforcement receipts.
use sha2::{Digest, Sha256};

pub(super) fn launch_mechanism(component: Option<&[u8]>) -> String {
    match component {
        Some(component) => format!(
            "LPAC, registryRead, no net caps; Job; ACLs; bounded stdio; DLL sha256:{:x}; app-IAT children; exact ancestor READ_ATTRIBUTES handles, not inherited; EOF stdin; NT realpath; no ancestor ACL writes",
            Sha256::digest(component)
        ),
        None => "verified Less-Privileged AppContainer token with registryRead runtime capability and no network capabilities, explicit path ACL grants with validated existing write targets, suspended Job Object assignment, and bounded standard-handle capture".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssuranceLevel, FilesystemPolicy, SandboxMode, SandboxPolicy};
    use crate::execution::{
        EnforcementDimensions, evidence_for_dimensions, validate_dimension_evidence,
    };

    #[test]
    fn windows_launch_metadata_passes_checked_evidence_validation() {
        for assurance in [AssuranceLevel::Restricted, AssuranceLevel::ManagedTree] {
            let policy = SandboxPolicy::new_with_assurance(
                SandboxMode::Required,
                assurance,
                FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
                false,
                vec![],
                true,
                Default::default(),
            )
            .unwrap();
            let dimensions = EnforcementDimensions::requested_by(&policy);
            for component in [None, Some(b"test compatibility DLL".as_slice())] {
                let mechanism = launch_mechanism(component);
                let evidence = evidence_for_dimensions(&dimensions, &mechanism, &[]);
                validate_dimension_evidence(&dimensions, &evidence).unwrap_or_else(|error| {
                    panic!(
                        "{}-byte Windows mechanism rejected: {error}",
                        mechanism.len()
                    )
                });
                if let Some(component) = component {
                    assert!(mechanism.contains(&format!("sha256:{:x}", Sha256::digest(component))));
                }
            }
        }
    }
}
