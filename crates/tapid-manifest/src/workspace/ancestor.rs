use super::*;

impl Workspace {
    /// Find the nearest enclosing workspace, or the nearest standalone package.
    /// An explicit exact-name selector overrides inference, never owner discovery.
    /// Invalid encountered manifests fail without searching farther ancestors.
    /// Returns the validated owner and the selected member name (None for root).
    pub fn discover_ancestor(
        directory: &Path,
        selector: Option<&str>,
    ) -> Result<(Self, Option<String>), crate::WorkspaceError> {
        let directory = fs::canonicalize(directory).map_err(|error| {
            format!(
                "cannot resolve invocation directory {}: {error}",
                directory.display()
            )
        })?;
        if !directory.is_dir() {
            return Err(format!(
                "invocation path is not a directory: {}",
                directory.display()
            )
            .into());
        }
        let mut packages: Vec<PathBuf> = Vec::new();
        for ancestor in directory.ancestors() {
            let path = ancestor.join("package.json");
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(crate::WorkspaceError::ManifestRead {
                        path: path.clone(),
                        message: format!(
                            "cannot inspect ancestor manifest {}: {error}",
                            path.display()
                        ),
                    });
                }
            };
            if !metadata.file_type().is_file() {
                return Err(crate::WorkspaceError::ManifestRead {
                    path: path.clone(),
                    message: format!(
                        "ancestor manifest must be a regular file, not a symlink: {}",
                        path.display()
                    ),
                });
            }
            let text = read_file(&path)?;
            let manifest =
                PackageManifest::parse(&text).map_err(|error| crate::WorkspaceError::Manifest {
                    member: false,
                    path: path.clone(),
                    diagnostic: Box::new(error.diagnostic(&text)),
                    error: Box::new(error),
                })?;
            if manifest.extra.contains_key("workspaces") {
                let workspace = Self::discover_typed(ancestor).map_err(|error| match error {
                    crate::WorkspaceError::Discovery(message) => crate::WorkspaceError::Discovery(
                        format!("invalid workspace at {}: {message}", ancestor.display()),
                    ),
                    other => other,
                })?;
                let selected = if let Some(name) = selector {
                    workspace
                        .select(Some(name))
                        .map_err(crate::WorkspaceError::Selection)?;
                    Some(name.to_owned())
                } else {
                    let containing = workspace
                        .members
                        .iter()
                        .filter(|member| {
                            directory
                                .starts_with(member.path.parent().expect("member manifest parent"))
                        })
                        .collect::<Vec<_>>();
                    if containing.len() > 1 {
                        return Err(format!(
                            "ambiguous workspace membership for {}; use --workspace NAME",
                            directory.display()
                        )
                        .into());
                    }
                    // Every intervening package must itself be a declared member.
                    // Otherwise location-based selection could mutate the wrong package.
                    if let Some(undeclared) = packages
                        .iter()
                        .find(|path| !workspace.members.iter().any(|member| &member.path == *path))
                    {
                        return Err(format!("undeclared package {}; declare it as a workspace member or use --project-dir PATH or --workspace NAME", undeclared.display()).into());
                    }
                    containing.first().map(|member| member.name.clone())
                };
                return Ok((workspace, selected));
            }
            packages.push(path);
        }
        let nearest = packages.first().ok_or_else(|| {
            format!(
                "no enclosing package.json for {}; use --project-dir PATH",
                directory.display()
            )
        })?;
        let workspace = Self::discover_typed(nearest.parent().expect("ancestor manifest parent"))?;
        workspace
            .select(selector)
            .map_err(crate::WorkspaceError::Selection)?;
        Ok((workspace, selector.map(str::to_owned)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_test_support::TempProject;

    fn workspace() -> TempProject {
        let project = TempProject::new("ancestor-boundaries").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["apps/news","packages/ui"]}"#,
            )
            .unwrap();
        project
            .write(
                "apps/news/package.json",
                br#"{"name":"news","version":"1.0.0"}"#,
            )
            .unwrap();
        project
            .write(
                "packages/ui/package.json",
                br#"{"name":"@example/ui","version":"1.0.0"}"#,
            )
            .unwrap();
        project
    }

    #[test]
    fn explicit_selector_overrides_location_but_never_searches_another_owner() {
        let project = workspace();
        let (owner, selected) =
            Workspace::discover_ancestor(&project.path().join("apps/news"), Some("@example/ui"))
                .unwrap();
        assert_eq!(selected.as_deref(), Some("@example/ui"));
        assert_eq!(
            owner.select(selected.as_deref()).unwrap().name().as_str(),
            "@example/ui"
        );
        assert!(
            Workspace::discover_ancestor(project.path(), Some("missing"))
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
    }

    #[test]
    fn ordinary_directories_select_root_but_undeclared_packages_fail() {
        let project = workspace();
        fs::create_dir_all(project.path().join("apps/newsletter/src")).unwrap();
        assert_eq!(
            Workspace::discover_ancestor(&project.path().join("apps/newsletter/src"), None)
                .unwrap()
                .1,
            None
        );
        project
            .write(
                "apps/newsletter/package.json",
                br#"{"name":"undeclared","version":"1.0.0"}"#,
            )
            .unwrap();
        let error = Workspace::discover_ancestor(&project.path().join("apps/newsletter/src"), None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("undeclared package") && error.contains("--project-dir"),
            "{error}"
        );
        assert_eq!(
            Workspace::discover_ancestor(&project.path().join("apps/newsletter/src"), Some("news"))
                .unwrap()
                .1
                .as_deref(),
            Some("news")
        );
    }

    #[test]
    fn standalone_descendants_and_nearest_nested_workspace_are_independent() {
        let project = workspace();
        project
            .write(
                "apps/news/nested/package.json",
                br#"{"name":"inner","version":"1.0.0","workspaces":["member"]}"#,
            )
            .unwrap();
        project
            .write(
                "apps/news/nested/member/package.json",
                br#"{"name":"inner-member","version":"1.0.0"}"#,
            )
            .unwrap();
        let (owner, selected) =
            Workspace::discover_ancestor(&project.path().join("apps/news/nested/member"), None)
                .unwrap();
        assert_eq!(owner.root().name().as_str(), "inner");
        assert_eq!(selected.as_deref(), Some("inner-member"));
        assert!(
            Workspace::discover_ancestor(&project.path().join("apps/news/nested"), Some("news"))
                .is_err()
        );
        let standalone = TempProject::new("standalone-ancestor").unwrap();
        standalone
            .write("package.json", br#"{"name":"alone","version":"1.0.0"}"#)
            .unwrap();
        fs::create_dir_all(standalone.path().join("src/deep")).unwrap();
        let (owner, selected) =
            Workspace::discover_ancestor(&standalone.path().join("src/deep"), None).unwrap();
        assert_eq!(owner.root().name().as_str(), "alone");
        assert_eq!(selected, None);
    }

    #[test]
    fn invalid_nearer_manifests_never_fall_back() {
        for bytes in [
            b"invalid".as_slice(),
            br#"{"name":"inner","version":"1.0.0","workspaces":false}"#,
        ] {
            let project = workspace();
            project
                .write("apps/news/inner/package.json", bytes)
                .unwrap();
            assert!(
                Workspace::discover_ancestor(&project.path().join("apps/news/inner"), Some("news"))
                    .is_err()
            );
        }
        let project = workspace();
        fs::create_dir_all(project.path().join("apps/news/inner/package.json")).unwrap();
        let error = Workspace::discover_ancestor(&project.path().join("apps/news/inner"), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("regular file"), "{error}");
    }

    #[test]
    fn overlapping_members_require_an_explicit_selector() {
        let project = workspace();
        project.write("package.json", br#"{"name":"root","version":"1.0.0","workspaces":["apps/news","apps/news/inner"]}"#).unwrap();
        project
            .write(
                "apps/news/inner/package.json",
                br#"{"name":"inner","version":"1.0.0"}"#,
            )
            .unwrap();
        assert!(
            Workspace::discover_ancestor(&project.path().join("apps/news/inner"), None)
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert_eq!(
            Workspace::discover_ancestor(&project.path().join("apps/news/inner"), Some("inner"))
                .unwrap()
                .1
                .as_deref(),
            Some("inner")
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_aliases_and_manifest_symlinks_preserve_safety() {
        use std::os::unix::fs::symlink;
        let project = workspace();
        symlink("apps/news", project.path().join("alias")).unwrap();
        assert_eq!(
            Workspace::discover_ancestor(&project.path().join("alias"), None)
                .unwrap()
                .1
                .as_deref(),
            Some("news")
        );
        fs::rename(
            project.path().join("apps/news/package.json"),
            project.path().join("apps/news/real.json"),
        )
        .unwrap();
        symlink("real.json", project.path().join("apps/news/package.json")).unwrap();
        assert!(
            Workspace::discover_ancestor(&project.path().join("apps/news"), Some("@example/ui"))
                .unwrap_err()
                .to_string()
                .contains("regular file")
        );
    }
    #[test]
    fn absent_project_and_oversized_nearer_manifest_have_actionable_errors() {
        let empty = TempProject::new("no-enclosing-package").unwrap();
        let error = Workspace::discover_ancestor(empty.path(), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--project-dir"), "{error}");
        let project = workspace();
        let path = project.write("apps/news/inner/package.json", b"").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_MANIFEST_BYTES as u64 + 1)
            .unwrap();
        let error = Workspace::discover_ancestor(path.parent().unwrap(), Some("news"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("32 MiB") && error.contains("inner/package.json"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_alias_cannot_inherit_lexical_membership_and_external_members_fail() {
        use std::os::unix::fs::symlink;
        let project = workspace();
        let external = TempProject::new("ancestor-external-alias").unwrap();
        external
            .write("package.json", br#"{"name":"external","version":"1.0.0"}"#)
            .unwrap();
        symlink(external.path(), project.path().join("external-alias")).unwrap();
        let (owner, selected) =
            Workspace::discover_ancestor(&project.path().join("external-alias"), None).unwrap();
        assert_eq!(owner.root().name().as_str(), "external");
        assert_eq!(selected, None);
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["external-alias"]}"#,
            )
            .unwrap();
        assert!(
            Workspace::discover_ancestor(project.path(), None)
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
    }
}
