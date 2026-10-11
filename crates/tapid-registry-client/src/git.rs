//! Credential-free HTTPS Git transport. No checkout or package code executes.
use crate::transport::{
    STANDARD_ARTIFACT_MAX_RESPONSE_BYTES, STANDARD_METADATA_MAX_RESPONSE_BYTES,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tapid_core::GitRepository;
mod http;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitDependency {
    repository: GitRepository,
    reference: String,
}

impl GitDependency {
    pub fn repository(&self) -> &GitRepository {
        &self.repository
    }
    pub fn reference(&self) -> &str {
        &self.reference
    }

    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec
            .strip_prefix("git+")
            .ok_or("only git+https dependencies are supported")?;
        let (repository, reference) = spec.split_once('#').unwrap_or((spec, "HEAD"));
        let repository = repository.parse::<GitRepository>().map_err(
            |_| "Git repository must be a credential-free HTTPS URL without query or fragment",
        )?;
        if reference.is_empty()
            || reference.len() > 256
            || reference.starts_with('-')
            || reference.ends_with('.')
            || reference.ends_with('/')
            || reference.ends_with(".lock")
            || reference.contains("..")
            || reference.contains("//")
            || !reference
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_./".contains(&byte))
        {
            return Err("Git reference must be a commit, branch, or tag that can be pinned safely; semver selectors and revision expressions are unsupported".into());
        }
        Ok(Self {
            repository,
            reference: reference.into(),
        })
    }

    /// Fetches and peels one reference, then archives that exact commit. The
    /// caller supplies a fresh private scratch directory, never a project/home.
    pub fn fetch(&self, scratch: &Path) -> Result<(String, Vec<u8>), String> {
        self.fetch_with(scratch, http::fetch(self)?, run_bounded)
    }

    fn fetch_with(
        &self,
        scratch: &Path,
        pack: http::FetchedPack,
        mut run: impl FnMut(&mut Command, usize) -> Result<Vec<u8>, String>,
    ) -> Result<(String, Vec<u8>), String> {
        let program = git_program()?;
        let empty_config = scratch.join("empty-config");
        fs::write(&empty_config, b"").map_err(|_| "cannot create isolated Git configuration")?;
        let bare = scratch.join("repository.git");
        let mut command = git_command(&program, scratch, &empty_config);
        command
            .args(["init", "--bare", "--template="])
            .arg(format!("--object-format={}", pack.format))
            .arg(&bare);
        run(&mut command, STANDARD_METADATA_MAX_RESPONSE_BYTES)?;
        let pack_path = scratch.join("artifact.pack");
        fs::write(&pack_path, &pack.bytes).map_err(|_| "cannot stage bounded Git pack")?;
        drop(pack.bytes);
        let mut command = git_command(&program, scratch, &empty_config);
        command
            .arg("--git-dir")
            .arg(&bare)
            .args([
                "index-pack",
                "--stdin",
                "--fsck-objects",
                "--index-version=2",
            ])
            .arg(format!(
                "--max-input-size={STANDARD_ARTIFACT_MAX_RESPONSE_BYTES}"
            ))
            .stdin(Stdio::from(
                fs::File::open(&pack_path).map_err(|_| "cannot read bounded Git pack")?,
            ));
        run(&mut command, STANDARD_METADATA_MAX_RESPONSE_BYTES)?;
        fs::remove_file(&pack_path).map_err(|_| "cannot remove staged Git pack")?;
        let mut command = git_command(&program, scratch, &empty_config);
        command
            .arg("--git-dir")
            .arg(&bare)
            .args(["rev-parse", "--verify"])
            .arg(format!("{}^{{commit}}", pack.object));
        let commit = String::from_utf8(run(&mut command, STANDARD_METADATA_MAX_RESPONSE_BYTES)?)
            .map_err(|_| "Git returned an invalid commit")?
            .trim()
            .to_owned();
        if commit.len() != if pack.format == "sha256" { 64 } else { 40 }
            || !commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Git reference did not peel to a full commit identity".into());
        }
        if matches!(self.reference.len(), 40 | 64)
            && self.reference.bytes().all(|byte| byte.is_ascii_hexdigit())
            && !self.reference.eq_ignore_ascii_case(&commit)
        {
            return Err("Git returned a different commit than the requested pin".into());
        }
        let mut command = git_command(&program, scratch, &empty_config);
        command
            .arg("--git-dir")
            .arg(&bare)
            .args(["ls-tree", "-r"])
            .arg(&commit);
        if run(&mut command, STANDARD_METADATA_MAX_RESPONSE_BYTES)?
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(b"160000 "))
        {
            return Err("Git submodules are unsupported in copied dependencies".into());
        }
        let mut command = git_command(&program, scratch, &empty_config);
        command
            .arg("--git-dir")
            .arg(&bare)
            .args(["archive", "--format=tar", "--prefix=package/"])
            .arg(&commit);
        let bytes = run(&mut command, STANDARD_ARTIFACT_MAX_RESPONSE_BYTES)?;
        Ok((commit, bytes))
    }
}

