use super::*;
use crate::application::outcome::{LifecycleIdentity, LifecycleReport};

fn identity(
    origin: &str,
    peer: &tapid_core::PeerContext,
    platform: &tapid_core::PlatformContext,
) -> LifecycleIdentity {
    let package = tapid_lockfile::LockedPackage::new_with_context_and_provenance(
        origin,
        "plugin",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "0".repeat(64)),
        (peer, platform),
        tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    LifecycleIdentity::new(&package.key(), &package)
}
fn simple_identity() -> LifecycleIdentity {
    identity(
        "https://registry.npmjs.org",
        &Default::default(),
        &tapid_core::PlatformContext::new(None, None, None).unwrap(),
    )
}

#[test]
fn lifecycle_details_are_bounded_with_exact_omissions_without_stopping_checks() {
    let mut outcome = OperationOutcome::unchanged(std::path::Path::new("project"));
    let mut report = LifecycleReport::new();
    report.begin(1025);
    for _ in 0..1025 {
        let package = report.package(simple_identity());
        report.discovered(package, true);
        let hook = report.hook(package, "install");
        report.authorization(hook, "not_approved");
        report.disposition(hook, "skipped", Some("default_denial"));
        if report.inspected_packages == 1024 {
            assert_eq!(report.packages.len(), 1024);
            assert_eq!(report.omitted_packages, 0);
            let boundary = lifecycle_report(&report);
            assert_eq!(boundary["details_complete"], true);
        }
        outcome.lifecycle_warning(Warning::DependencyLifecycleHookSkipped {
            package: Box::new(simple_identity()),
            hook: "install".into(),
        });
    }
    report.finish();
    assert_eq!(report.packages.len(), 1024);
    assert_eq!(outcome.warnings.len(), 1024);
    outcome.lifecycle = Some(report);
    let result = envelope("install", "success", Some(&outcome));
    assert_eq!(result["lifecycle"]["coverage"], "complete");
    assert_eq!(result["lifecycle"]["inspected_packages"], 1025);
    assert_eq!(result["lifecycle"]["hooks_skipped"], 1025);
    assert_eq!(result["lifecycle"]["omitted_packages"], 1);
    assert_eq!(result["lifecycle"]["details_complete"], false);
    assert_eq!(result["warning_details_omitted"], 1);
    assert_eq!(result["warning_details_complete"], false);
    assert_eq!(
        result["warnings"],
        json!(["DEPENDENCY_LIFECYCLE_HOOK_SKIPPED"])
    );
}

#[test]
fn lifecycle_identity_preserves_origin_peers_and_platform_and_sorts_before_display() {
    let peer =
        tapid_core::PeerContext::default().with("react".parse().unwrap(), "1.0.0".parse().unwrap());
    let none = tapid_core::PlatformContext::new(None, None, None).unwrap();
    let linux = tapid_core::PlatformContext::new(Some("linux"), Some("x64"), None).unwrap();
    let identities = vec![
        simple_identity(),
        identity("https://other.test", &Default::default(), &none),
        identity("https://registry.npmjs.org", &peer, &none),
        identity("https://registry.npmjs.org", &Default::default(), &linux),
    ];
    let mut outcome = OperationOutcome::unchanged(std::path::Path::new("project"));
    let mut report = LifecycleReport::new();
    report.begin(4);
    for identity in identities.into_iter().rev() {
        let index = report.package(identity);
        report.discovered(index, true);
    }
    report.finish();
    outcome.lifecycle = Some(report);
    let result = envelope("install", "success", Some(&outcome));
    let packages = result["lifecycle"]["packages"].as_array().unwrap();
    let keys = packages
        .iter()
        .map(|p| p["identity"]["key"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        keys.iter().collect::<std::collections::BTreeSet<_>>().len(),
        4
    );
    assert!(keys.windows(2).all(|p| p[0] < p[1]));
    assert_eq!(packages[0]["identity"]["source"], "https://other.test");
    assert!(
        packages
            .iter()
            .any(|p| p["identity"]["peer_context"] == "name=react;version=1.0.0")
    );
    assert!(
        packages
            .iter()
            .any(|p| p["identity"]["platform_context"] == "os=linux;cpu=x64;libc=")
    );
}

#[test]
fn lifecycle_hook_details_are_bounded_and_sanitized_with_pointer_tracking() {
    let mut outcome = OperationOutcome::unchanged(std::path::Path::new("project"));
    let mut report = LifecycleReport::new();
    report.begin(1);
    let index = report.package(simple_identity());
    report.discovered(index, true);
    for _ in 0..1025 {
        report.hook(index, "install");
    }
    let record = &mut report.packages[0].hooks[0];
    record.hook = format!("\u{1b}{}", "界".repeat(2000));
    outcome.warnings.push(Warning::UpdateGraphChange {
        package: "https://user:fixture-secret@example.test/path?token=hidden#fragment".into(),
        reason: "\n".into(),
    });
    report.finish();
    outcome.lifecycle = Some(report);
    let result = envelope("install", "success", Some(&outcome));
    assert_eq!(
        result["lifecycle"]["packages"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1024
    );
    assert_eq!(result["lifecycle"]["packages"][0]["omitted_hooks"], 1);
    assert_eq!(result["lifecycle"]["details_complete"], false);
    let text = result.to_string();
    assert!(!text.contains("fixture-secret"));
    assert!(!text.contains("hidden"));
    assert!(!text.contains("fragment"));
    assert!(
        result["truncated_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p
                .as_str()
                .unwrap()
                .starts_with("/lifecycle/packages/0/hooks/"))
    );
}

#[test]
fn lifecycle_scalar_truncation_marks_details_incomplete_without_changing_coverage() {
    let mut outcome = OperationOutcome::unchanged(std::path::Path::new("project"));
    let mut report = LifecycleReport::new();
    report.begin(1);
    let package = report.package(simple_identity());
    report.discovered(package, true);
    report.hook(package, &"界".repeat(2000));
    report.finish();
    outcome.lifecycle = Some(report);
    let result = envelope("install", "success", Some(&outcome));
    assert_eq!(result["outcome"], "success");
    assert_eq!(result["lifecycle"]["coverage"], "complete");
    assert_eq!(result["lifecycle"]["omitted_packages"], 0);
    assert_eq!(result["lifecycle"]["packages"][0]["omitted_hooks"], 0);
    assert_eq!(result["lifecycle"]["details_complete"], false);
    assert_eq!(
        result["lifecycle"]["packages"][0]["hooks"][0]["hook"]
            .as_str()
            .unwrap()
            .len(),
        4095
    );
    assert_eq!(
        result["truncated_fields"],
        json!(["/lifecycle/packages/0/hooks/0/hook"])
    );
}
