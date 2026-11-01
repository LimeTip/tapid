use std::{path::Path, process::Command};
use tapid_test_support::{TempHome, TempProject};

/// Keep the project and its command environment alive for the whole scenario.
pub(super) struct CliFixture {
    pub project: TempProject,
    home: TempHome,
}

impl CliFixture {
    pub fn new(label: &str) -> Self {
        Self {
            project: TempProject::new(label).unwrap(),
            home: TempHome::new(label).unwrap(),
        }
    }

    pub fn command(&self, args: &[&str]) -> Command {
        command_in_home(self.project.path(), self.home.path(), args, true)
    }
}

/// Preserve arguments exactly. Store overrides belong to the calling test;
/// otherwise all option orders use the same isolated home and default store.
pub(super) fn command_in_home(
    project: &Path,
    home: &Path,
    args: &[&str],
    clear_environment: bool,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
    if clear_environment {
        command.env_clear();
    }
    command
        .args(args)
        .current_dir(project)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CACHE_HOME", home)
        .env("LOCALAPPDATA", home)
        .env("APPDATA", home)
        .env("XDG_CONFIG_HOME", home.join(".config"));
    command
}
