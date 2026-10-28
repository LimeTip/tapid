//! Source-only preparation; project inputs are observed, never locked or recovered.
use super::{
    observed_inputs::Inputs,
    outcome::{ErrorKind, OperationalError},
    replay,
};
use crate::online;
use std::{collections::BTreeSet, fs, path::Path};
use tapid_lockfile::Lockfile;
use tapid_store::Store;

pub(crate) struct Report {
    pub result: Result<(), OperationalError>,
    pub logical_instances: usize,
    pub unique_trees: Option<usize>,
    pub available: Option<usize>,
    pub observations: online::PreparationObservations,
    pub newly_published: usize,
    pub reason: Option<&'static str>,
    pub entries: Vec<Entry>,
}

pub(crate) struct Entry {
    pub key: String,
    pub digest: Option<String>,
    pub state: &'static str,
}

impl Report {
    pub fn count(&self, state: &str) -> usize {
        self.entries
            .iter()
            .filter(|entry| self.entry_state(entry) == state)
            .count()
    }
    pub fn entry_state(&self, entry: &Entry) -> &'static str {
        if entry.state != "unprocessed" {
            return entry.state;
        }
        if let Some(state) = self.observations.failures.get(&entry.key) {
            return state;
        }
        entry
            .digest
            .as_ref()
            .and_then(|digest| self.observations.trees.get(digest))
            .copied()
            .unwrap_or("unprocessed")
    }
}

