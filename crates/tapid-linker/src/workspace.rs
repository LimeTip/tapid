//! Pure planning for linking local workspace packages into a project.
//!
//! This models package-manager materialization only. It does not provide
//! runtime workspace resolution, mutate the filesystem, or validate symlinks.
use std::path::{Component, Path, PathBuf};

use tapid_core::{PackageName, PackageVersion};

use crate::PlanError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspacePackage {
    /// Package root relative to the project root, using normal path components.
    pub root: PathBuf,
    pub name: PackageName,
    pub version: PackageVersion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceLink {
    pub name: PackageName,
    pub version: PackageVersion,
    /// Absolute source root below the project root.
    pub source: PathBuf,
    /// Project-local package-manager link destination.
    pub target: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct WorkspaceLinkPlan {
    pub links: Vec<WorkspaceLink>,
}

/// Plans deterministic package-manager links for local workspace packages.
/// This is not runtime support and performs no filesystem mutation.
pub fn plan_workspace_links(
    project_root: impl Into<PathBuf>,
    packages: impl IntoIterator<Item = WorkspacePackage>,
) -> Result<WorkspaceLinkPlan, PlanError> {
    let project_root = project_root.into();
    if !project_root.is_absolute()
        || project_root
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(PlanError::InvalidManagedRoot(project_root));
    }
    let mut packages: Vec<_> = packages.into_iter().collect();
    packages.sort_by(|a, b| (&a.name, &a.version, &a.root).cmp(&(&b.name, &b.version, &b.root)));
    let mut links = Vec::with_capacity(packages.len());
    let mut targets = std::collections::BTreeSet::new();
    for package in packages {
        if !valid_relative_root(&package.root) {
            return Err(PlanError::PathOutsideManagedRoot(package.root));
        }
        let source = project_root.join(&package.root);
        if !source
            .strip_prefix(&project_root)
            .is_ok_and(|p| !p.as_os_str().is_empty())
        {
            return Err(PlanError::PathOutsideManagedRoot(source));
        }
        let target = project_root
            .join("node_modules")
            .join(package.name.as_str().split('/').collect::<PathBuf>());
        if !target
            .strip_prefix(&project_root)
            .is_ok_and(|p| !p.as_os_str().is_empty())
        {
            return Err(PlanError::PathOutsideManagedRoot(target));
        }
        if !targets.insert(target.clone()) {
            return Err(PlanError::ConflictingTarget(target));
        }
        links.push(WorkspaceLink {
            name: package.name,
            version: package.version,
            source,
            target,
        });
    }
    Ok(WorkspaceLinkPlan { links })
}

fn valid_relative_root(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(root: &str, name: &str, version: &str) -> WorkspacePackage {
        WorkspacePackage {
            root: root.into(),
            name: name.parse().unwrap(),
            version: version.parse().unwrap(),
        }
    }

    #[test]
    fn plans_sorted_scoped_and_unscoped_package_links_with_identity() {
        let root = PathBuf::from("/project");
        let plan = plan_workspace_links(
            &root,
            [
                package("packages/tool", "@scope/tool", "1.2.3"),
                package("apps/web", "web", "2.0.0"),
            ],
        )
        .unwrap();
        assert_eq!(plan.links.len(), 2);
        assert_eq!(plan.links[0].name.as_str(), "@scope/tool");
        assert_eq!(plan.links[0].version.to_string(), "1.2.3");
        assert_eq!(plan.links[0].source, root.join("packages/tool"));
        assert_eq!(plan.links[0].target, root.join("node_modules/@scope/tool"));
        let reversed = plan_workspace_links(
            root,
            [
                package("apps/web", "web", "2.0.0"),
                package("packages/tool", "@scope/tool", "1.2.3"),
            ],
        )
        .unwrap();
        assert_eq!(plan, reversed);
    }

    #[test]
    fn rejects_absolute_and_traversing_package_roots() {
        for path in [
            PathBuf::from("/outside"),
            PathBuf::from("../outside"),
            PathBuf::from("packages/../outside"),
        ] {
            assert!(matches!(
                plan_workspace_links(
                    "/project",
                    [WorkspacePackage {
                        root: path,
                        name: "safe".parse().unwrap(),
                        version: "1.0.0".parse().unwrap()
                    }]
                ),
                Err(PlanError::PathOutsideManagedRoot(_))
            ));
        }
    }

    #[test]
    fn rejects_duplicate_link_targets() {
        assert!(matches!(
            plan_workspace_links(
                "/project",
                [
                    package("packages/a", "same", "1.0.0"),
                    package("packages/b", "same", "2.0.0")
                ]
            ),
            Err(PlanError::ConflictingTarget(_))
        ));
    }
}
