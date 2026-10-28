use super::command_support::CliFixture;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::fs;

#[test]
fn lockfile_only_registry_selection_preserves_installation() {
    for installed in [false, true] {
        let scenario = CliFixture::new("lockfile-only-selection");
        let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1.0.0"},"scripts":{"postinstall":"must-not-run"}}"#;
        scenario.project.write("package.json", manifest).unwrap();
        let integrity = format!("sha512-{}", STANDARD.encode([1; 64]));
        // These artifacts deliberately do not exist: selection must only read metadata.
        let fixture = serde_json::json!({"packages":[
            {"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.0.0", "integrity":integrity, "artifact":"never-download", "dependencies":{"child":"^1.0.0"}},
            {"registry":"https://registry.npmjs.org", "name":"child", "version":"1.2.0", "integrity":integrity, "artifact":"never-download"}
        ]});
        scenario
            .project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        if installed {
            scenario
                .project
                .write("node_modules/sentinel", b"running application")
                .unwrap();
            scenario
                .project
                .write("node_modules/.bin/sentinel", b"existing shim")
                .unwrap();
            scenario
                .project
                .write(".tapid-managed", b"existing owner")
                .unwrap();
        }
        let output = scenario
            .command(&[
                "install",
                "--lockfile-only",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                "store",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read(scenario.project.path().join("package.json")).unwrap(),
            manifest
        );
        assert!(!scenario.project.path().join("store").exists());
        assert!(!scenario.project.path().join("never-download").exists());
        let lock: serde_json::Value =
            serde_json::from_slice(&fs::read(scenario.project.path().join("tapid.lock")).unwrap())
                .unwrap();
        assert_eq!(lock["lockfileVersion"], 11);
        let packages = lock["packages"].as_object().unwrap();
        assert_eq!(packages.len(), 2);
        let mut selections = packages
            .values()
            .map(|p| (p["name"].as_str().unwrap(), p["version"].as_str().unwrap()))
            .collect::<Vec<_>>();
        selections.sort();
        assert_eq!(selections, [("child", "1.2.0"), ("parent", "1.0.0")]);
        if installed {
            assert_eq!(
                fs::read(scenario.project.path().join("node_modules/sentinel")).unwrap(),
                b"running application"
            );
            assert_eq!(
                fs::read(scenario.project.path().join("node_modules/.bin/sentinel")).unwrap(),
                b"existing shim"
            );
            assert_eq!(
                fs::read(scenario.project.path().join(".tapid-managed")).unwrap(),
                b"existing owner"
            );
        } else {
            assert!(!scenario.project.path().join("node_modules").exists());
            assert!(!scenario.project.path().join(".tapid-managed").exists());
        }
        assert!(
            !scenario
                .project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists()
        );
    }
}
