//! Windows launch metadata shared by support checks and enforcement receipts.
use sha2::{Digest, Sha256};

pub(super) fn launch_mechanism(component: Option<&[u8]>, network: bool) -> String {
    let network = if network {
        "unrestricted IP: private user socket broker, DNS"
    } else {
        "persistent exact-SID WFP v4/v6 bind guard"
    };
    match component {
        Some(component) => format!(
            "LPAC/Job; registryRead; zero network SIDs; ACLs; stdio; DLL sha256:{:x}; app-IAT; exact READ_ATTRIBUTES ancestors; EOF stdin; {network}",
            Sha256::digest(component)
        ),
        None => format!(
            "verified LPAC token; registryRead; zero network SIDs; explicit ACLs; suspended Job assignment; bounded stdio; {network}"
        ),
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
                for network in [false, true] {
                    let mechanism = launch_mechanism(component, network);
                    let evidence = evidence_for_dimensions(&dimensions, &mechanism, &[]);
                    validate_dimension_evidence(&dimensions, &evidence).unwrap_or_else(|error| {
                        panic!(
                            "{}-byte Windows mechanism rejected: {error}",
                            mechanism.len()
                        )
                    });
                    if let Some(component) = component {
                        assert!(
                            mechanism.contains(&format!("sha256:{:x}", Sha256::digest(component)))
                        );
                    }
                    assert!(mechanism.contains(if network {
                        "socket broker"
                    } else {
                        "WFP v4/v6 bind guard"
                    }));
                }
            }
        }
    }
}
