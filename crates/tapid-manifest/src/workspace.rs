use crate::PackageManifest;
use serde_json::Value;
use std::{
    fs,
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
            ));
        }
        let root_manifest_path = project_dir.join("package.json");
        let root_metadata = fs::symlink_metadata(&root_manifest_path)
            .map_err(|error| format!("cannot inspect workspace root manifest: {error}"))?;
        if !root_metadata.file_type().is_file() {
            return Err("workspace root package.json must be a regular file".to_owned());
        }
        let root_path = contained_path(&project_dir, &root_manifest_path)?;
        let root_text = read_file(&root_path)?;
        let root = PackageManifest::parse(&root_text).map_err(|error| error.to_string())?;
        let document: Value = serde_json::from_str(&root_text)
            .map_err(|error| format!("invalid workspace package.json: {error}"))?;
        let patterns = workspace_patterns(&document)?;
        let mut paths = Vec::new();
        for pattern in patterns {
            paths.extend(expand_pattern(&project_dir, &pattern)?);
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
                ));
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
                ));
            }
            let text = read_file(&canonical)?;
            let manifest = PackageManifest::parse(&text).map_err(|error| {
                format!("invalid workspace member {}: {error}", canonical.display())
            })?;
            if members
                .iter()
                .any(|member: &WorkspaceMember| member.name == manifest.name().to_string())
                || manifest.name() == root.name()
            {
                return Err(format!(
                    "duplicate workspace package name '{}'",
                    manifest.name()
                ));
            }
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
    fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn workspace_patterns(document: &Value) -> Result<Vec<String>, String> {
    let value = match document.get("workspaces") {
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

fn expand_pattern(root: &Path, pattern: &str) -> Result<Vec<PathBuf>, String> {
    let relative = Path::new(pattern);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("workspace pattern is outside project: {pattern}"));
    }
    if !relative
        .components()
        .any(|component| matches!(component, std::path::Component::Normal(_)))
        || pattern.contains(['?', '[', ']', '{', '}', '!', '(', ')', '\\', ':'])
        || relative.components().any(|component| {
            let name = component.as_os_str().to_string_lossy();
            name.contains('*') && name != "*"
        })
    {
        return Err(format!(
            "unsupported workspace pattern '{pattern}'; use a relative directory path or a whole path component '*' for one directory level"
        ));
    }
    let mut paths = vec![root.to_path_buf()];
    for component in relative.components() {
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
                            ));
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
                path.push(name);
            }
        }
    }
    let mut manifests = Vec::new();
    for path in paths {
        let manifest = path.join("package.json");
        match fs::symlink_metadata(&manifest) {
            // Keep every existing entry for the regular-file validation in discovery.
            // Following the final component here would hide dangling symlinks.
            Ok(_) => manifests.push(manifest),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot inspect workspace manifest {}: {error}",
                    manifest.display()
                ));
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
