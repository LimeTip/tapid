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
        let root_path = project_dir.join("package.json");
        let root_metadata = fs::symlink_metadata(&root_path)
            .map_err(|error| format!("cannot inspect workspace root manifest: {error}"))?;
        if !root_metadata.file_type().is_file() {
            return Err("workspace root package.json must be a regular file".to_owned());
        }
        let root_text = read_file(&root_path)?;
        let root = PackageManifest::parse(&root_text).map_err(|error| error.to_string())?;
        let document: Value = serde_json::from_str(&root_text)
            .map_err(|error| format!("invalid workspace package.json: {error}"))?;
        let patterns = workspace_patterns(&document)?;
        let mut paths = Vec::new();
        for pattern in patterns {
            paths.extend(expand_pattern(&project_dir, &pattern)?);
        }
        paths.sort();
        paths.dedup();
        let mut members = Vec::new();
        for path in paths {
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
    let mut paths = vec![root.to_path_buf()];
    for component in relative.components() {
        let name = component.as_os_str();
        if name == "*" {
            let mut next = Vec::new();
            for path in paths {
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
                    if file_type.is_symlink() {
                        return Err(format!(
                            "workspace glob encountered a symlink, which is unsupported: {}",
                            entry.path().display()
                        ));
                    }
                    if file_type.is_dir() {
                        next.push(entry.path());
                    }
                }
            }
            paths = next;
        } else {
            for path in &mut paths {
                path.push(name);
            }
        }
    }
    let mut manifests = paths
        .into_iter()
        .map(|path| path.join("package.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    manifests.sort();
    Ok(manifests)
}

#[cfg(test)]
mod tests {
    use super::expand_pattern;
    use proptest::prelude::*;
    use std::path::Path;

    proptest! {
        #[test]
        fn workspace_patterns_with_parent_components_are_always_rejected(components in prop::collection::vec("[a-z]{1,8}", 0..4)) {
            let pattern = if components.is_empty() { "..".to_owned() } else { format!("{}/..", components.join("/")) };
            prop_assert!(expand_pattern(Path::new("/tmp/tapid-workspace"), &pattern).is_err());
        }
    }
}
