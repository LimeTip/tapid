use crate::commands::manifest::read_manifest;
use crate::filesystem::activation::ActivationLock;
use crate::{online, package_spec};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use tapid_linker::{LayoutInput, ManagedRoot, plan_layout};
use tapid_lockfile::Lockfile;
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
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn run(
    project_dir: &Path,
    package: Option<&str>,
    store_root: Option<&Path>,
    offline: bool,
    frozen: bool,
    registry_fixture: Option<&Path>,
    allow_unverified_registry_artifacts: bool,
) -> Result<InstallReport, String> {
    if package.is_some() && (offline || frozen) {
        return Err("a package argument cannot be used with --offline or --frozen".to_owned());
    }
    if allow_unverified_registry_artifacts && (offline || frozen) {
        return Err(
            "--allow-unverified-registry-artifacts cannot be used with --offline or --frozen"
                .to_owned(),
        );
    }
    let mut warnings = Vec::new();
    if allow_unverified_registry_artifacts {
        warnings.push("warning: npm artifacts without registry integrity are not authenticated against a registry-declared digest".to_owned());
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
    let activation_lock = ActivationLock::acquire(&project_dir)?;
    let manifest = read_manifest(&project_dir.join("package.json"))?;
    let manifest_path = project_dir.join("package.json");
    let mut manifest_transaction = None;
    let manifest = if let Some(spec) = package {
        let (name, requirement) = package_spec::parse(spec);
        let updated = match manifest.with_dependency(name, requirement) {
            Ok(value) => value,
            Err(error) => return Err(format!("cannot add dependency '{spec}': {error}")),
        };
        let transaction = match ManifestTransaction::begin(&manifest_path) {
            Ok(value) => value,
            Err(error) => return Err(format!("cannot prepare package.json update: {error}")),
        };
        if let Err(error) = transaction.write(&updated.to_json()) {
            return Err(format!("cannot update package.json: {error}"));
        }
        manifest_transaction = Some(transaction);
        updated
    } else {
        manifest
    };
    let lock_path = project_dir.join("tapid.lock");
    if !offline && !frozen {
        let store = Store::new(
            store_root
                .map(PathBuf::from)
                .unwrap_or_else(|| project_dir.join(".tapid-store")),
        );
        let (lock, input, trees) = online::resolve_and_fetch(
            &project_dir,
            &manifest,
            &store,
            registry_fixture,
            allow_unverified_registry_artifacts,
        )?;
        let lock_json = match lock.to_json() {
            Ok(value) => value,
            Err(error) => return Err(format!("cannot serialize lockfile: {error}")),
        };
        let lock_backup = match crate::filesystem::atomic::replace_lockfile(&lock_path, &lock_json)
        {
            Ok(value) => value,
            Err(error) => {
                return Err(format!(
                    "cannot replace lockfile {}: {error}",
                    lock_path.display()
                ));
            }
        };
        if let Err(error) = materialize_install(&project_dir, input, trees, &activation_lock) {
            let _ =
                crate::filesystem::atomic::rollback_lockfile(&lock_path, lock_backup.as_deref());
            return Err(error.to_string());
        }
        let _ = crate::filesystem::atomic::discard_lockfile_backup(lock_backup.as_deref());
        if let Some(transaction) = manifest_transaction.take() {
            transaction.commit();
        }
        return Ok(InstallReport {
            package_count: lock.packages().len(),
            replayed: false,
            warnings,
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
    let store = Store::new(
        store_root
            .map(PathBuf::from)
            .unwrap_or_else(|| project_dir.join(".tapid-store")),
    );
    let (input, trees) = crate::application::replay::replay_input(&lock, &manifest, &store)?;
    materialize_with_lock(&project_dir, input, trees, true, &activation_lock).map(|_| {
        InstallReport {
            package_count: lock.packages().len(),
            replayed: true,
            warnings,
        }
    })
}

fn materialize_install(
    project_dir: &Path,
    input: LayoutInput,
    trees: BTreeMap<String, PathBuf>,
    activation_lock: &ActivationLock,
) -> Result<(), String> {
    materialize_with_lock(project_dir, input, trees, false, activation_lock)
}

fn materialize_with_lock(
    project_dir: &Path,
    input: LayoutInput,
    trees: BTreeMap<String, PathBuf>,
    replayed: bool,
    activation_lock: &ActivationLock,
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
    let plan = match plan_layout(root, input.clone(), platform) {
        Ok(value) => value,
        Err(error) => {
            if replayed {
                crate::application::replay::cleanup_replay_snapshots(&trees);
            }
            return Err(error.to_string());
        }
    };
    let stage = project_dir.join(format!(
        ".tapid-install-stage-{}-{}",
        std::process::id(),
        crate::filesystem::atomic::unique_nonce()
    ));
    if let Err(error) = fs::create_dir(&stage) {
        if replayed {
            crate::application::replay::cleanup_replay_snapshots(&trees);
        }
        return Err(format!("cannot create install staging directory: {error}"));
    }
    let result = crate::filesystem::tree::materialize_stage(&stage, &plan, &input, &trees)
        .and_then(|_| {
            crate::filesystem::activation::activate_node_modules_with_lock(
                project_dir,
                &stage,
                activation_lock,
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
