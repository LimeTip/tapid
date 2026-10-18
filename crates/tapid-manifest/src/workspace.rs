use crate::{MAX_MANIFEST_BYTES, PackageManifest};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceMember {
    name: String,
    path: PathBuf,
    manifest: PackageManifest,
}

impl WorkspaceMember {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    root: PackageManifest,
    root_path: PathBuf,
    members: Vec<WorkspaceMember>,
}

impl Workspace {
    pub fn discover(project_dir: &Path) -> Result<Self, String> {
        Self::discover_typed(project_dir).map_err(|error| error.to_string())
    }

    /// Discover members without discarding the location of a manifest parse failure.
    pub fn discover_typed(project_dir: &Path) -> Result<Self, crate::WorkspaceError> {
        Self::discover_bounded(project_dir, usize::MAX, usize::MAX)
    }

    /// Bound candidate manifests and filesystem expansion work before parsing members.
    /// Overlapping patterns count toward the candidate limit before deduplication.
    pub fn discover_bounded(
        project_dir: &Path,
        max_candidates: usize,
        max_expansion_steps: usize,
    ) -> Result<Self, crate::WorkspaceError> {
        Self::discover_bounded_with_member_source(
            project_dir,
            max_candidates,
            max_expansion_steps,
            |_, _| {},
        )
    }

    /// Observe the exact member source parsed during bounded discovery.
    /// The borrowed source is discarded after each member; it is not retained in
    /// the workspace. Observations before a discovery error are partial.
    pub fn discover_bounded_with_member_source(
        project_dir: &Path,
        max_candidates: usize,
        max_expansion_steps: usize,
        mut observe: impl FnMut(&Path, &str),
    ) -> Result<Self, crate::WorkspaceError> {
        let project_dir = fs::canonicalize(project_dir).map_err(|error| {
            format!(
                "cannot resolve workspace root {}: {error}",
                project_dir.display()
            )
        })?;
        if !project_dir.is_dir() {
            return Err(format!(
                "workspace root is not a directory: {}",
                project_dir.display()
            )
            .into());
        }
        let root_manifest_path = project_dir.join("package.json");
        let root_metadata = fs::symlink_metadata(&root_manifest_path)
            .map_err(|error| format!("cannot inspect workspace root manifest: {error}"))?;
        if !root_metadata.file_type().is_file() {
            return Err("workspace root package.json must be a regular file"
                .to_owned()
                .into());
        }
        let root_path = contained_path(&project_dir, &root_manifest_path)?;
        let root_text = read_file(&root_path)?;
        let root = PackageManifest::parse(&root_text).map_err(|error| {
            crate::WorkspaceError::Manifest {
                path: root_path.clone(),
                diagnostic: Box::new(error.diagnostic(&root_text)),
                error: Box::new(error),
            }
        })?;
        let patterns = workspace_patterns(root.extra.get("workspaces"))?;
        let mut paths = Vec::new();
        let mut remaining = max_expansion_steps;
        for pattern in patterns {
            paths.extend(expand_pattern_bounded(
                &project_dir,
                &pattern,
                max_candidates - paths.len(),
                &mut remaining,
            )?);
        }
        paths = paths
            .into_iter()
            .map(|path| {
                // Inspect the declared manifest before canonicalization loses symlink identity.
                let metadata = fs::symlink_metadata(&path).map_err(|error| {
                    format!(
                        "cannot inspect workspace manifest {}: {error}",
                        path.display()
                    )
                })?;
                if !metadata.file_type().is_file() {
                    return Err(format!(
                        "workspace manifest must be a regular file, not a symlink: {}",
                        path.display()
                    ));
                }
                contained_path(&project_dir, &path)
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        paths.dedup();
        let mut members = Vec::new();
        let mut names = BTreeSet::from([root.name().to_string()]);
        for path in paths {
            let canonical = fs::canonicalize(&path).map_err(|error| {
                format!(
                    "cannot resolve workspace manifest {}: {error}",
                    path.display()
                )
            })?;
            if !canonical.starts_with(&project_dir) {
                return Err(format!(
                    "workspace member escapes workspace root: {}",
                    path.display()
                )
                .into());
            }
            let member_root = canonical
                .parent()
                .ok_or_else(|| "workspace member manifest has no parent".to_owned())?;
            let relative_member_root = member_root.strip_prefix(&project_dir).map_err(|_| {
                format!(
                    "workspace member escapes workspace root: {}",
                    path.display()
                )
            })?;
            if relative_member_root
                .components()
                .any(|component| component.as_os_str() == "node_modules")
            {
                return Err(format!(
                    "workspace member may not be inside root node_modules: {}",
                    member_root.display()
                )
                .into());
            }
            let text = read_file(&canonical)?;
            let manifest =
                PackageManifest::parse(&text).map_err(|error| crate::WorkspaceError::Manifest {
                    path: canonical.clone(),
                    diagnostic: Box::new(error.diagnostic(&text)),
                    error: Box::new(error),
                })?;
            if !names.insert(manifest.name().to_string()) {
                return Err(
                    format!("duplicate workspace package name '{}'", manifest.name()).into(),
                );
            }
            observe(&canonical, &text);
            members.push(WorkspaceMember {
                name: manifest.name().to_string(),
                path: canonical,
                manifest,
            });
        }
        members.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
        Ok(Self {
            root,
            root_path,
            members,
        })
    }

    pub fn root(&self) -> &PackageManifest {
        &self.root
    }
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }
    pub fn members(&self) -> &[WorkspaceMember] {
        &self.members
    }

    pub fn select(&self, name: Option<&str>) -> Result<&PackageManifest, String> {
        match name {
            None => Ok(&self.root),
            Some("") => Err("workspace selector cannot be empty".to_owned()),
            Some(name) => self
                .members
                .iter()
                .find(|member| member.name == name)
                .map(|member| &member.manifest)
                .ok_or_else(|| format!("workspace member '{name}' was not found")),
        }
    }

    pub fn select_path(&self, name: Option<&str>) -> Result<&Path, String> {
        match name {
            None => Ok(&self.root_path),
            Some("") => Err("workspace selector cannot be empty".to_owned()),
            Some(name) => self
                .members
                .iter()
                .find(|member| member.name == name)
                .map(|member| member.path.as_path())
                .ok_or_else(|| format!("workspace member '{name}' was not found")),
        }
    }
}

fn contained_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        format!(
            "cannot canonicalize workspace path {}: {error}",
            path.display()
        )
    })?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "workspace path is outside project: {}",
            path.display()
        ));
    }
    Ok(canonical)
}

