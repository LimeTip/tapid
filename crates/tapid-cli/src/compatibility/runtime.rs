//! Explicit caller-authorized observation, never evidence of execution containment.
use super::Runtime;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, clap::Args)]
pub(crate) struct Options {
    /// Identify the first eligible external Node on PATH. Executes caller-selected code.
    #[arg(long, conflicts_with = "node_runtime")]
    identify_node: bool,
    /// Identify this absolute external Node executable. Executes caller-selected code.
    #[arg(long, value_name = "ABSOLUTE_PATH")]
    node_runtime: Option<PathBuf>,
}

impl Options {
    pub(crate) fn identify(&self, project: &Path, store: Option<&Path>) -> Runtime {
        if !self.identify_node && self.node_runtime.is_none() {
            return Runtime::default();
        }
        let mut runtime = Runtime {
            state: "failed",
            reason: "runtime_identity_invalid",
            ..Runtime::default()
        };
        let Ok(project) = fs::canonicalize(project) else {
            return runtime;
        };
        let store = store
            .map(Path::to_path_buf)
            .map_or_else(crate::application::install::default_store_root, Ok);
        let Ok(store) = store else { return runtime };
        let mut excluded = vec![project.clone()];
        if let Ok(store) = fs::canonicalize(store) {
            excluded.push(store);
        }
        let eligible = |path: &Path| {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
                || path.ancestors().any(|ancestor| {
                    fs::canonicalize(ancestor)
                        .ok()
                        .is_some_and(|p| excluded.iter().any(|root| p.starts_with(root)))
                })
            {
                return None;
            }
            let canonical = crate::run::canonical_node_executable(path).ok()?;
            (!excluded.iter().any(|root| canonical.starts_with(root))).then_some(canonical)
        };
        let executable = match &self.node_runtime {
            Some(path) => eligible(path),
            None => std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path).take(256).find_map(|dir| {
                    eligible(&dir.join(if cfg!(windows) { "node.exe" } else { "node" }))
                })
            }),
        };
        let Some(executable) = executable else {
            if self.node_runtime.is_none() {
                runtime.state = "not_found";
                runtime.reason = "no_eligible_runtime";
            }
            return runtime;
        };
        runtime.executable = Some(executable.clone());
        match probe(&executable, &excluded) {
            Ok(version) => {
                runtime.state = "identified";
                runtime.reason = "version_observed";
                runtime.version = Some(version);
            }
            Err(reason) => runtime.reason = reason,
        }
        runtime
    }
}

fn probe(
    executable: &Path,
    excluded: &[PathBuf],
) -> Result<tapid_core::PackageVersion, &'static str> {
    use tapid_runner::{
        ExecutionLimits, ExecutionRequest, FilesystemPolicy, SandboxMode, SandboxPolicy,
        Termination,
    };
    let temp = fs::canonicalize(std::env::temp_dir()).map_err(|_| "probe_setup_failed")?;
    if excluded.iter().any(|root| temp.starts_with(root)) {
        return Err("probe_setup_failed");
    }
    let capture = temp.join(format!(
        "tapid-node-identification-{}-{}",
        std::process::id(),
        crate::filesystem::atomic::unique_nonce()
    ));
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&capture).map_err(|_| "probe_setup_failed")?;
    let result = (|| {
        let root = fs::canonicalize(&capture).map_err(|_| "probe_setup_failed")?;
        let policy = SandboxPolicy::new(
            SandboxMode::Disabled,
            FilesystemPolicy::new(vec![], vec![]).map_err(|_| "probe_setup_failed")?,
            false,
            vec![],
            false,
            ExecutionLimits::new(Some(5), Some(4096), None, None)
                .map_err(|_| "probe_setup_failed")?,
        )
        .map_err(|_| "probe_setup_failed")?;
        // No ambient environment is forwarded. The runner supplies only its controlled PATH
        // and the required system environment on Windows. This is explicitly uncontained.
        let request = ExecutionRequest::builder(executable)
            .args(["--version"])
            .project_root(&root)
            .policy(policy)
            .build()
            .map_err(|_| "probe_setup_failed")?;
        match tapid_runner::execute_uncontained(&request, &root)
            .map_err(|_| "probe_launch_failed")?
        {
            Termination::Exited(0) => {}
            Termination::TimedOut => return Err("probe_timed_out"),
            Termination::OutputLimitExceeded => return Err("probe_output_limit"),
            _ => return Err("probe_unsuccessful"),
        }
        // Recheck after exit: a short-lived executable can exceed the polling ceiling.
        let stdout = crate::filesystem::read::read_bounded_file(&root.join("stdout"), 4096)
            .map_err(|_| "probe_output_limit")?;
        let stderr = crate::filesystem::read::read_bounded_file(&root.join("stderr"), 4096)
            .map_err(|_| "probe_output_limit")?;
        if stdout.len() + stderr.len() > 4096 {
            return Err("probe_output_limit");
        }
        if !stderr.is_empty() {
            return Err("probe_malformed_version");
        }
        let text = std::str::from_utf8(&stdout).map_err(|_| "probe_malformed_version")?;
        let version = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text);
        version
            .strip_prefix('v')
            .ok_or("probe_malformed_version")?
            .parse()
            .map_err(|_| "probe_malformed_version")
    })();
    let _ = fs::remove_dir_all(capture);
    result
}
