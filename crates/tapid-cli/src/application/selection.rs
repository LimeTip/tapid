//! Selection persistence without installed-layout or verified-store effects.
use super::{
    inputs::Inputs,
    outcome::{ErrorKind, OperationalError},
};
use std::{fs, path::Path};
use tapid_lockfile::Lockfile;

// Command integration remains gated on imported-lock handling and shared outcomes.
#[allow(dead_code)]
pub(super) fn persist_native(
    project: &Path,
    fixture: Option<&Path>,
) -> Result<crate::online::SelectionCandidate, OperationalError> {
    let project = fs::canonicalize(project)
        .map_err(|e| OperationalError::from_source(ErrorKind::Project, e))?;
    // Recover metadata only before capturing evidence; activation recovery is forbidden.
    drop(crate::filesystem::activation::ActivationLock::acquire_metadata(&project)?);
    let target = project.join("package.json");
    let mut inputs = Inputs::capture(&project, &target)?;
    let manifest = inputs.manifest(&target).clone();
    inputs.capture_copied_files(&manifest)?;
    let previous = inputs
        .lock
        .as_deref()
        .map(|bytes| {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?;
            Lockfile::from_json(text).map_err(OperationalError::from)
        })
        .transpose()?;
    if let Some(lock) = &previous {
        lock.validate_selection(lock.root_manifest_digest())?;
    }
    let workspace = inputs.materialize(&inputs.config, None)?;
    let candidate = crate::online::prepare_selection(
        &project,
        &manifest,
        workspace,
        fixture,
        &inputs.config,
        previous.as_ref(),
        inputs.root_digest(),
    )?;
    inputs.publish_selection(&candidate.lock, None)?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_selection_failure_preserves_previous_outputs() {
        let project = tapid_test_support::TempProject::new("selection-entrypoint-failure").unwrap();
        let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1"}}"#;
        project.write("package.json", manifest).unwrap();
        project.write("node_modules/sentinel", b"running").unwrap();
        let fixture = project
            .write(
                "registry.json",
                include_bytes!("../../tests/fixtures/npm-import/registry.json"),
            )
            .unwrap();
        super::persist_native(project.path(), Some(&fixture)).unwrap();
        let before = std::fs::read(project.path().join("tapid.lock")).unwrap();
        project
            .write("registry.json", br#"{"packages":[]}"#)
            .unwrap();
        assert!(super::persist_native(project.path(), Some(&fixture)).is_err());
        assert_eq!(
            std::fs::read(project.path().join("tapid.lock")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running"
        );
        assert!(!crate::filesystem::lifecycle_journal::has_pending(project.path()).unwrap());
        assert!(!project.path().join("store").exists());
    }

    #[test]
    fn native_selection_entrypoint_preserves_installed_application() {
        let project = tapid_test_support::TempProject::new("selection-entrypoint").unwrap();
        let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1"}}"#;
        project.write("package.json", manifest).unwrap();
        project.write("node_modules/sentinel", b"running").unwrap();
        let fixture = project
            .write(
                "registry.json",
                include_bytes!("../../tests/fixtures/npm-import/registry.json"),
            )
            .unwrap();
        let candidate = super::persist_native(project.path(), Some(&fixture)).unwrap();
        assert!(candidate.updated_manifest.is_none());
        assert!(candidate.metadata_lookups > 0);
        let lock = candidate.lock;
        assert_eq!(lock.packages().len(), 2);
        assert!(
            lock.packages()
                .values()
                .all(|p| p.source_tree_receipt().is_none())
        );
        assert_eq!(
            std::fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running"
        );
        assert!(!project.path().join("store").exists());
        assert_eq!(
            tapid_lockfile::Lockfile::from_json(
                &std::fs::read_to_string(project.path().join("tapid.lock")).unwrap()
            )
            .unwrap(),
            lock
        );
    }
}