fn read_file(path: &Path) -> Result<String, String> {
    #[cfg(all(test, unix))]
    read_race_tests::before_open(path);
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Type checks before opening cannot exclude replacement by a symlink or
        // FIFO. Reject symlinks and never wait for a FIFO writer, then check the
        // opened descriptor's type before reading any bytes.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(format!(
            "workspace manifest must be a regular file no larger than 32 MiB: {}",
            path.display()
        ));
    }
    // Bound the read itself as well as metadata, so growth after inspection cannot
    // allocate the whole replacement content.
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "workspace manifest exceeds 32 MiB: {}",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

#[cfg(all(test, unix))]
mod read_race_tests {
    use super::*;
    use std::{cell::RefCell, os::unix::fs::symlink};
    use tapid_test_support::TempProject;

    thread_local! {
        static REPLACEMENT: RefCell<Option<(PathBuf, PathBuf)>> = const { RefCell::new(None) };
    }

    struct FaultGuard;
    impl Drop for FaultGuard {
        fn drop(&mut self) {
            REPLACEMENT.with(|pending| pending.borrow_mut().take());
        }
    }

    pub(super) fn before_open(path: &Path) {
        let replacement = REPLACEMENT.with(|pending| {
            let mut pending = pending.borrow_mut();
            if pending.as_ref().is_some_and(|(target, _)| target == path) {
                pending.take()
            } else {
                None
            }
        });
        if let Some((target, replacement)) = replacement {
            fs::rename(replacement, target).unwrap();
        }
    }

