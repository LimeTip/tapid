use super::{command_support::CliFixture, isolated_command};
use std::path::PathBuf;

#[test]
fn command_fixture_preserves_coverage_profile_destination() {
    let fixture = CliFixture::new("coverage-command-fixture");
    let Some(profile) = std::env::var_os("TAPID_PROFILE_FIXTURE_CHILD") else {
        let profiles = tapid_test_support::TempProject::new("coverage-profile-owner").unwrap();
        for profile in [
            PathBuf::from("child-%p.profraw"),
            profiles.path().join("absolute-%p.profraw"),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "command_fixture_tests::command_fixture_preserves_coverage_profile_destination",
                    "--nocapture",
                ])
                .current_dir(profiles.path())
                .env("TAPID_PROFILE_FIXTURE_CHILD", &profile)
                .env("LLVM_PROFILE_FILE", &profile)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
        return;
    };
    let mut command = fixture.command(&["--json", "capabilities"]);
    assert_eq!(
        command
            .get_envs()
            .find(|(key, _)| *key == "LLVM_PROFILE_FILE"),
        Some((
            std::ffi::OsStr::new("LLVM_PROFILE_FILE"),
            Some(std::path::absolute(&profile).unwrap().as_os_str())
        ))
    );
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_dir(fixture.project.path()).unwrap().count(),
        0
    );
}

#[test]
fn command_fixture_preserves_arguments_including_global_option_order() {
    let project = tapid_test_support::TempProject::new("command-fixture-arguments").unwrap();
    for args in [
        vec!["install", "--json"],
        vec!["--json", "install"],
        vec!["install", "--store-dir=chosen"],
        vec!["install", "--store-dir", "chosen"],
        vec!["run", "script", "--", "install"],
    ] {
        let command = isolated_command(project.path(), &args, true);
        assert_eq!(command.get_args().collect::<Vec<_>>(), args);
    }
}

#[test]
fn owned_command_fixture_keeps_environment_stable_and_cleans_up() {
    let (project_path, home_path) = {
        let fixture = CliFixture::new("owned-command-fixture");
        let before = fixture.command(&["--json", "install"]);
        let after = fixture.command(&["install", "--json"]);
        let env = |command: &std::process::Command| {
            command
                .get_envs()
                .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
                .collect::<Vec<_>>()
        };
        assert_eq!(env(&before), env(&after));
        let home = before
            .get_envs()
            .find(|(name, _)| *name == "HOME")
            .unwrap()
            .1
            .unwrap();
        let home = PathBuf::from(home);
        assert!(home.is_dir());
        (fixture.project.path().to_owned(), home)
    };
    assert!(!project_path.exists());
    assert!(!home_path.exists());
}