pub(crate) fn run(project: &Path, root: &Path, fixture: Option<&Path>) -> Report {
    inspect(project, root, fixture, |_, _| {})
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Checkpoint {
    Prepared,
    Committed,
    Verified,
}

fn inspect(
    project: &Path,
    root: &Path,
    fixture: Option<&Path>,
    mut checkpoint: impl FnMut(Checkpoint, &Store),
) -> Report {
    let mut report = Report {
        result: Ok(()),
        logical_instances: 0,
        unique_trees: None,
        available: None,
        observations: Default::default(),
        newly_published: 0,
        reason: None,
        entries: Vec::new(),
    };
    report.result = prepare(project, root, fixture, &mut report, &mut checkpoint);
    report
}

fn prepare(
    project: &Path,
    root: &Path,
    fixture: Option<&Path>,
    report: &mut Report,
    checkpoint: &mut impl FnMut(Checkpoint, &Store),
) -> Result<(), OperationalError> {
    let project = fs::canonicalize(project)
        .map_err(|e| OperationalError::from_source(ErrorKind::Project, e))?;
    if crate::filesystem::lifecycle_journal::has_pending(&project)
        .map_err(|e| OperationalError::new(ErrorKind::Recovery, e))?
    {
        return Err(OperationalError::new(
            ErrorKind::Recovery,
            "pending project recovery blocks cache fetch; run install to recover",
        ));
    }
    let inputs = Inputs::capture_sources(&project, &project.join("package.json"))?;
    let bytes = inputs.lock.as_deref().ok_or_else(|| {
        OperationalError::new(
            ErrorKind::LockfileMissing,
            "cache fetch requires tapid.lock",
        )
    })?;
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?;
    if value["lockfileVersion"] == 8 {
        let imported = tapid_lockfile::ImportedNpmLockfile::from_json(
            std::str::from_utf8(bytes)
                .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?,
        )
        .map_err(|e| OperationalError::new(ErrorKind::Lockfile, e))?;
        report.entries = imported
            .graph()
            .map_err(|e| OperationalError::new(ErrorKind::Lockfile, e))?
            .packages
            .keys()
            .map(|key| Entry {
                key: key.clone(),
                digest: None,
                state: "unsupported",
            })
            .collect();
        report.logical_instances = report.entries.len();
        report.reason = Some("CACHE_FETCH_IMPORTED_LOCK_UNSUPPORTED");
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "cache fetch does not support imported schema-8 locks, including locks with receipts; frozen install can establish receipts but activates an installation",
        ));
    }
    if let Some(packages) = value["packages"].as_object() {
        report.entries = packages
            .iter()
            .map(|(key, package)| Entry {
                key: key.clone(),
                digest: package["treeDigest"].as_str().map(str::to_owned),
                state: if package["treeDigest"].as_str().is_none_or(str::is_empty) {
                    "unsupported"
                } else {
                    "unprocessed"
                },
            })
            .collect();
        report.logical_instances = report.entries.len();
        if report.count("unsupported") > 0 {
            report.reason = Some("CACHE_FETCH_MISSING_RECEIPT");
            return Err(OperationalError::new(
                ErrorKind::InvalidRequest,
                "cache fetch requires existing tree receipts and never writes receipts; frozen install may establish receipts but activates an installation",
            ));
        }
    }
    let lock = Lockfile::from_json(
        std::str::from_utf8(bytes)
            .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?,
    )?;
    let packages = lock.packages_typed()?;
    let trees = packages
        .iter()
        .map(|(_, package)| package.tree_digest().to_owned())
        .collect::<BTreeSet<_>>();
    report.unique_trees = Some(trees.len());
    report.entries = packages
        .iter()
        .map(|(key, package)| Entry {
            key: key.to_string(),
            digest: Some(package.tree_digest().to_owned()),
            state: "unprocessed",
        })
        .collect();
    if lock.root_manifest_digest() != inputs.root_digest() {
        lock.validate_replay(inputs.root_digest())?;
    }
    online::validate_locked_routes(&lock, &inputs.config)?;
    online::validate_locked_files(&project, &lock)?;
    let workspace = inputs.materialize(&inputs.config, None)?;
    if !online::workspace_matches_lock(&workspace, &lock) {
        return Err(OperationalError::new(
            ErrorKind::LockManifestMismatch,
            "workspace inputs differ from tapid.lock",
        ));
    }
    let manifest = inputs.manifest(&project.join("package.json"));
    let dependencies =
        online::resolved_workspace_registry_dependencies(manifest, &workspace, &inputs.config)?;
    replay::validate_workspace_dependency_edges(&workspace, &dependencies, &lock)?;
    replay::validate_root_bindings(&lock, manifest, &inputs.config)?;
    for ((key, package), entry) in packages.into_iter().zip(&mut report.entries) {
        if !online::locked_platform_matches(&key)? {
            report.reason = Some("CACHE_FETCH_TARGET_UNSUPPORTED");
            entry.state = "unsupported";
        }
        if let Err(error) = online::locked_artifact_url(&key, package) {
            if error.kind == ErrorKind::Lockfile {
                report.reason = Some("CACHE_FETCH_MISSING_PIN");
                entry.state = "unsupported";
            } else {
                entry.state = "failed";
                return Err(error);
            }
        }
        if key
            .source
            .registry()
            .is_some_and(|registry| registry.as_str() != "https://jsr.io")
        {
            inputs.config.route(key.name.as_str())?;
        }
    }
    if report.count("unsupported") > 0 {
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "cache fetch requires exact pinned archive URLs and compatible target contexts",
        ));
    }
    lock.validate_replay(inputs.root_digest())?;
    let store = Store::new(root);
    let transaction = online::hydrate_locked(
        &lock,
        &store,
        &inputs.config,
        fixture,
        &project,
        &mut report.observations,
    )?;
    checkpoint(Checkpoint::Prepared, &store);
    inputs.recheck()?;
    online::validate_locked_files(&project, &lock)?;
    if let Some(transaction) = transaction {
        let publication = transaction.publish()?;
        let introduced = publication.newly_published_trees();
        publication.commit()?;
        report.newly_published = introduced;
    }
    checkpoint(Checkpoint::Committed, &store);
    // commit consumes the exclusive publication guard before shared acquisition.
    let _guard = store.read_guard()?;
    for digest in trees {
        store.verified_tree_path(&digest.parse().expect("validated digest"))?;
    }
    inputs.recheck()?;
    online::validate_locked_files(&project, &lock)?;
    report.available = report.unique_trees;
    checkpoint(Checkpoint::Verified, &store);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    pub(super) fn project() -> (tapid_test_support::TempProject, std::path::PathBuf) {
        let project = tapid_test_support::TempProject::new("fetch-interleaving").unwrap();
        let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"1.0.0"}}"#;
        project.write("package.json", manifest).unwrap();
        let fixture = include_str!("../../tests/fixtures/npm-import/registry.json");
        let records: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let bytes = STANDARD
            .decode(
                records["packages"][0]["artifact"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("base64:")
                    .unwrap(),
            )
            .unwrap();
        let source = project.path().join("source");
        tapid_archive::extract_to(
            &bytes,
            tapid_archive::ArchiveFormat::TarGz,
            &source,
            Default::default(),
        )
        .unwrap();
        let mut lock = Lockfile::new(&crate::filesystem::atomic::digest_bytes(manifest)).unwrap();
        let mut package = LockedPackage::new_with_provenance(
            "https://registry.npmjs.org",
            "parent",
            "1.0.0",
            records["packages"][0]["integrity"].as_str().unwrap(),
            &tapid_archive::canonical_tree_digest(&source).unwrap(),
            RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap();
        package
            .set_artifact_url("https://registry.npmjs.org/parent.tgz")
            .unwrap();
        let key = package.key();
        lock.insert_package(package).unwrap();
        lock.set_roots([key.clone()]).unwrap();
        lock.set_root_bindings(std::collections::BTreeMap::from([("parent".into(), key)]))
            .unwrap();
        project
            .write("tapid.lock", lock.to_json().unwrap().as_bytes())
            .unwrap();
        project.write("registry.json", fixture.as_bytes()).unwrap();
        let path = project.path().join("registry.json");
        (project, path)
    }

    #[test]
    fn competing_fetch_does_not_inflate_publication_counts() {
        let (project, fixture) = project();
        let store = project.path().join("store");
        let report = inspect(project.path(), &store, Some(&fixture), |stage, _| {
            if stage == Checkpoint::Prepared {
                let winner = run(project.path(), &store, Some(&fixture));
                assert!(winner.result.is_ok());
                assert_eq!(winner.newly_published, 1);
            }
        });
        assert!(report.result.is_ok());
        assert_eq!(report.observations.count("fetched_verified"), 1);
        assert_eq!(report.newly_published, 0);
        assert_eq!(report.available, Some(1));
        assert!(!project.path().join(".tapid-activation.lock").exists());
    }

    #[test]
    fn clean_between_commit_and_inspection_prevents_success() {
        let (project, fixture) = project();
        let report = inspect(
            project.path(),
            &project.path().join("store"),
            Some(&fixture),
            |stage, store| {
                if stage == Checkpoint::Committed {
                    store.clean_cache().unwrap();
                }
            },
        );
        assert_eq!(
            report.result.err().unwrap().kind,
            ErrorKind::StoreUnavailable
        );
        assert_eq!(report.newly_published, 1);
        assert_eq!(report.available, None);
    }

    #[test]
    fn final_verification_guard_excludes_clean() {
        let (project, fixture) = project();
        let report = inspect(
            project.path(),
            &project.path().join("store"),
            Some(&fixture),
            |stage, store| {
                if stage == Checkpoint::Verified {
                    assert!(
                        matches!(store.clean_cache(), Err(tapid_store::IngestError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock)
                    );
                }
            },
        );
        assert!(report.result.is_ok());
    }

    #[test]
    fn fetch_prepares_while_a_reader_holds_the_store_and_publishes_after_release() {
        let (project, fixture) = project();
        let root = project.path().join("store");
        let store = Store::new(&root);
        let guard = store.read_guard().unwrap();
        let prepared = std::sync::Barrier::new(2);
        let release = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let fetch = scope.spawn(|| {
                inspect(project.path(), &root, Some(&fixture), |stage, _| {
                    if stage == Checkpoint::Prepared {
                        prepared.wait();
                        release.wait();
                    }
                })
            });
            prepared.wait();
            assert!(!root.join("trees").exists());
            drop(guard);
            release.wait();
            let report = fetch.join().unwrap();
            assert!(report.result.is_ok());
            assert_eq!(report.newly_published, 1);
            assert_eq!(report.available, Some(1));
        });
    }

    #[test]
    fn changed_project_input_discards_staged_sources_before_publication() {
        let (project, fixture) = project();
        let root = project.path().join("store");
        let report = inspect(project.path(), &root, Some(&fixture), |stage, _| {
            if stage == Checkpoint::Prepared {
                fs::write(project.path().join("package.json"), b"changed").unwrap();
            }
        });
        assert_eq!(report.result.err().unwrap().kind, ErrorKind::ProjectChanged);
        assert_eq!(report.newly_published, 0);
        assert_eq!(report.observations.count("fetched_verified"), 1);
        assert!(!root.join("trees").exists());
    }
}