    fn project() -> TempProject {
        let project = TempProject::new("workspace-open-race").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["member"]}"#,
            )
            .unwrap();
        project
            .write(
                "member/package.json",
                br#"{"name":"member","version":"1.0.0"}"#,
            )
            .unwrap();
        project
    }

    #[test]
    fn discovery_rejects_symlink_replacements_before_open() {
        for manifest in ["package.json", "member/package.json"] {
            let project = project();
            let external = TempProject::new("workspace-open-external").unwrap();
            let source = br#"{"name":"external","version":"1.0.0"}"#;
            let external_manifest = external.write("package.json", source).unwrap();
            let replacement = project.path().join("replacement");
            symlink(&external_manifest, &replacement).unwrap();
            let target = project.path().join(manifest).canonicalize().unwrap();
            REPLACEMENT.with(|pending| *pending.borrow_mut() = Some((target, replacement)));
            let _guard = FaultGuard;
            assert!(
                Workspace::discover_typed(project.path()).is_err(),
                "followed replacement for {manifest}"
            );
            assert_eq!(fs::read(external_manifest).unwrap(), source);
        }
    }

    #[test]
    fn discovery_rejects_fifo_replacements_without_waiting_for_a_writer() {
        const ROOT_ENV: &str = "TAPID_TEST_WORKSPACE_FIFO_ROOT";
        const MANIFEST_ENV: &str = "TAPID_TEST_WORKSPACE_FIFO_MANIFEST";
        if let Some(root) = std::env::var_os(ROOT_ENV) {
            let root = PathBuf::from(root);
            let manifest = std::env::var_os(MANIFEST_ENV).unwrap();
            let target = root.join(manifest).canonicalize().unwrap();
            REPLACEMENT.with(|pending| {
                *pending.borrow_mut() = Some((target, root.join("replacement")));
            });
            let _guard = FaultGuard;
            assert!(Workspace::discover_typed(&root).is_err());
            return;
        }

        // A blocking open must fail this test rather than hang the test runner.
        for manifest in ["package.json", "member/package.json"] {
            let project = project();
            assert!(
                std::process::Command::new("mkfifo")
                    .arg(project.path().join("replacement"))
                    .status()
                    .unwrap()
                    .success()
            );
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "workspace::read_race_tests::discovery_rejects_fifo_replacements_without_waiting_for_a_writer", "--nocapture"])
                .env(ROOT_ENV, project.path())
                .env(MANIFEST_ENV, manifest)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if child.try_wait().unwrap().is_some() {
                    let output = child.wait_with_output().unwrap();
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("discovery blocked opening replacement FIFO for {manifest}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

fn workspace_patterns(workspaces: Option<&Value>) -> Result<Vec<String>, String> {
    let value = match workspaces {
        None => return Ok(Vec::new()),
        Some(Value::String(pattern)) => return Ok(vec![pattern.to_owned()]),
        Some(Value::Array(values)) => values,
        Some(Value::Object(object)) => match object.get("packages") {
            Some(Value::String(pattern)) => return Ok(vec![pattern.to_owned()]),
            Some(Value::Array(values)) => values,
            _ => {
                return Err("package.json workspaces.packages must be a string or array".to_owned());
            }
        },
        Some(_) => {
            return Err("package.json workspaces must be a string, array, or object".to_owned());
        }
    };
    value
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "package.json workspace patterns must be strings".to_owned())
        })
        .collect()
}

#[cfg(test)]
fn expand_pattern(root: &Path, pattern: &str) -> Result<Vec<PathBuf>, String> {
    let mut remaining = usize::MAX;
    expand_pattern_bounded(root, pattern, usize::MAX, &mut remaining)
        .map_err(|error| error.to_string())
}

fn expansion_step(remaining: &mut usize) -> Result<(), crate::WorkspaceError> {
    *remaining = remaining
        .checked_sub(1)
        .ok_or(crate::WorkspaceError::DiscoveryLimitExceeded)?;
    Ok(())
}

