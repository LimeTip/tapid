//! Profile metadata travels with the staged layout; it is not source authority.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, io::Write, path::Path};
use tapid_lockfile::{InstallProfile, NativeSelection};

const RECORD: &str = "node_modules/.tapid-profile.json";
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstalledProfile {
    version: u32,
    selection_digest: String,
    target: String,
    generation: String,
    include_dev: bool,
    include_optional: bool,
    materialized_keys: BTreeSet<String>,
}

fn selection_digest(selection: &NativeSelection) -> Result<String, String> {
    Ok(format!(
        "sha256-{:x}",
        Sha256::digest(selection.to_json().map_err(|e| e.to_string())?.as_bytes())
    ))
}

// Kept internal until the schema-11 application activation path is integrated.
#[allow(dead_code)]
pub(crate) fn stage_profile(
    stage: &Path,
    selection: &NativeSelection,
    profile: InstallProfile,
    target: &str,
    generation: &str,
) -> Result<(), String> {
    if target.is_empty() || generation.is_empty() {
        return Err("installed profile requires target and generation".into());
    }
    let record = InstalledProfile {
        version: 1,
        selection_digest: selection_digest(selection)?,
        target: target.into(),
        generation: generation.into(),
        include_dev: profile.include_dev,
        include_optional: profile.include_optional,
        materialized_keys: selection
            .materialized_keys(profile)
            .map_err(|e| e.to_string())?,
    };
    let bytes = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("installed profile exceeds metadata limit".into());
    }
    let directory = fs::symlink_metadata(stage.join("node_modules")).map_err(|e| e.to_string())?;
    if !directory.is_dir() || directory.file_type().is_symlink() {
        return Err("installed profile requires a staged regular directory".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.join(RECORD))
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    super::activation::sync_directory(&stage.join("node_modules"))
}

/// Validates layout description only, never source-tree or derived-output integrity.
#[allow(dead_code)]
pub(crate) fn load_profile(
    project: &Path,
    selection: &NativeSelection,
    target: &str,
) -> Result<InstallProfile, String> {
    super::activation::validate_managed_node_modules(project)?;
    let bytes =
        super::read::read_bounded_file(&project.join("node_modules/.tapid-generation"), 128)
            .map_err(|e| format!("cannot read layout generation: {e:?}"))?;
    let generation = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    validate_profile(project, selection, target, generation)
}