fn run_bounded(command: &mut Command, limit: usize) -> Result<Vec<u8>, String> {
    run_with_deadline(command, limit, std::time::Duration::from_secs(120))
}

fn run_with_deadline(
    command: &mut Command,
    limit: usize,
    timeout: std::time::Duration,
) -> Result<Vec<u8>, String> {
    let start = std::time::Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| "cannot execute Git")?;
    let stdout = child.stdout.take().ok_or("cannot read Git output")?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let _ = tx.send(crate::transport::read_bounded(stdout, limit));
    });
    let result = match rx.recv_timeout(timeout) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(_)) => Err("Git output exceeds its limit or cannot be read".into()),
        Err(_) => Err("Git command exceeded its deadline".into()),
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return result;
    }
    let _ = reader.join();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return result,
            Ok(Some(_)) => {
                return Err(
                    "Git command failed; repository or pinned commit is unavailable".into(),
                );
            }
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Git command exceeded its deadline or cannot be waited for".into());
            }
        }
    }
}

fn git_program() -> Result<PathBuf, String> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|path| path.is_absolute())
    {
        let candidate = directory.join(if cfg!(windows) { "git.exe" } else { "git" });
        if let Ok(path) = fs::canonicalize(candidate)
            && path.is_file()
        {
            return Ok(path);
        }
    }
    Err("git executable is required for HTTPS Git dependencies".into())
}