fn expand_pattern_bounded(
    root: &Path,
    pattern: &str,
    max_candidates: usize,
    remaining: &mut usize,
) -> Result<Vec<PathBuf>, crate::WorkspaceError> {
    expansion_step(remaining)?;
    let relative = Path::new(pattern);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("workspace pattern is outside project: {pattern}").into());
    }
    if !relative
        .components()
        .any(|component| matches!(component, std::path::Component::Normal(_)))
        || pattern.contains(['?', '[', ']', '{', '}', '!', '(', ')', '\\'])
        || (cfg!(windows) && pattern.contains(':'))
        || relative.components().any(|component| {
            let name = component.as_os_str().to_string_lossy();
            name.contains('*') && name != "*"
        })
    {
        return Err(format!(
            "unsupported workspace pattern '{pattern}'; use a relative directory path or a whole path component '*' for one directory level"
        ).into());
    }
    let mut paths = vec![root.to_path_buf()];
    for component in relative.components() {
        expansion_step(remaining)?;
        let name = component.as_os_str();
        if name == "*" {
            let mut next = Vec::new();
            for path in paths {
                let path = contained_path(root, &path)?;
                let entries = fs::read_dir(&path).map_err(|error| {
                    format!(
                        "cannot read workspace directory {}: {error}",
                        path.display()
                    )
                })?;
                for entry in entries {
                    expansion_step(remaining)?;
                    let entry = entry
                        .map_err(|error| format!("cannot inspect workspace directory: {error}"))?;
                    let file_type = entry
                        .file_type()
                        .map_err(|error| format!("cannot inspect workspace directory: {error}"))?;
                    let candidate = entry.path();
                    if file_type.is_symlink() {
                        let target_metadata = fs::metadata(&candidate).map_err(|error| {
                            format!(
                                "cannot inspect workspace symlink target {}: {error}",
                                candidate.display()
                            )
                        })?;
                        if target_metadata.is_file() {
                            continue;
                        }
                        if !target_metadata.is_dir() {
                            return Err(format!(
                                "workspace glob encountered an unsupported symlink target: {}",
                                candidate.display()
                            )
                            .into());
                        }
                    } else if !file_type.is_dir() {
                        continue;
                    }
                    next.push(contained_path(root, &candidate)?);
                }
            }
            paths = next;
        } else {
            for path in &mut paths {
                expansion_step(remaining)?;
                path.push(name);
            }
        }
    }
    let mut manifests = Vec::new();
    for path in paths {
        expansion_step(remaining)?;
        let manifest = path.join("package.json");
        match fs::symlink_metadata(&manifest) {
            // Keep every existing entry for the regular-file validation in discovery.
            // Following the final component here would hide dangling symlinks.
            Ok(_) => {
                if manifests.len() == max_candidates {
                    return Err(crate::WorkspaceError::DiscoveryLimitExceeded);
                }
                manifests.push(manifest);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot inspect workspace manifest {}: {error}",
                    manifest.display()
                )
                .into());
            }
        }
    }
    manifests.sort();
    Ok(manifests)
}

#[cfg(test)]
mod tests {
    use super::{Workspace, expand_pattern};
    use proptest::prelude::*;
    use std::{fs, path::Path};
    use tapid_test_support::TempProject;

    #[test]
    fn bounded_discovery_counts_entries_without_manifests_and_across_patterns() {
        let project = TempProject::new("workspace-discovery-work-limit").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
            )
            .unwrap();
        for i in 0..8 {
            project
                .write(format!("packages/file-{i}"), b"not a workspace")
                .unwrap();
        }
        assert!(matches!(
            Workspace::discover_bounded(project.path(), 1, 6),
            Err(crate::WorkspaceError::DiscoveryLimitExceeded)
        ));
        assert!(
            Workspace::discover(project.path())
                .unwrap()
                .members()
                .is_empty()
        );
        project.write("package.json", br#"{"name":"root","version":"1.0.0","workspaces":["missing","missing","missing"]}"#).unwrap();
        assert!(matches!(
            Workspace::discover_bounded(project.path(), 1, 8),
            Err(crate::WorkspaceError::DiscoveryLimitExceeded)
        ));
    }

    #[test]
    fn bounded_discovery_preserves_selection_at_the_candidate_limit() {
        let project = TempProject::new("workspace-discovery-candidate-limit").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
            )
            .unwrap();
        for name in ["a", "b"] {
            project
                .write(
                    format!("packages/{name}/package.json"),
                    format!(r#"{{"name":"{name}","version":"1.0.0"}}"#).as_bytes(),
                )
                .unwrap();
        }
        assert!(matches!(
            Workspace::discover_bounded(project.path(), 1, 100),
            Err(crate::WorkspaceError::DiscoveryLimitExceeded)
        ));
        let workspace = Workspace::discover_bounded(project.path(), 2, 100).unwrap();
        assert_eq!(workspace, Workspace::discover(project.path()).unwrap());
        assert_eq!(workspace.select(Some("b")).unwrap().name().as_str(), "b");
    }

