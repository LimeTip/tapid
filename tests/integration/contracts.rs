use std::io::Cursor;
use std::str::FromStr;

use serde_json::json;
use tapid_archive::{ArchiveEntry, ValidationLimits, validate_entries};
use tapid_core::{PackageInstanceId, PeerContext, PlatformContext};
use tapid_linker::{
    ManagedRoot, MaterializationInput, PackageInstance, VerifiedTreeReference, plan_materialization,
};
use tapid_manifest::PackageManifest;
use tapid_policy::{Capability, Decision, Evidence, PolicyDecision, ReasonCode};
use tapid_publish::{PackageSource, Publisher};
use tapid_registry_client::{RawRegistrySnapshot, RegistrySnapshot};
use tapid_resolver::{Dependency, Requirement, ResolutionOptions, resolve};
use tapid_runner::{Approval, RunnerRequest, ValidationError, plan as runner_plan};
use tapid_signatures::{TrustEnvelope, VerificationError};
use tapid_store::{IngestResult, Store};
use tapid_test_support::TempProject;

#[test]
fn contracts_flow_from_runtime_fixture_to_deterministic_plan() {
    let project = TempProject::new("contracts-flow").unwrap();
    project
        .write(
            "package.json",
            br#"{
      "name": "demo-app",
      "version": "1.0.0",
      "dependencies": {"demo-lib": "^1.0.0"},
      "scripts": {"build": "echo build"}
    }"#,
        )
        .unwrap();
    project
        .write("src/index.js", b"export const answer = 42;\n")
        .unwrap();
    project.write("ignored.secret", b"do not publish").unwrap();

    // Manifest parsing is the evidence-producing input boundary.
    let manifest = PackageManifest::parse(
        &std::fs::read_to_string(project.path().join("package.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.name().as_str(), "demo-app");
    assert_eq!(manifest.dependencies()["demo-lib"], "^1.0.0");
    assert_eq!(manifest.scripts()["build"], "echo build");

    // Registry bytes are a fixture, then normalized into the typed snapshot.
    let registry = "https://registry.example.test".parse().unwrap();
    let raw: RawRegistrySnapshot = serde_json::from_slice(
        br#"{"registry":"https://registry.example.test/","packages":[
          {"name":"demo-lib","version":"1.0.0"},
          {"name":"demo-lib","version":"1.4.0"},
          {"name":"demo-lib","version":"2.0.0"}
        ]}"#,
    )
    .unwrap();
    let snapshot = RegistrySnapshot::normalize(raw).unwrap();
    assert_eq!(snapshot.registry(), &registry);
    assert_eq!(snapshot.candidates(&"demo-lib".parse().unwrap()).len(), 3);

    let dependency = Dependency::new(
        registry.clone(),
        "demo-lib".parse().unwrap(),
        Requirement::from_str(manifest.dependencies()["demo-lib"].as_str()).unwrap(),
    );
    let resolution = resolve(
        &[dependency],
        &[snapshot.clone()],
        ResolutionOptions::default(),
    )
    .unwrap();
    assert_eq!(resolution.selected[0].version.to_string(), "1.4.0");

    // The packed bytes are both an archive fixture and the store's digest source.
    let packed = tapid_publish::pack(
        &PackageSource::new(project.path(), "1.0.0")
            .with_exclusions(tapid_publish::ExclusionRules::new().exclude("ignored.secret")),
    )
    .unwrap();
    let entries = packed
        .manifest
        .files
        .iter()
        .map(|file| ArchiveEntry::file(file.path.clone(), file.size));
    validate_entries(entries, ValidationLimits::default()).unwrap();
    assert!(
        validate_entries(
            [ArchiveEntry::file("../escape", 1)],
            ValidationLimits::default()
        )
        .is_err()
    );

    let store_root = project.path().join("store");
    let store = Store::new(&store_root);
    let ingested = store
        .ingest(packed.digest(), Cursor::new(&packed.bytes))
        .unwrap();
    assert!(matches!(ingested, IngestResult::Activated(_)));
    assert_eq!(
        std::fs::read(store.artifact_path(packed.digest())).unwrap(),
        packed.bytes
    );
    assert!(matches!(
        store.ingest(packed.digest(), Cursor::new(b"different")),
        Ok(IngestResult::AlreadyPresent(_))
    ));

    // Link planning consumes verified, absolute runtime paths and is deterministic.
    let selected = &resolution.selected[0];
    let instance = PackageInstance {
        id: PackageInstanceId::new(
            selected.registry.clone(),
            selected.name.clone(),
            selected.version,
        ),
        peer_context: PeerContext::default(),
        platform_context: PlatformContext::new(Some("linux"), Some("x86_64"), Some("gnu")).unwrap(),
        tree: VerifiedTreeReference::new(
            packed.digest().as_str(),
            store.artifact_path(packed.digest()),
        )
        .unwrap(),
    };
    let root = ManagedRoot::new(project.path().join("managed")).unwrap();
    let plan_a = plan_materialization(
        root.clone(),
        MaterializationInput {
            instances: vec![instance.clone()],
        },
    )
    .unwrap();
    let plan_b = plan_materialization(
        root,
        MaterializationInput {
            instances: vec![instance],
        },
    )
    .unwrap();
    assert_eq!(plan_a, plan_b);
    assert_eq!(plan_a.entries.len(), 1);
    assert!(
        plan_a.entries[0]
            .target
            .starts_with(project.path().join("managed"))
    );

    // Evidence is retained as data; policy is a separate decision over that data.
    let evidence = vec![Evidence::declared(
        Capability::Network,
        "build script requests network",
    )];
    let evidence_only = PolicyDecision::new(Decision::Allow, vec![], evidence.clone());
    assert_eq!(evidence_only.decision(), Decision::Allow);
    assert_eq!(evidence_only.evidence(), evidence.as_slice());
    let request = RunnerRequest {
        artifact_digest: packed.digest().to_string(),
        script: " echo build\r\n".into(),
        unattended: false,
        os: "linux".into(),
    };
    let runner = runner_plan(&request, evidence.clone());
    assert_eq!(runner.policy.evidence(), evidence.as_slice());
    assert_eq!(runner.policy.decision(), Decision::Warn);
    assert!(
        runner
            .policy
            .reasons()
            .contains(&ReasonCode::DeclaredNetworkCapability)
    );
    assert!(runner.approval_required);
    assert_eq!(
        runner.validate_approval(&request, None),
        Err(ValidationError::MissingApproval)
    );
    assert!(
        runner
            .validate_approval(&request, Some(&Approval::for_request(&request)))
            .is_ok()
    );

    // Trust verification rejects unsigned artifacts even when their canonical bytes are valid.
    let envelope = TrustEnvelope::unsigned(
        "audit",
        packed.digest().to_string(),
        json!({"decision":"warn","evidence":["network"]}),
    );
    assert!(!envelope.signing_bytes().unwrap().is_empty());
    assert_eq!(envelope.verify(), Err(VerificationError::Unsigned));
}