fn git_command(program: &Path, scratch: &Path, empty_config: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .current_dir(scratch)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("HOME", scratch)
        .env("USERPROFILE", scratch)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", empty_config)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "credential.helper=",
            "-c",
            "core.hooksPath=disabled-hooks",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "http.followRedirects=false",
            "-c",
            "http.lowSpeedLimit=1",
            "-c",
            "http.lowSpeedTime=30",
        ]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn git_specs_reject_credentials_protocols_and_unsafe_revision_expressions() {
        for spec in [
            "git+ssh://example.test/repo",
            "git+https://user:secret@example.test/repo#main",
            "git+https://example.test/repo?secret=token#main",
            "git+https://example.test/repo#semver:^1",
            "git+https://example.test/repo#HEAD~1",
            "git+https://example.test/repo#--upload-pack=bad",
        ] {
            let error = GitDependency::parse(spec).unwrap_err();
            assert!(!error.contains("secret"));
        }
        assert_eq!(
            GitDependency::parse("git+https://example.test/repo#refs/tags/v1")
                .unwrap()
                .reference,
            "refs/tags/v1"
        );
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    fn fixture_pack(spec: &GitDependency) -> http::FetchedPack {
        http::FetchedPack {
            object: "a".repeat(if spec.reference().len() == 64 { 64 } else { 40 }),
            format: if spec.reference().len() == 64 {
                "sha256"
            } else {
                "sha1"
            },
            bytes: Vec::new(),
        }
    }

    #[test]
    fn git_output_limits_and_deadlines_terminate_and_reap_the_child() {
        if let Some(mode) = std::env::var_os("TAPID_GIT_OUTPUT_PROBE") {
            if mode == "output" {
                use std::io::Write;
                loop {
                    if std::io::stdout().write_all(&[b'x'; 4096]).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(30));
            return;
        }
        for mode in ["output", "timeout"] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "git::transport_tests::git_output_limits_and_deadlines_terminate_and_reap_the_child", "--nocapture"])
                .env("TAPID_GIT_OUTPUT_PROBE", mode).stderr(Stdio::null());
            let error =
                run_with_deadline(&mut child, 1024, std::time::Duration::from_secs(2)).unwrap_err();
            assert!(
                error.contains(if mode == "output" {
                    "exceeds its limit"
                } else {
                    "deadline"
                }),
                "{error}"
            );
        }
    }

    #[test]
    fn git_transport_initializes_the_selected_object_format() {
        let scratch = tapid_test_support::TempProject::new("git-sha256-init").unwrap();
        let spec =
            GitDependency::parse(&format!("git+https://example.test/repo#{}", "a".repeat(64)))
                .unwrap();
        let _ = spec.fetch_with(scratch.path(), fixture_pack(&spec), |command, _limit| {
            let args = command.get_args().collect::<Vec<_>>();
            if args.contains(&std::ffi::OsStr::new("init")) {
                assert!(args.contains(&std::ffi::OsStr::new("--object-format=sha256")));
            }
            Err("stop after init".into())
        });
    }

    #[test]
    fn git_transport_never_delegates_unbounded_network_writes_to_git() {
        let scratch = tapid_test_support::TempProject::new("git-bounded-fetch").unwrap();
        let spec = GitDependency::parse("git+https://example.test/repo#main").unwrap();
        let _ = spec.fetch_with(scratch.path(), fixture_pack(&spec), |command, _limit| {
            let args = command.get_args().collect::<Vec<_>>();
            assert!(!args.contains(&std::ffi::OsStr::new("fetch")));
            if args.contains(&std::ffi::OsStr::new("rev-parse")) {
                return Ok("a".repeat(40).into_bytes());
            }
            if args.contains(&std::ffi::OsStr::new("archive")) {
                assert!(
                    !args
                        .iter()
                        .any(|arg| arg.to_string_lossy().starts_with("--output="))
                );
                return Err("stop after archive".into());
            }
            Ok(Vec::new())
        });
    }

    #[test]
    fn git_transport_rejects_unpinnable_refs_changed_commits_and_submodules() {
        for (reference, peeled, tree) in [
            ("main".into(), "not-a-commit".into(), Vec::new()),
            ("a".repeat(40), "b".repeat(40), Vec::new()),
            (
                "main".into(),
                "a".repeat(40),
                b"160000 commit object\tsubmodule\n".to_vec(),
            ),
        ] {
            let scratch = tapid_test_support::TempProject::new("git-invalid-pin").unwrap();
            let spec =
                GitDependency::parse(&format!("git+https://example.test/repo.git#{reference}"))
                    .unwrap();
            let result = spec.fetch_with(scratch.path(), fixture_pack(&spec), |command, _limit| {
                let args = command.get_args().collect::<Vec<_>>();
                assert!(!args.contains(&std::ffi::OsStr::new("archive")));
                if args.contains(&std::ffi::OsStr::new("rev-parse")) {
                    return Ok(peeled.as_bytes().to_vec());
                }
                if args.contains(&std::ffi::OsStr::new("ls-tree")) {
                    return Ok(tree.clone());
                }
                Ok(Vec::new())
            });
            assert!(result.is_err());
        }
    }

    #[test]
    fn git_transport_peels_and_archives_only_the_exact_commit() {
        let scratch = tapid_test_support::TempProject::new("git-transport").unwrap();
        let spec = GitDependency::parse("git+https://example.test/repo.git#release").unwrap();
        let commit = "a".repeat(40);
        let mut calls = 0;
        let (pinned, bytes) = spec
            .fetch_with(scratch.path(), fixture_pack(&spec), |command, _limit| {
                let args = command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                assert!(args.contains(&"protocol.allow=never".into()));
                assert!(args.contains(&"protocol.https.allow=always".into()));
                assert!(args.contains(&"http.followRedirects=false".into()));
                assert_eq!(command.get_current_dir(), Some(scratch.path()));
                for (name, _) in command.get_envs() {
                    assert!(
                        [
                            "HOME",
                            "USERPROFILE",
                            "GIT_CONFIG_NOSYSTEM",
                            "GIT_CONFIG_GLOBAL",
                            "GIT_TERMINAL_PROMPT"
                        ]
                        .contains(&name.to_str().unwrap())
                    );
                }
                calls += 1;
                if args.contains(&"fetch".into()) {
                    assert!(args.contains(&"release".into()));
                }
                if args.contains(&"rev-parse".into()) {
                    return Ok(format!("{commit}\n").into_bytes());
                }
                if args.contains(&"archive".into()) {
                    assert_eq!(args.last(), Some(&commit));
                    return Ok(b"archive bytes".to_vec());
                }
                Ok(Vec::new())
            })
            .unwrap();
        assert_eq!(calls, 5);
        assert_eq!(pinned, commit);
        assert_eq!(bytes, b"archive bytes");
    }

    #[test]
    fn git_transport_clears_inherited_configuration_and_credentials() {
        if std::env::var_os("TAPID_GIT_ENV_PROBE").is_some() {
            let scratch = tapid_test_support::TempProject::new("git-environment").unwrap();
            let empty = scratch.write("empty-config", b"").unwrap();
            let mut command = git_command(&git_program().unwrap(), scratch.path(), &empty);
            command.args(["config", "--get", "http.extraHeader"]);
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            return;
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "git::transport_tests::git_transport_clears_inherited_configuration_and_credentials", "--nocapture"])
            .env("TAPID_GIT_ENV_PROBE", "1")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env("GIT_CONFIG_VALUE_0", "Authorization: dummy-test-credential")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
