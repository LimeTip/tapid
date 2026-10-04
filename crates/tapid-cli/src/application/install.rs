use crate::commands::manifest::read_manifest;
use crate::filesystem::activation::ActivationLock;
use crate::{online, package_spec};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};
use tapid_linker::{ManagedRoot, NamedLayoutInput, plan_named_layout};
use tapid_lockfile::Lockfile;
use tapid_manifest::PackageManifest;
use tapid_store::Store;

struct ManifestTransaction {
    path: PathBuf,
    original: Vec<u8>,
    committed: bool,
}

impl ManifestTransaction {
    fn begin(path: &Path) -> Result<Self, String> {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() {
            return Err("package.json must be a regular file".to_owned());
        }
        Ok(Self {
            path: path.to_owned(),
            original: fs::read(path).map_err(|error| error.to_string())?,
            committed: false,
        })
    }

    fn write(&self, contents: &str) -> Result<(), String> {
        fs::write(&self.path, contents).map_err(|error| error.to_string())
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for ManifestTransaction {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::write(&self.path, &self.original);
        }
    }
}

pub(crate) struct InstallReport {
    pub(crate) package_count: usize,
    pub(crate) replayed: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum InstallMode {
    Online,
    Offline,
    Frozen,
}

fn default_store_root_for<F>(platform: &str, mut environment: F) -> Result<PathBuf, String>
where
    F: FnMut(&str) -> Option<OsString>,
{
    fn absolute(value: OsString, variable: &str, platform: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(value);
        let target_absolute = if platform == "windows" {
            path.to_str().is_some_and(|value| {
                let bytes = value.as_bytes();
                (bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'\\' | b'/'))
                    || value.starts_with("\\\\")
            })
        } else {
            path.to_string_lossy().starts_with('/')
        };
        if !target_absolute {
            return Err(format!("{variable} must contain an absolute path"));
        }
        Ok(path)
    }

    let root = match platform {
        "macos" => absolute(
            environment("HOME").ok_or("HOME is required to locate Tapid's verified store")?,
            "HOME",
            platform,
        )?
        .join("Library/Caches"),
        "windows" => absolute(
            environment("LOCALAPPDATA")
                .ok_or("LOCALAPPDATA is required to locate Tapid's verified store")?,
            "LOCALAPPDATA",
            platform,
        )?,
        _ => {
            if let Some(cache) = environment("XDG_CACHE_HOME") {
                absolute(cache, "XDG_CACHE_HOME", platform)?
            } else {
                absolute(
                    environment("HOME")
                        .ok_or("HOME is required to locate Tapid's verified store")?,
                    "HOME",
                    platform,
                )?
                .join(".cache")
            }
        }
    };
    Ok(root.join("tapid/store"))
}

fn default_store_root() -> Result<PathBuf, String> {
    default_store_root_for(std::env::consts::OS, |name| std::env::var_os(name))
}