#[cfg(test)]
mod interruption_tests {
    use super::*;
    #[test]
    fn interrupted_fetch_staging_never_activates_project_and_can_retry() {
        const ROOT: &str = "TAPID_TEST_FETCH_INTERRUPTION_ROOT";
        if let Some(root) = std::env::var_os(ROOT) {
            let root = std::path::PathBuf::from(root);
            inspect(
                &root,
                &root.join("store"),
                Some(&root.join("registry.json")),
                |stage, _| {
                    if stage == Checkpoint::Prepared {
                        std::process::exit(74);
                    }
                },
            );
            panic!("fetch interruption did not execute");
        }
        let (project, fixture) = super::tests::project();
        let locked = fs::read(project.path().join("tapid.lock")).unwrap();
        let manifest = fs::read(project.path().join("package.json")).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "application::cache_fetch::interruption_tests::interrupted_fetch_staging_never_activates_project_and_can_retry"])
            .env(ROOT, project.path()).output().unwrap();
        assert_eq!(output.status.code(), Some(74), "{output:?}");
        assert!(!project.path().join("store/trees").exists());
        assert!(!project.path().join("node_modules").exists());
        assert!(!project.path().join(".tapid-activation.lock").exists());
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), locked);
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        let report = run(
            project.path(),
            &project.path().join("store"),
            Some(&fixture),
        );
        assert!(report.result.is_ok());
        assert_eq!(report.available, Some(1));
    }
}
