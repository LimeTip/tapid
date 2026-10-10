//! Credential-free HTTPS Git transport. No checkout or package code executes.
use crate::transport::{
    STANDARD_ARTIFACT_MAX_RESPONSE_BYTES, STANDARD_METADATA_MAX_RESPONSE_BYTES,
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tapid_core::GitRepository;

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
        self.fetch_with(scratch, |command| {
            let output = command.output().map_err(|_| "cannot execute Git")?;
            if !output.status.success() {
                return Err(
                    "Git command failed; repository or pinned commit is unavailable".into(),
                );
            }
            if output.stdout.len() > STANDARD_METADATA_MAX_RESPONSE_BYTES {
                return Err("Git metadata exceeds 32 MiB".into());
            }
            Ok(output.stdout)
        })
    }

    fn fetch_with(
        &self,
        scratch: &Path,
        mut run: impl FnMut(&mut Command) -> Result<Vec<u8>, String>,
    ) -> Result<(String, Vec<u8>), String> {
        let program = git_program()?;
        let empty_config = scratch.join("empty-config");
        fs::write(&empty_config, b"").map_err(|_| "cannot create isolated Git configuration")?;
        let bare = scratch.join("repository.git");
        let mut command = git_command(&program, scratch, &empty_config);
        command.args(["init", "--bare", "--template="]).arg(&bare);
        run(&mut command)?;
        let mut command = git_command(&program, scratch, &empty_config);
        command
            .arg("--git-dir")
            .arg(&bare)
            .args([
                "fetch",
                "--depth=1",
                "--no-tags",
                "--no-recurse-submodules",
                "--",
            ])
            .arg(self.repository.as_str())
            .arg(&self.reference);
        run(&mut command)?;
        let mut command = git_command(&program, scratch, &empty_config);
        command
            .arg("--git-dir")
            .arg(&bare)
            .args(["rev-parse", "--verify", "FETCH_HEAD^{commit}"]);
        let commit = String::from_utf8(run(&mut command)?)
            .map_err(|_| "Git returned an invalid commit")?
            .trim()
            .to_owned();
        if !matches!(commit.len(), 40 | 64)
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
        if run(&mut command)?
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(b"160000 "))
        {
            return Err("Git submodules are unsupported in copied dependencies".into());
        }
        let archive = scratch.join("artifact.tar");
        let mut command = git_command(&program, scratch, &empty_config);
        command
            .arg("--git-dir")
            .arg(&bare)
            .args(["archive", "--format=tar", "--prefix=package/"])
            .arg(format!("--output={}", archive.display()))
            .arg(&commit);
        run(&mut command)?;
        let file = fs::File::open(&archive).map_err(|_| "cannot read Git archive")?;
        if file
            .metadata()
            .map_err(|_| "cannot inspect Git archive")?
            .len()
            > STANDARD_ARTIFACT_MAX_RESPONSE_BYTES as u64
        {
            return Err("Git archive exceeds 512 MiB".into());
        }
        let mut bytes = Vec::new();
        file.take(STANDARD_ARTIFACT_MAX_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read Git archive")?;
        if bytes.len() > STANDARD_ARTIFACT_MAX_RESPONSE_BYTES {
            return Err("Git archive exceeds 512 MiB".into());
        }
        Ok((commit, bytes))
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
            let result = spec.fetch_with(scratch.path(), |command| {
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
            .fetch_with(scratch.path(), |command| {
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
                if let Some(output) = args.iter().find_map(|arg| arg.strip_prefix("--output=")) {
                    assert_eq!(args.last(), Some(&commit));
                    fs::write(output, b"archive bytes").unwrap();
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