pub(crate) fn run(
    project_dir: &Path,
    package: Option<&str>,
    store_root: Option<&Path>,
    mode: InstallMode,
    registry_fixture: Option<&Path>,
    allow_unverified_registry_artifacts: bool,
    report_replay_progress: impl FnMut(usize, usize),
) -> Result<InstallReport, String> {
    run_with_manifest(
        project_dir,
        None,
        package,
        store_root,
        mode,
        registry_fixture,
        allow_unverified_registry_artifacts,
        report_replay_progress,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_with_manifest(
    project_dir: &Path,
    manifest_override: Option<&PackageManifest>,
    package: Option<&str>,
    store_root: Option<&Path>,
    mode: InstallMode,
    registry_fixture: Option<&Path>,
    allow_unverified_registry_artifacts: bool,
    report_replay_progress: impl FnMut(usize, usize),
) -> Result<InstallReport, String> {
    let offline = matches!(mode, InstallMode::Offline);
    let frozen = matches!(mode, InstallMode::Frozen);
    if package.is_some() && (offline || frozen) {
        return Err("a package argument cannot be used with --offline or --frozen".to_owned());
    }
    if allow_unverified_registry_artifacts && (offline || frozen) {
        return Err(
            "--allow-unverified-registry-artifacts cannot be used with --offline or --frozen"
                .to_owned(),
        );
    }
    let project_dir = match fs::canonicalize(project_dir) {
        Ok(path) if path.is_dir() => path,
        Ok(path) => {
            return Err(format!(
                "project directory is not a directory: {}",
                path.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "cannot access project directory '{}': {error}",
                project_dir.display()
            ));
        }
    };
    let preflight_manifest = read_manifest(&project_dir.join("package.json"))?;
    online::manifest_roots(&preflight_manifest)?;
    if let Some(updated) = manifest_override {
        online::manifest_roots(updated)?;
    }
    if offline || frozen {
        let lock_path = project_dir.join("tapid.lock");
        if lock_path.is_file() {
            // Reject incompatible persisted identities before activation recovery can
            // mutate project state. Read and validate again under the lock below.
            fs::read_to_string(&lock_path)
                .map_err(|error| error.to_string())
                .and_then(|input| Lockfile::from_json(&input).map_err(|error| error.to_string()))
                .map_err(|error| format!("invalid lockfile {}: {error}", lock_path.display()))?;
        }
    }
    let activation_lock = ActivationLock::acquire(&project_dir)?;
    if cfg!(debug_assertions) && std::env::var_os("TAPID_TEST_RECOVER_ONLY").is_some() {
        return Ok(InstallReport {
            package_count: 0,
            replayed: false,
        });
    }
    let current_manifest = read_manifest(&project_dir.join("package.json"))?;
    let manifest_path = project_dir.join("package.json");
    let lock_path = project_dir.join("tapid.lock");
    let original_manifest = fs::read(&manifest_path)
        .map_err(|error| format!("cannot preserve package.json for recovery: {error}"))?;
    let original_lock = match fs::read(&lock_path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot preserve tapid.lock for recovery: {error}")),
    };
    let lifecycle_journal_required_before_resolution =
        offline || frozen || manifest_override.is_some() || package.is_some();
    let mut lifecycle_journal = if lifecycle_journal_required_before_resolution {
        Some(
            crate::filesystem::lifecycle_journal::LifecycleJournal::begin(
                &project_dir,
                activation_lock.owner_line(),
                &original_manifest,
                original_lock.as_deref(),
            )?,
        )
    } else {
        None
    };
    let mut manifest_transaction = None;
    let manifest = if let Some(updated) = manifest_override {
        if package.is_some() {
            return Err("cannot combine a manifest override with a package argument".to_owned());
        }
        let transaction = ManifestTransaction::begin(&manifest_path)
            .map_err(|error| format!("cannot prepare package.json update: {error}"))?;
        transaction
            .write(&updated.to_json())
            .map_err(|error| format!("cannot update package.json: {error}"))?;
        manifest_transaction = Some(transaction);
        updated.clone()
    } else if let Some(spec) = package {
        let (name, requirement) = package_spec::parse(spec);
        let updated = current_manifest
            .with_dependency(name, requirement)
            .map_err(|error| format!("cannot add dependency '{spec}': {error}"))?;
        let transaction = ManifestTransaction::begin(&manifest_path)
            .map_err(|error| format!("cannot prepare package.json update: {error}"))?;
        transaction
            .write(&updated.to_json())
            .map_err(|error| format!("cannot update package.json: {error}"))?;
        manifest_transaction = Some(transaction);
        updated
    } else {
        current_manifest
    };
    if !offline && !frozen {
        let store = Store::new(match store_root {
            Some(path) => path.to_owned(),
            None => default_store_root()?,
        });
        let registry_config = crate::registry::RegistryConfig::load(&project_dir)?;
        let (lock, mut input, trees, store_transaction) = online::resolve_and_fetch(
            &project_dir,
            &manifest,
            &store,
            registry_fixture,
            allow_unverified_registry_artifacts,
            &registry_config,
        )?;
        if lifecycle_journal.is_none() {
            lifecycle_journal = Some(
                crate::filesystem::lifecycle_journal::LifecycleJournal::begin(
                    &project_dir,
                    activation_lock.owner_line(),
                    &original_manifest,
                    original_lock.as_deref(),
                )?,
            );
        }
        if let Some(journal) = lifecycle_journal.as_mut() {
            journal.set_store_root(store.root())?;
        }
        let lock_json = match lock.to_json() {
            Ok(value) => value,
            Err(error) => return Err(format!("cannot serialize lockfile: {error}")),
        };
        let publication_result = if let Some(journal) = lifecycle_journal.as_ref() {
            store_transaction.publish_for_lifecycle(&journal.coordinator_path())
        } else {
            store_transaction.publish()
        };
        let publication = publication_result
            .map_err(|error| format!("cannot publish verified store trees: {error}"))?;
        crate::filesystem::activation::test_crash_at("store_published");
        let trees = trees
            .into_iter()
            .map(|(key, path)| (key, publication.resolve_path(&path)))
            .collect();
        for instance in &mut input.instances {
            instance.tree.root = publication.resolve_path(&instance.tree.root);
        }
        let lock_backup = match crate::filesystem::atomic::replace_lockfile(&lock_path, &lock_json)
        {
            Ok(value) => value,
            Err(error) => {
                let store_rollback = publication
                    .rollback()
                    .err()
                    .map(|rollback| rollback.to_string());
                let mut message =
                    format!("cannot replace lockfile {}: {error}", lock_path.display());
                if let Some(rollback) = store_rollback {
                    message.push_str(&format!("; store rollback failed: {rollback}"));
                }
                return Err(message);
            }
        };
        crate::filesystem::activation::test_crash_at("lockfile_replaced");
        if let Err(error) = materialize_install(
            &project_dir,
            input,
            trees,
            &activation_lock,
            lifecycle_journal.is_some(),
        ) {
            let lock_rollback =
                crate::filesystem::atomic::rollback_lockfile(&lock_path, lock_backup.as_deref())
                    .err();
            let store_rollback = publication
                .rollback()
                .err()
                .map(|rollback| rollback.to_string());
            let mut message = error.to_string();
            if let Some(rollback) = lock_rollback {
                message.push_str(&format!("; lockfile rollback failed: {rollback}"));
            }
            if let Some(rollback) = store_rollback {
                message.push_str(&format!("; store rollback failed: {rollback}"));
            }
            return Err(message);
        }
        crate::filesystem::activation::test_crash_at("activation_complete");
        if let Some(journal) = lifecycle_journal.as_mut() {
            journal.mark_committed()?;
        }
        crate::filesystem::activation::test_crash_at("commit_decision");
        publication
            .commit()
            .map_err(|error| format!("cannot finalize verified store transaction: {error}"))?;
        let _ = crate::filesystem::atomic::discard_lockfile_backup(lock_backup.as_deref());
        if let Some(transaction) = manifest_transaction.take() {
            transaction.commit();
        }
        if let Some(journal) = lifecycle_journal.take() {
            journal.finish()?;
        }
        return Ok(InstallReport {
            package_count: lock.packages().len(),
            replayed: false,
        });
    }
    if !lock_path.is_file() {
        let message = if offline {
            "offline install requires tapid.lock"
        } else if frozen {
            "frozen install requires tapid.lock"
        } else {
            "cannot install without tapid.lock; network resolution is unavailable"
        };
        return Err(format!("{message}: {}", lock_path.display()));
    }
    let lock = match fs::read_to_string(&lock_path)
        .map_err(|e| e.to_string())
        .and_then(|s| Lockfile::from_json(&s).map_err(|e| e.to_string()))
    {
        Ok(value) => value,
        Err(error) => return Err(format!("invalid lockfile {}: {error}", lock_path.display())),
    };
    let current_manifest_digest = match fs::read(project_dir.join("package.json")) {
        Ok(bytes) => crate::filesystem::atomic::digest_bytes(&bytes),
        Err(error) => {
            return Err(format!(
                "cannot read root manifest for lockfile replay: {error}"
            ));
        }
    };
    if let Err(error) = lock.validate_replay(&current_manifest_digest) {
        return Err(format!("invalid lockfile {}: {error}", lock_path.display()));
    }
    let store = Store::new(match store_root {
        Some(path) => path.to_owned(),
        None => default_store_root()?,
    });
    if let Some(journal) = lifecycle_journal.as_mut() {
        store
            .recover_transactions()
            .map_err(|error| format!("cannot prepare shared store for recovery: {error}"))?;
        journal.set_store_root(store.root())?;
    }
    let registry_config = crate::registry::RegistryConfig::load(&project_dir)?;
    let (input, trees) = crate::application::replay::replay_input(
        &lock,
        &manifest,
        &store,
        &registry_config,
        report_replay_progress,
    )?;
    let replayed = materialize_with_lock(
        &project_dir,
        input,
        trees,
        true,
        &activation_lock,
        lifecycle_journal.is_some(),
    );
    replayed?;
    if let Some(journal) = lifecycle_journal.as_mut() {
        journal.mark_committed()?;
    }
    if let Some(journal) = lifecycle_journal.take() {
        journal.finish()?;
    }
    Ok(InstallReport {
        package_count: lock.packages().len(),
        replayed: true,
    })
}

fn materialize_install(
    project_dir: &Path,
    input: NamedLayoutInput,
    trees: BTreeMap<String, PathBuf>,
    activation_lock: &ActivationLock,
    preserve_previous: bool,
) -> Result<(), String> {
    materialize_with_lock(
        project_dir,
        input,
        trees,
        false,
        activation_lock,
        preserve_previous,
    )
}

fn materialize_with_lock(
    project_dir: &Path,
    input: NamedLayoutInput,
    trees: BTreeMap<String, PathBuf>,
    replayed: bool,
    activation_lock: &ActivationLock,
    preserve_previous: bool,
) -> Result<(), String> {
    let root = match ManagedRoot::new(project_dir) {
        Ok(value) => value,
        Err(error) => {
            if replayed {
                crate::application::replay::cleanup_replay_snapshots(&trees);
            }
            return Err(error.to_string());
        }
    };
    let platform = crate::application::replay::current_platform();
    let plan = match plan_named_layout(root, input.clone(), platform) {
        Ok(value) => value,
        Err(error) => {
            if replayed {
                crate::application::replay::cleanup_replay_snapshots(&trees);
            }
            return Err(error.to_string());
        }
    };
    let stage = match activation_lock.create_stage(project_dir) {
        Ok(stage) => stage,
        Err(error) => {
            if replayed {
                crate::application::replay::cleanup_replay_snapshots(&trees);
            }
            return Err(error);
        }
    };
    let result = crate::filesystem::tree::materialize_stage(
        &stage,
        &plan,
        &input.instances,
        &trees,
        replayed,
    )
    .and_then(|_| {
        crate::filesystem::activation::activate_node_modules_with_lock(
            project_dir,
            &stage,
            activation_lock,
            preserve_previous,
        )
    });
    if replayed {
        crate::application::replay::cleanup_replay_snapshots(&trees);
    }
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn environment<'a>(
        entries: &'a [(&'a str, &'a str)],
    ) -> impl FnMut(&str) -> Option<OsString> + 'a {
        move |name| {
            entries
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn default_store_is_outside_a_macos_consumer_project() {
        let store =
            default_store_root_for("macos", environment(&[("HOME", "/Users/example")])).unwrap();

        assert_eq!(
            store,
            PathBuf::from("/Users/example/Library/Caches/tapid/store")
        );
        assert!(!store.starts_with("/Users/example/source/application"));
    }

    #[test]
    fn linux_default_store_prefers_absolute_xdg_cache_home() {
        let store = default_store_root_for(
            "linux",
            environment(&[
                ("XDG_CACHE_HOME", "/cache/example"),
                ("HOME", "/home/example"),
            ]),
        )
        .unwrap();

        assert_eq!(store, PathBuf::from("/cache/example/tapid/store"));
    }

    #[test]
    fn windows_default_store_uses_local_application_data() {
        let store = default_store_root_for(
            "windows",
            environment(&[("LOCALAPPDATA", "C:\\Users\\example\\AppData\\Local")]),
        )
        .unwrap();

        assert_eq!(
            store,
            PathBuf::from("C:\\Users\\example\\AppData\\Local").join("tapid/store")
        );
    }

    #[test]
    fn relative_cache_environment_is_rejected() {
        let error = default_store_root_for(
            "linux",
            environment(&[
                ("XDG_CACHE_HOME", "relative/cache"),
                ("HOME", "/home/example"),
            ]),
        )
        .unwrap_err();

        assert!(error.contains("absolute"));
    }
}
