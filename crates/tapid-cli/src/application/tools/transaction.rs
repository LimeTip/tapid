//! The project install journal cannot cover global command files. This journal
//! records both catalogs and a separate, content-bound commit decision.
use super::{
    error,
    state::{self, Catalog},
};
use crate::application::outcome::OperationalError;
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    before: Catalog,
    after: Catalog,
}

fn bytes(root: &Path, name: &str) -> Result<Option<Vec<u8>>, OperationalError> {
    let path = root.join("bin").join(name);
    state::check_path(&path)?;
    match fs::symlink_metadata(&path) {
        Ok(_) => Ok(Some(state::read(&path)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(error(e)),
    }
}

fn preflight(root: &Path, before: &Catalog, after: &Catalog) -> Result<(), OperationalError> {
    for name in before
        .shims
        .keys()
        .chain(after.shims.keys())
        .collect::<BTreeSet<_>>()
    {
        let current = bytes(root, name)?;
        match (before.shims.get(name), current) {
            (Some(expected), Some(actual)) if actual == expected.as_bytes() => {}
            (Some(_), _) => {
                return Err(error(format!(
                    "Tapid launcher was replaced or removed; preserving {}",
                    root.join("bin").join(name).display()
                )));
            }
            (None, None) => {}
            (None, Some(_)) => {
                return Err(error(format!(
                    "command collision with an unowned file: {name}"
                )));
            }
        }
    }
    Ok(())
}

fn settle(root: &Path, desired: &Catalog, alternate: &Catalog) -> Result<(), OperationalError> {
    // Only recorded bytes may be replaced/deleted. A user's replacement blocks
    // settlement rather than becoming permission to overwrite it.
    for name in desired
        .shims
        .keys()
        .chain(alternate.shims.keys())
        .collect::<BTreeSet<_>>()
    {
        let current = bytes(root, name)?;
        let wanted = desired.shims.get(name);
        if current.as_deref() == wanted.map(|s| s.as_bytes()) {
            continue;
        }
        if let Some(current) = &current {
            if ![desired.shims.get(name), alternate.shims.get(name)]
                .into_iter()
                .flatten()
                .any(|s| s.as_bytes() == current)
            {
                return Err(error(format!(
                    "preserving modified launcher during recovery: {name}"
                )));
            }
            fs::remove_file(root.join("bin").join(name)).map_err(error)?;
        }
        if let Some(contents) = wanted {
            state::write_new(&root.join("bin").join(name), contents.as_bytes(), true)?;
        }
    }
    let path = root.join("catalog.json");
    state::check_path(&path)?;
    let encoded = serde_json::to_string_pretty(desired).map_err(error)?;
    let backup = crate::filesystem::atomic::replace_lockfile(&path, &encoded).map_err(error)?;
    crate::filesystem::atomic::discard_lockfile_backup(backup.as_deref()).map_err(error)?;
    crate::filesystem::activation::sync_directory(&root.join("bin")).map_err(error)?;
    crate::filesystem::activation::sync_directory(root).map_err(error)
}

pub(super) fn recover(root: &Path) -> Result<bool, OperationalError> {
    let pending = root.join("pending.json");
    state::check_path(&pending)?;
    if !pending.try_exists().map_err(error)? {
        if root.join("committed").try_exists().map_err(error)? {
            let marker = state::read(&root.join("committed"))?;
            if state::load(root)?
                .settled_transaction
                .as_ref()
                .map(|s| s.as_bytes())
                != Some(marker.as_slice())
            {
                return Err(error("orphaned tool commit decision requires inspection"));
            }
            fs::remove_file(root.join("committed")).map_err(error)?;
            return Ok(true);
        }
        return Ok(false);
    }
    let encoded = state::read(&pending)?;
    let journal: Journal = serde_json::from_slice(&encoded).map_err(error)?;
    state::validate(&journal.before)?;
    state::validate(&journal.after)?;
    let committed = root.join("committed");
    state::check_path(&committed)?;
    let decision = committed.try_exists().map_err(error)?;
    if decision
        && state::read(&committed)? != crate::filesystem::atomic::digest_bytes(&encoded).as_bytes()
    {
        return Err(error("tool commit decision does not match its journal"));
    }
    let (desired, alternate) = if decision {
        (&journal.after, &journal.before)
    } else {
        (&journal.before, &journal.after)
    };
    let mut desired = desired.clone();
    if decision {
        desired.settled_transaction = Some(crate::filesystem::atomic::digest_bytes(&encoded));
    }
    settle(root, &desired, alternate)?;
    fs::remove_file(pending).map_err(error)?;
    crate::filesystem::activation::sync_directory(root).map_err(error)?;
    crate::filesystem::activation::test_crash_at("tool_settled");
    if decision {
        fs::remove_file(&committed).map_err(error)?;
        crate::filesystem::activation::sync_directory(root).map_err(error)?;
    }
    Ok(true)
}

pub(super) fn activate(
    root: &Path,
    before: &Catalog,
    after: &Catalog,
) -> Result<(), OperationalError> {
    state::validate(after)?;
    preflight(root, before, after)?;
    let encoded = serde_json::to_vec(&Journal {
        before: before.clone(),
        after: after.clone(),
    })
    .map_err(error)?;
    if encoded.len() > state::LIMIT {
        return Err(error("tool transaction exceeds its bounded state limit"));
    }
    state::write_new(&root.join("pending.json"), &encoded, false)?;
    for (name, contents) in &after.shims {
        if bytes(root, name)?.is_none() {
            state::write_new(&root.join("bin").join(name), contents.as_bytes(), true)?;
        }
    }
    crate::filesystem::activation::test_crash_at("tool_launchers");
    state::write_new(
        &root.join("committed"),
        crate::filesystem::atomic::digest_bytes(&encoded).as_bytes(),
        false,
    )?;
    crate::filesystem::activation::test_crash_at("tool_commit");
    recover(root)?;
    Ok(())
}

pub(super) struct Cleanup {
    pub pending: bool,
    pub changed: bool,
}
const RETIRED: &str = ".tapid-tool-retired-";
const STAGING: &str = ".tapid-tool-staging-v1-";

pub(super) fn create_environment(
    root: &Path,
    generation: &str,
) -> Result<PathBuf, OperationalError> {
    let directory = root.join("environments");
    let environment = directory.join(generation);
    state::check_path(&environment)?;
    match fs::symlink_metadata(&environment) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(error(
                "tool generation path already exists; preserving files",
            ));
        }
        Err(e) => return Err(error(e)),
    }
    let intent = directory.join(format!("{STAGING}{generation}"));
    state::check_path(&intent)?;
    fs::create_dir(&intent).map_err(error)?;
    crate::filesystem::activation::sync_directory(&directory).map_err(error)?;
    crate::filesystem::activation::test_crash_at("tool_stage_intent");
    fs::create_dir(&environment).map_err(error)?;
    crate::filesystem::activation::test_crash_at("tool_environment_created");
    state::write_new(
        &environment.join(".tapid-tool"),
        generation.as_bytes(),
        false,
    )?;
    crate::filesystem::activation::test_crash_at("tool_environment_owned");
    state::write_new(&environment.join(".in-use"), b"", false)?;
    fs::remove_dir(intent).map_err(error)?;
    crate::filesystem::activation::sync_directory(&directory).map_err(error)?;
    Ok(environment)
}