/// Validates layout description only, never source-tree or derived-output integrity.
#[allow(dead_code)]
pub(crate) fn validate_profile(
    project: &Path,
    selection: &NativeSelection,
    target: &str,
    generation: &str,
) -> Result<InstallProfile, String> {
    let directory =
        fs::symlink_metadata(project.join("node_modules")).map_err(|e| e.to_string())?;
    if !directory.is_dir() || directory.file_type().is_symlink() {
        return Err("installed profile requires a regular layout directory".into());
    }
    let bytes = super::read::read_bounded_file(&project.join(RECORD), MAX_BYTES)
        .map_err(|e| format!("cannot read installed profile: {e:?}"))?;
    let record: InstalledProfile = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let profile = InstallProfile {
        include_dev: record.include_dev,
        include_optional: record.include_optional,
    };
    if record.version != 1
        || record.target != target
        || record.generation != generation
        || target.is_empty()
        || generation.is_empty()
        || record.selection_digest != selection_digest(selection)?
        || record.materialized_keys
            != selection
                .materialized_keys(profile)
                .map_err(|e| e.to_string())?
    {
        return Err(
            "installed profile does not match selection, target or layout generation".into(),
        );
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_lockfile::{InstallProfile, NativeSelection};

    #[test]
    fn active_profile_uses_activation_generation_and_rejects_forgery() {
        use crate::filesystem::activation::{ActivationLock, activate_node_modules_with_preflight};
        let project = tapid_test_support::TempProject::new("active-profile").unwrap();
        let selection = NativeSelection::new(
            "0".into(),
            "0".into(),
            format!("sha256-{}", "a".repeat(64)),
            Default::default(),
            vec![],
            Default::default(),
        )
        .unwrap();
        let lock = ActivationLock::acquire(project.path()).unwrap();
        let stage = lock.create_stage(project.path()).unwrap();
        fs::create_dir_all(stage.join("node_modules")).unwrap();
        stage_profile(
            &stage,
            &selection,
            InstallProfile::production(),
            "linux-x64",
            lock.owner_line(),
        )
        .unwrap();
        assert!(load_profile(&stage, &selection, "linux-x64").is_err());
        activate_node_modules_with_preflight(project.path(), &stage, &lock, false, || Ok(()))
            .unwrap();
        assert_eq!(
            load_profile(project.path(), &selection, "linux-x64").unwrap(),
            InstallProfile::production()
        );
        let path = project.path().join(RECORD);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["generation"] = serde_json::json!("forged");
        let forged = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &forged).unwrap();
        assert!(load_profile(project.path(), &selection, "linux-x64").is_err());
        assert_eq!(fs::read(&path).unwrap(), forged);
    }

    #[test]
    fn staged_profile_binds_selection_target_keys_and_generation() {
        let project = tapid_test_support::TempProject::new("installed-profile").unwrap();
        let selection = NativeSelection::new(
            "0".into(),
            "0".into(),
            format!("sha256-{}", "a".repeat(64)),
            Default::default(),
            vec![],
            Default::default(),
        )
        .unwrap();
        let stage = project.path().join("stage");
        std::fs::create_dir_all(stage.join("node_modules")).unwrap();
        stage_profile(
            &stage,
            &selection,
            InstallProfile::production(),
            "linux-x64",
            "generation-1",
        )
        .unwrap();
        validate_profile(&stage, &selection, "linux-x64", "generation-1").unwrap();
        let changed_selection = NativeSelection::new(
            "0".into(),
            "0".into(),
            format!("sha256-{}", "b".repeat(64)),
            Default::default(),
            vec![],
            Default::default(),
        )
        .unwrap();
        assert!(validate_profile(&stage, &changed_selection, "linux-x64", "generation-1").is_err());
        assert!(
            stage_profile(
                &stage,
                &selection,
                InstallProfile::default(),
                "linux-x64",
                "generation-2"
            )
            .is_err()
        );
        assert!(validate_profile(&stage, &selection, "darwin-arm64", "generation-1").is_err());
        assert!(validate_profile(&stage, &selection, "linux-x64", "generation-2").is_err());
        let path = stage.join("node_modules/.tapid-profile.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["materializedKeys"] = serde_json::json!(["forged"]);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(validate_profile(&stage, &selection, "linux-x64", "generation-1").is_err());
    }

    #[test]
    fn profile_activation_and_failed_preflight_preserve_coherent_layout() {
        use crate::filesystem::activation::{ActivationLock, activate_node_modules_with_preflight};
        let project = tapid_test_support::TempProject::new("profile-activation").unwrap();
        let selection = NativeSelection::new(
            "0".into(),
            "0".into(),
            format!("sha256-{}", "a".repeat(64)),
            Default::default(),
            vec![],
            Default::default(),
        )
        .unwrap();
        let lock = ActivationLock::acquire(project.path()).unwrap();
        for (generation, profile) in [
            ("full-1", InstallProfile::default()),
            ("production-2", InstallProfile::production()),
            ("full-3", InstallProfile::default()),
        ] {
            let stage = lock.create_stage(project.path()).unwrap();
            fs::create_dir_all(stage.join("node_modules")).unwrap();
            stage_profile(&stage, &selection, profile, "linux-x64", generation).unwrap();
            if generation == "production-2" {
                assert!(
                    activate_node_modules_with_preflight(
                        project.path(),
                        &stage,
                        &lock,
                        false,
                        || Err("injected preflight failure".into())
                    )
                    .is_err()
                );
                assert_eq!(
                    validate_profile(project.path(), &selection, "linux-x64", "full-1").unwrap(),
                    InstallProfile::default()
                );
            }
            activate_node_modules_with_preflight(project.path(), &stage, &lock, false, || Ok(()))
                .unwrap();
            assert_eq!(
                validate_profile(project.path(), &selection, "linux-x64", generation).unwrap(),
                profile
            );
        }
    }
}