    #[test]
    fn unsupported_workspace_patterns_fail_instead_of_silently_dropping_members() {
        for pattern in [
            "packages/**",
            "packages/u*",
            "packages/?i",
            "packages/[ab]",
            "{apps,packages}/*",
            "!packages/private",
            "packages/@(ui)",
            "packages/+(ui)",
            "packages/\\*",
            "",
            ".",
            "./",
        ] {
            let project = TempProject::new("unsupported-workspace-pattern").unwrap();
            project
                .write(
                    "package.json",
                    &serde_json::to_vec(&serde_json::json!({
                        "name": "root", "version": "1.0.0", "workspaces": [pattern]
                    }))
                    .unwrap(),
                )
                .unwrap();
            let error = Workspace::discover(project.path()).unwrap_err();
            assert!(
                error.contains("unsupported workspace pattern"),
                "{pattern}: {error}"
            );
            assert!(error.contains("whole path component '*'"), "{error}");
        }
    }

    #[test]
    fn workspace_patterns_support_literal_paths_and_multiple_single_level_wildcards() {
        let project = TempProject::new("supported-workspace-patterns").unwrap();
        project.write("package.json", br#"{"name":"root","version":"1.0.0","workspaces":["apps/news","packages/*/*","apps/*"]}"#).unwrap();
        for (path, name) in [
            ("apps/news", "news"),
            ("packages/group/ui", "ui"),
            ("packages/group/tools", "tools"),
        ] {
            project
                .write(
                    format!("{path}/package.json"),
                    format!(r#"{{"name":"{name}","version":"1.0.0"}}"#).as_bytes(),
                )
                .unwrap();
        }
        let workspace = Workspace::discover(project.path()).unwrap();
        assert_eq!(
            workspace
                .members()
                .iter()
                .map(|member| member.name())
                .collect::<Vec<_>>(),
            ["news", "tools", "ui"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_patterns_allow_colons_in_literal_paths() {
        let project = TempProject::new("colon-workspace-pattern").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["packages/foo:bar"]}"#,
            )
            .unwrap();
        project
            .write(
                "packages/foo:bar/package.json",
                br#"{"name":"foo-bar","version":"1.0.0"}"#,
            )
            .unwrap();

        let workspace = Workspace::discover(project.path()).unwrap();

        assert_eq!(workspace.members().len(), 1);
        assert_eq!(workspace.members()[0].name(), "foo-bar");
    }

    #[test]
    fn workspace_ignores_absent_manifests_but_rejects_directory_manifest_entries() {
        let project = TempProject::new("workspace-manifest-directory").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
            )
            .unwrap();
        fs::create_dir_all(project.path().join("packages/ui")).unwrap();
        assert!(
            Workspace::discover(project.path())
                .unwrap()
                .members()
                .is_empty()
        );
        fs::create_dir(project.path().join("packages/ui/package.json")).unwrap();
        let error = Workspace::discover(project.path()).unwrap_err();
        assert!(
            error.contains("workspace manifest must be a regular file"),
            "{error}"
        );
    }

    proptest! {
        #[test]
        fn workspace_patterns_with_parent_components_are_always_rejected(components in prop::collection::vec("[a-z]{1,8}", 0..4)) {
            let pattern = if components.is_empty() { "..".to_owned() } else { format!("{}/..", components.join("/")) };
            prop_assert!(expand_pattern(Path::new("/tmp/tapid-workspace"), &pattern).is_err());
        }
    }
}

#[cfg(all(test, unix))]
mod containment_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tapid_test_support::TempProject;

    #[test]
    fn workspace_rejects_symlinked_member_manifest_even_with_contained_target() {
        let project = TempProject::new("workspace-manifest-symlink").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
            )
            .unwrap();
        project
            .write(
                "packages/ui/real.json",
                br#"{"name":"ui","version":"1.0.0"}"#,
            )
            .unwrap();
        symlink("real.json", project.path().join("packages/ui/package.json")).unwrap();
        let error = Workspace::discover(project.path()).unwrap_err();
        assert!(
            error.contains("workspace manifest must be a regular file, not a symlink"),
            "{error}"
        );
    }