fn remove_payload(environment: &Path) -> Result<(), OperationalError> {
    for entry in fs::read_dir(environment).map_err(error)? {
        let entry = entry.map_err(error)?;
        if entry.file_name() == ".tapid-tool" || entry.file_name() == ".in-use" {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(error)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if state::is_link(&metadata) && metadata.file_attributes() & 0x10 != 0 {
                fs::remove_dir(&path).map_err(error)?;
                continue;
            }
        }
        if metadata.is_dir() && !state::is_link(&metadata) {
            // The standard library does not traverse directory symlinks.
            fs::remove_dir_all(&path).map_err(error)?;
        } else {
            fs::remove_file(&path).map_err(error)?;
        }
    }
    crate::filesystem::activation::sync_directory(environment).map_err(error)
}

fn finish_empty_retirement(directory: &Path, generation: &str) -> Result<(), OperationalError> {
    let marker = directory.join(format!("{RETIRED}{generation}"));
    if state::read(&marker)? != generation.as_bytes() {
        return Err(error("invalid retired environment ownership"));
    }
    let environment = directory.join(generation);
    state::check_path(&environment)?;
    match fs::remove_dir(&environment) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error(e)),
    }
    crate::filesystem::activation::sync_directory(directory).map_err(error)?;
    fs::remove_file(marker).map_err(error)?;
    crate::filesystem::activation::sync_directory(directory).map_err(error)
}

