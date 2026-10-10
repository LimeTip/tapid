//! Human install output uses the application's progress and settled outcome.
use crate::application::{
    install::{InstallReport, Progress},
    outcome::{ChangeState, ErrorKind, OperationFailure, RetryAdvice},
};
use std::{
    io::IsTerminal,
    time::{Duration, Instant},
};

pub(crate) struct Reporter {
    started: Instant,
    last: Option<(u8, Duration, String)>,
    json: bool,
    terminal: bool,
}

impl Reporter {
    pub(crate) fn new(json: bool) -> Self {
        Self {
            started: Instant::now(),
            last: None,
            json,
            terminal: std::io::stderr().is_terminal(),
        }
    }

    pub(crate) fn progress(&mut self, event: Progress) {
        if !self.terminal {
            return;
        }
        if let Some(line) = self.progress_line(event, self.started.elapsed()) {
            eprintln!("{line}");
        }
    }

    fn progress_line(&mut self, event: Progress, elapsed: Duration) -> Option<String> {
        if self.json {
            return None;
        }
        let (phase, complete, line) = match event {
            Progress::Metadata(count) => (0, false, format!("Resolving: {count} package(s)")),
            Progress::Artifact(done, total) => {
                (1, done == total, format!("Verifying: {done}/{total}"))
            }
            Progress::Replay(done, total) => {
                (2, done == total, format!("Reading store: {done}/{total}"))
            }
            Progress::Materialization(done, total) => {
                (3, done == total, format!("Linking: {done}/{total}"))
            }
        };
        if let Some((previous_phase, previous_time, previous_line)) = &self.last
            && (previous_line == &line
                || (*previous_phase == phase
                    && !complete
                    && elapsed.saturating_sub(*previous_time) < Duration::from_secs(1)))
        {
            return None;
        }
        self.last = Some((phase, elapsed, line.clone()));
        Some(line)
    }

    pub(crate) fn summary(&self, report: &InstallReport) {
        super::report_warnings(&report.outcome.warnings);
        if report.replayed {
            println!(
                "Replayed lockfile: {} package(s) in {:.2}s",
                report.package_count,
                self.started.elapsed().as_secs_f64()
            );
        } else {
            println!(
                "Installed {} package(s) in {:.2}s",
                report.package_count,
                self.started.elapsed().as_secs_f64()
            );
        }
        if let Some(changes) = &report.package_changes {
            println!(
                "Lock selections: {} added, {} changed, {} reused, {} removed",
                changes.added, changes.changed, changes.reused, changes.removed
            );
        }
        let mut files = report
            .outcome
            .changed_files
            .iter()
            .map(|path| {
                let relative = path
                    .strip_prefix(&report.outcome.project_dir)
                    .unwrap_or(path);
                crate::application::outcome::sanitize(&relative.display().to_string())
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        if files.is_empty() {
            println!("Project files unchanged.");
        } else {
            println!("Changed: {}", files.join(", "));
        }
    }

    pub(crate) fn failure(&self, failure: &OperationFailure) {
        let phase = match failure.error.kind {
            ErrorKind::InvalidRequest
            | ErrorKind::Project
            | ErrorKind::Manifest
            | ErrorKind::ProjectBusy => "project validation",
            ErrorKind::Lockfile | ErrorKind::LockfileMissing | ErrorKind::LockManifestMismatch => {
                "lockfile validation"
            }
            ErrorKind::RegistryConfiguration
            | ErrorKind::RegistryCredentialMissing
            | ErrorKind::RegistryMetadata
            | ErrorKind::RegistryTransport => "registry access",
            ErrorKind::Resolution | ErrorKind::PeerDependency => "dependency resolution",
            ErrorKind::Integrity | ErrorKind::Archive => "artifact verification",
            ErrorKind::Store | ErrorKind::StoreUnavailable | ErrorKind::StoreBusy => "store access",
            ErrorKind::Materialization => "linking packages",
            ErrorKind::Transaction => "publishing project changes",
            ErrorKind::Recovery => "transaction recovery",
            ErrorKind::InvalidData => "install validation",
        };
        eprintln!("Install failed during {phase}.");
        super::report_failure(failure);
        match failure.outcome.state {
            ChangeState::Unchanged => eprintln!("Project files unchanged."),
            ChangeState::RolledBack => eprintln!("Project changes rolled back."),
            _ => {}
        }
        // Never suggest repeating a committed operation or bypassing recovery.
        if failure.retry != RetryAdvice::AfterCorrection {
            return;
        }
        let hint = match failure.error.kind {
            ErrorKind::LockfileMissing => "Run 'tapid install' online to create tapid.lock.",
            ErrorKind::LockManifestMismatch => {
                "Run 'tapid install' online to update tapid.lock for the current manifest."
            }
            ErrorKind::StoreUnavailable => {
                "Run 'tapid install --frozen' online with the same store directory to fetch missing locked packages."
            }
            ErrorKind::RegistryCredentialMissing => {
                "Check the credentials configured for this package's registry."
            }
            ErrorKind::RegistryTransport => {
                "Check the registry address and network connection before retrying."
            }
            ErrorKind::Integrity => {
                "Check the package source and registry integrity metadata before retrying."
            }
            _ => return,
        };
        eprintln!("{hint}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_progress_throttles_bursts_but_keeps_phase_changes_and_completion() {
        let mut reporter = Reporter::new(false);
        let now = Duration::ZERO;
        assert_eq!(
            reporter
                .progress_line(Progress::Metadata(1), now)
                .as_deref(),
            Some("Resolving: 1 package(s)")
        );
        for count in 2..1000 {
            assert!(
                reporter
                    .progress_line(Progress::Metadata(count), now)
                    .is_none()
            );
        }
        assert!(
            reporter
                .progress_line(Progress::Metadata(1000), Duration::from_secs(1))
                .is_some()
        );
        assert_eq!(
            reporter
                .progress_line(Progress::Artifact(1, 100), now)
                .as_deref(),
            Some("Verifying: 1/100")
        );
        assert!(
            reporter
                .progress_line(Progress::Artifact(50, 100), now)
                .is_none()
        );
        assert_eq!(
            reporter
                .progress_line(Progress::Artifact(100, 100), now)
                .as_deref(),
            Some("Verifying: 100/100")
        );
        assert!(
            reporter
                .progress_line(Progress::Artifact(100, 100), now)
                .is_none()
        );
        assert_eq!(
            reporter
                .progress_line(Progress::Replay(1, 1), now)
                .as_deref(),
            Some("Reading store: 1/1")
        );
        assert_eq!(
            reporter
                .progress_line(Progress::Materialization(1, 1), now)
                .as_deref(),
            Some("Linking: 1/1")
        );
    }

    #[test]
    fn install_progress_is_silent_in_json_mode() {
        let mut reporter = Reporter::new(true);
        for event in [
            Progress::Metadata(1),
            Progress::Artifact(1, 1),
            Progress::Replay(1, 1),
            Progress::Materialization(1, 1),
        ] {
            assert!(reporter.progress_line(event, Duration::ZERO).is_none());
        }
    }
}