    #[test]
    fn workspace_rejects_dangling_and_directory_target_manifest_symlinks() {
        for pattern in ["packages/*", "packages/ui"] {
            for target in ["missing.json", "directory"] {
                let project = TempProject::new("workspace-nonfile-manifest").unwrap();
                project
                    .write(
                        "package.json",
                        format!(
                            r#"{{"name":"root","version":"1.0.0","workspaces":["{pattern}"]}}"#
                        )
                        .as_bytes(),
                    )
                    .unwrap();
                fs::create_dir_all(project.path().join("packages/ui/directory")).unwrap();
                symlink(target, project.path().join("packages/ui/package.json")).unwrap();
                let error = Workspace::discover(project.path()).unwrap_err();
                assert!(
                    error.contains("workspace manifest must be a regular file, not a symlink"),
                    "{pattern}, {target}: {error}"
                );
            }
        }
    }

    #[test]
    fn workspace_rejects_symlink_escape_before_parsing_member() {
        for pattern in ["packages/*", "packages/member"] {
            let project = TempProject::new("workspace-symlink-root").unwrap();
            let external = TempProject::new("workspace-symlink-external").unwrap();
            project
                .write(
                    "package.json",
                    format!(r#"{{"name":"root","version":"1.0.0","workspaces":["{pattern}"]}}"#)
                        .as_bytes(),
                )
                .unwrap();
            let manifest = external
                .write(
                    "member/package.json",
                    b"external content must not be parsed",
                )
                .unwrap();
            symlink(external.path(), project.path().join("packages")).unwrap();
            let error = Workspace::discover(project.path()).unwrap_err();
            assert!(error.contains("outside project"), "{error}");
            assert_eq!(
                fs::read(manifest).unwrap(),
                b"external content must not be parsed"
            );
        }
    }

    #[test]
    fn workspace_keeps_internal_symlinks_and_root_aliases_canonical() {
        let project = TempProject::new("workspace-internal-symlink").unwrap();
        project
            .write(
                "root/package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
            )
            .unwrap();
        let member = project
            .write(
                "root/real/member/package.json",
                br#"{"name":"member","version":"1.0.0"}"#,
            )
            .unwrap();
        symlink("real", project.path().join("root/packages")).unwrap();
        symlink("root", project.path().join("alias")).unwrap();
        let workspace = Workspace::discover(&project.path().join("alias")).unwrap();
        assert_eq!(
            workspace.select_path(Some("member")).unwrap(),
            fs::canonicalize(member).unwrap()
        );
        assert_eq!(
            workspace.root_path(),
            fs::canonicalize(project.path().join("root/package.json")).unwrap()
        );
    }
    #[test]
    fn wildcard_members_follow_internal_symlinks_and_reject_external_symlinks() {
        let project = TempProject::new("wildcard-member-symlink").unwrap();
        let external = TempProject::new("wildcard-member-external").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
            )
            .unwrap();
        let member = project
            .write(
                "real/member/package.json",
                br#"{"name":"member","version":"1.0.0"}"#,
            )
            .unwrap();
        fs::create_dir(project.path().join("packages")).unwrap();
        let link = project.path().join("packages/member");
        symlink("../real/member", &link).unwrap();
        let workspace = Workspace::discover(project.path()).unwrap();
        assert_eq!(
            workspace.select_path(Some("member")).unwrap(),
            fs::canonicalize(member).unwrap()
        );
        fs::remove_file(&link).unwrap();
        external
            .write("package.json", b"external content must not be parsed")
            .unwrap();
        symlink(external.path(), &link).unwrap();
        let error = Workspace::discover(project.path()).unwrap_err();
        assert!(error.contains("outside project"), "{error}");
    }
}

#[cfg(test)]
mod read_limit_tests {
    use super::*;

    #[test]
    fn discovery_rejects_oversized_member_before_parsing() {
        let project = tapid_test_support::TempProject::new("workspace-member-limit").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
            )
            .unwrap();
        let path = project
            .write("packages/member/package.json", b"{}")
            .unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(32 * 1024 * 1024 + 1)
            .unwrap();
        let error = Workspace::discover(project.path()).unwrap_err();
        assert!(error.contains("32 MiB"), "{error}");
    }
}