fn retire(directory: &Path, generation: &str, lease: fs::File) -> Result<(), OperationalError> {
    let environment = directory.join(generation);
    remove_payload(&environment)?;
    // Keep the in-tree marker until every payload entry has been removed.
    // A separate finalization marker then permits only nonrecursive removal
    // of the empty directory if a process stops after deleting the inner marker.
    let retired = directory.join(format!("{RETIRED}{generation}"));
    state::check_path(&retired)?;
    if retired.try_exists().map_err(error)? {
        if state::read(&retired)? != generation.as_bytes() {
            return Err(error("invalid retirement marker"));
        }
    } else {
        state::write_new(&retired, generation.as_bytes(), false)?;
    }
    fs::remove_file(environment.join(".in-use")).map_err(error)?;
    fs::remove_file(environment.join(".tapid-tool")).map_err(error)?;
    drop(lease);
    finish_empty_retirement(directory, generation)
}

pub(super) fn cleanup(root: &Path, catalog: &Catalog) -> Result<Cleanup, OperationalError> {
    let directory = root.join("environments");
    state::check_path(&directory)?;
    let active = catalog
        .tools
        .values()
        .map(|t| t.generation.as_str())
        .collect::<BTreeSet<_>>();
    let mut result = Cleanup {
        pending: false,
        changed: false,
    };
    // Only the atomic sidecar, never a generation name or temporary filename,
    // authorizes completing ownership of an interrupted staging directory.
    for entry in fs::read_dir(&directory).map_err(error)? {
        let entry = entry.map_err(error)?;
        let name = entry.file_name();
        let Some(generation) = name.to_str().and_then(|s| s.strip_prefix(STAGING)) else {
            continue;
        };
        if generation.is_empty()
            || !generation.bytes().all(|b| b.is_ascii_hexdigit())
            || active.contains(generation)
        {
            continue;
        }
        let intent = entry.path();
        let metadata = fs::symlink_metadata(&intent).map_err(error)?;
        if state::is_link(&metadata)
            || !metadata.is_dir()
            || fs::read_dir(&intent).map_err(error)?.next().is_some()
        {
            return Err(error(
                "invalid tool staging ownership marker; preserving files",
            ));
        }
        let environment = directory.join(generation);
        state::check_path(&environment)?;
        match fs::symlink_metadata(&environment) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Ok(metadata) if metadata.is_dir() && !state::is_link(&metadata) => {
                let marker = environment.join(".tapid-tool");
                match fs::symlink_metadata(&marker) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        state::write_new(&marker, generation.as_bytes(), false)?;
                    }
                    Ok(_) => {
                        if state::read(&marker)? != generation.as_bytes() {
                            return Err(error("tool staging ownership mismatch; preserving files"));
                        }
                    }
                    Err(e) => return Err(error(e)),
                }
                // An existing marker may come from a crash after rename but
                // before its directory sync. Persist it before retiring intent.
                crate::filesystem::activation::sync_directory(&environment).map_err(error)?;
            }
            Ok(_) => return Err(error("invalid tool staging directory; preserving files")),
            Err(e) => return Err(error(e)),
        }
        fs::remove_dir(intent).map_err(error)?;
        crate::filesystem::activation::sync_directory(&directory).map_err(error)?;
        result.changed = true;
    }
    for entry in fs::read_dir(&directory).map_err(error)? {
        let entry = entry.map_err(error)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if active.contains(name) || name.is_empty() || !name.bytes().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(error)?;
        if state::is_link(&metadata) || !metadata.is_dir() {
            continue;
        }
        let marker = path.join(".tapid-tool");
        if !marker.try_exists().map_err(error)? {
            continue;
        }
        if state::read(&marker)? != name.as_bytes() {
            return Err(error(
                "tool environment ownership mismatch; preserving files",
            ));
        }
        state::check_path(&path.join(".in-use"))?;
        let lease =
            crate::filesystem::activation::open_lock_file(&path.join(".in-use")).map_err(error)?;
        if FileExt::try_lock(&lease).is_err() {
            result.pending = true;
            continue;
        }
        if retire(&directory, name, lease).is_err() {
            result.pending = true;
        } else {
            result.changed = true;
        }
    }
    for entry in fs::read_dir(&directory).map_err(error)? {
        let entry = entry.map_err(error)?;
        let name = entry.file_name();
        let Some(generation) = name.to_str().and_then(|s| s.strip_prefix(RETIRED)) else {
            continue;
        };
        if generation.is_empty() || !generation.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        if active.contains(generation) {
            return Err(error(
                "an active tool has an inconsistent retirement marker",
            ));
        }
        if finish_empty_retirement(&directory, generation).is_err() {
            result.pending = true;
        } else {
            result.changed = true;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_test_support::TempHome;

    #[test]
    fn generation_creation_preserves_an_existing_unowned_directory() {
        let home = TempHome::new("tool-stage-collision").unwrap();
        let root = state::root_path(Some(home.path())).unwrap();
        state::initialize(&root).unwrap();
        let environment = root.join("environments/deadbeef");
        fs::create_dir(&environment).unwrap();
        fs::write(environment.join("user-file"), b"user file").unwrap();
        assert!(create_environment(&root, "deadbeef").is_err());
        assert!(
            !root
                .join(format!("environments/{STAGING}deadbeef"))
                .exists()
        );
        assert_eq!(
            fs::read(environment.join("user-file")).unwrap(),
            b"user file"
        );
    }

    #[test]
    fn stage_recovery_preserves_invalid_ownership_records() {
        for case in ["file", "nonempty", "wrong-owner", "directory-owner"] {
            let home = TempHome::new(case).unwrap();
            let root = state::root_path(Some(home.path())).unwrap();
            state::initialize(&root).unwrap();
            let environment = root.join("environments/deadbeef");
            fs::create_dir(&environment).unwrap();
            fs::write(environment.join("user-file"), b"user file").unwrap();
            let intent = root.join(format!("environments/{STAGING}deadbeef"));
            if case == "file" {
                fs::write(&intent, b"user file").unwrap();
            } else {
                fs::create_dir(&intent).unwrap();
            }
            match case {
                "nonempty" => fs::write(intent.join("user-file"), b"user file").unwrap(),
                "wrong-owner" => fs::write(environment.join(".tapid-tool"), b"other").unwrap(),
                "directory-owner" => fs::create_dir(environment.join(".tapid-tool")).unwrap(),
                _ => {}
            }
            assert!(cleanup(&root, &Catalog::default()).is_err(), "{case}");
            assert_eq!(
                fs::read(environment.join("user-file")).unwrap(),
                b"user file"
            );
            assert!(intent.exists());
        }
    }
}