#[test]
fn publishing_preview_is_runtime_derived_and_byte_deterministic() {
    let first = TempProject::new("publish-a").unwrap();
    let second = TempProject::new("publish-b").unwrap();
    for root in [first.path(), second.path()] {
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("package.json"), b"{}\n").unwrap();
        std::fs::write(root.join("lib/z.js"), b"z\n").unwrap();
        std::fs::write(root.join("lib/a.js"), b"a\n").unwrap();
    }
    let publisher = Publisher::new(());
    let a = publisher
        .preview(&PackageSource::new(first.path(), "3.2.1"))
        .unwrap();
    let b = publisher
        .preview(&PackageSource::new(second.path(), "3.2.1"))
        .unwrap();
    assert_eq!(a.artifact.bytes, b.artifact.bytes);
    assert_eq!(a.artifact.digest, b.artifact.digest);
    assert_eq!(
        a.artifact.manifest.paths().collect::<Vec<_>>(),
        vec!["lib/a.js", "lib/z.js", "package.json"]
    );
}

#[test]
fn unsigned_and_tampered_trust_artifacts_never_become_valid() {
    let mut envelope = TrustEnvelope::unsigned("audit", "sha256-artifact", json!({"ok":true}));
    assert_eq!(envelope.verify(), Err(VerificationError::Unsigned));
    envelope.signature = Some(tapid_signatures::DetachedSignature {
        algorithm: "future".into(),
        key_id: "test".into(),
        subject: "other".into(),
        artifact_digest: "sha256-artifact".into(),
        value: "not-a-signature".into(),
    });
    assert_eq!(envelope.verify(), Err(VerificationError::SubjectMismatch));
}
