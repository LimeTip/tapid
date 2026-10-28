use super::outcome::{
    ChangeState, ErrorKind, OperationFailure, OperationOutcome, OperationalError,
};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use tapid_publish::{NpmArtifact, NpmPackOptions, pack_npm};

pub(crate) struct PackResult {
    pub(crate) artifact: NpmArtifact,
    pub(crate) destination: Option<PathBuf>,
    pub(crate) preview: bool,
    pub(crate) outcome: OperationOutcome,
}

pub(crate) fn pack(
    directory: &Path,
    output: Option<&Path>,
    preview: bool,
) -> Result<PackResult, OperationFailure> {
    let mut outcome = OperationOutcome::unchanged(directory);
    let result = (|| -> io::Result<(NpmArtifact, Option<PathBuf>)> {
        if !preview && output.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--output is required unless --dry-run is used",
            ));
        }
        let destination = output.map(resolve_destination).transpose()?;
        let artifact = pack_npm(
            directory,
            &NpmPackOptions {
                destination: destination.clone(),
                ..Default::default()
            },
        )?;
        if !preview {
            let destination = destination.as_ref().unwrap();
            activate(destination, artifact.bytes(), &mut outcome)?;
        }
        Ok((artifact, destination))
    })();
    match result {
        Ok((artifact, destination)) => Ok(PackResult {
            artifact,
            destination,
            preview,
            outcome,
        }),
        Err(error) => Err(OperationFailure::new(
            OperationalError::from_source(ErrorKind::InvalidData, error),
            outcome,
            None,
        )),
    }
}
fn resolve_destination(path: &Path) -> io::Result<PathBuf> {
    let path = std::env::current_dir()?.join(path);
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output must name a file"))?;
    let destination = path.parent().unwrap().canonicalize()?.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output already exists",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(error),
    }
}
static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);
fn activate(destination: &Path, bytes: &[u8], outcome: &mut OperationOutcome) -> io::Result<()> {
    activate_with(destination, bytes, outcome, |_| Ok(()))
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Write,
    Sync,
    Link,
    Activated,
    Cleanup,
}
fn activate_with(
    destination: &Path,
    bytes: &[u8],
    outcome: &mut OperationOutcome,
    mut phase: impl FnMut(Phase) -> io::Result<()>,
) -> io::Result<()> {
    let parent = destination.parent().unwrap();
    let (staging, mut file) = (0..128)
        .find_map(|_| {
            let name = format!(
                ".tapid-pack-{}-{}",
                std::process::id(),
                NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
            );
            let path = parent.join(name);
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => Some(Ok((path, file))),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .unwrap_or_else(|| {
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "staging name attempts exhausted",
            ))
        })?;
    let result = (|| {
        phase(Phase::Write)?;
        file.write_all(bytes)?;
        phase(Phase::Sync)?;
        file.sync_all()?;
        phase(Phase::Link)?;
        fs::hard_link(&staging, destination)?;
        outcome.state = ChangeState::Committed;
        outcome.changed_files.push(destination.to_owned());
        phase(Phase::Activated)
    })();
    drop(file);
    let cleanup = phase(Phase::Cleanup).and_then(|_| fs::remove_file(&staging));
    if cleanup.is_err() && outcome.state == ChangeState::Committed {
        outcome.state = ChangeState::CommittedCleanupPending;
    }
    result?;
    if let Err(error) = cleanup {
        outcome.state = ChangeState::CommittedCleanupPending;
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_test_support::TempProject;

    #[test]
    fn activation_faults_never_expose_partial_destinations() {
        for phase in [Phase::Write, Phase::Sync, Phase::Link] {
            let project = TempProject::new("pack-activation-fault").unwrap();
            let destination = project.path().join("output");
            let mut outcome = OperationOutcome::unchanged(project.path());
            assert!(
                activate_with(&destination, b"complete", &mut outcome, |point| {
                    if point == phase {
                        Err(io::Error::other("injected failure"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
            );
            assert!(!destination.exists());
            assert_eq!(outcome.state, ChangeState::Unchanged);
            assert_eq!(fs::read_dir(project.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn activation_race_preserves_competing_destination() {
        let project = TempProject::new("pack-activation-race").unwrap();
        let destination = project.path().join("output");
        let mut outcome = OperationOutcome::unchanged(project.path());
        assert!(
            activate_with(&destination, b"ours", &mut outcome, |phase| {
                if phase == Phase::Link {
                    fs::write(&destination, b"competitor")?;
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"competitor");
        assert_eq!(outcome.state, ChangeState::Unchanged);
    }

    #[test]
    fn activation_cleanup_failure_reports_committed_artifact() {
        let project = TempProject::new("pack-cleanup-fault").unwrap();
        let destination = project.path().join("output");
        let mut outcome = OperationOutcome::unchanged(project.path());
        assert!(
            activate_with(&destination, b"complete", &mut outcome, |phase| {
                if phase == Phase::Cleanup {
                    Err(io::Error::other("cleanup denied"))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"complete");
        assert_eq!(outcome.state, ChangeState::CommittedCleanupPending);
        assert_eq!(fs::read_dir(project.path()).unwrap().count(), 2);
    }

    #[test]
    fn interrupted_activation_leaves_only_orphan_or_complete_destination() {
        for interrupt in [Phase::Write, Phase::Activated] {
            let project = TempProject::new("pack-interrupted").unwrap();
            let destination = project.path().join("output");
            let mut outcome = OperationOutcome::unchanged(project.path());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                activate_with(&destination, b"complete", &mut outcome, |phase| {
                    assert_ne!(phase, interrupt, "simulated interruption");
                    Ok(())
                })
            }));
            assert!(result.is_err());
            if interrupt == Phase::Activated {
                assert_eq!(fs::read(&destination).unwrap(), b"complete");
            } else {
                assert!(!destination.exists());
            }
            assert!(fs::read_dir(project.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .starts_with(".tapid-pack-")
            }));
        }
    }
}
