//! Private evaluation planner. No public command or input protocol is registered.
use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Edit<'a> {
    Add {
        section: DependencyKind,
        spec: &'a str,
    },
    Remove {
        section: DependencyKind,
        name: &'a str,
    },
    Move {
        name: &'a str,
        from: DependencyKind,
        to: DependencyKind,
    },
}

pub(crate) fn run(
    project: &Path,
    workspace: Option<&str>,
    store: &Path,
    fixture: &Path,
    edits: &[Edit<'_>],
) -> Result<super::super::install::InstallReport, OperationFailure> {
    super::super::dependency_edit_evaluation::observe(|counts| counts.batch_entries += 1);
    use super::super::install::{
        self, InstallMode, InstallRequest, ManifestEdit, ManifestMutation,
    };
    let selection = resolve_workspace(project, workspace)
        .map_err(|error| OperationFailure::unchanged(project, error))?;
    let updated = plan(&selection, edits)
        .map_err(|error| OperationFailure::unchanged(&selection.root_dir, error))?;
    install::run(
        InstallRequest {
            project_dir: &selection.root_dir,
            target_manifest_path: &selection.manifest_path,
            mutation: ManifestMutation::ReplaceManifest(ManifestEdit {
                expected: &selection.manifest,
                updated: &updated,
            }),
            store_root: Some(store),
            mode: InstallMode::Online,
            registry_fixture: Some(fixture),
            allow_unverified_registry_artifacts: false,
        },
        |_| {},
    )
}

pub(crate) fn plan(
    selection: &WorkspaceSelection,
    edits: &[Edit<'_>],
) -> Result<PackageManifest, OperationalError> {
    use std::collections::BTreeMap;
    let invalid = |message| OperationalError::new(ErrorKind::InvalidRequest, message);
    if edits.is_empty() || edits.len() > 256 {
        return Err(invalid("batch requires between 1 and 256 edits".to_owned()));
    }
    let original = &selection.manifest;
    let mut targets = BTreeMap::new();
    let mut actions = BTreeMap::<_, Vec<(usize, bool)>>::new();
    for (index, edit) in edits.iter().enumerate() {
        let (name, sections, adding) = match *edit {
            Edit::Add { section, spec } => {
                (crate::package_spec::parse(spec).0, vec![section], true)
            }
            Edit::Remove { section, name } => (name, vec![section], false),
            Edit::Move { name, from, to } => {
                if from == to {
                    return Err(invalid(format!("edit {index}: move sections must differ")));
                }
                (name, vec![from, to], false)
            }
        };
        let identity = crate::online::dep_parts(name)
            .map_err(|error| invalid(format!("edit {index}: {error}")))?;
        let identity = (identity.0.to_string(), identity.1.to_string());
        for section in sections {
            if let Some(previous) = targets.insert((section.field_name(), identity.clone()), index)
            {
                return Err(invalid(format!(
                    "edit {index} conflicts with edit {previous}: repeated declaration target"
                )));
            }
        }
        let prior = actions.entry(identity).or_default();
        if !matches!(edit, Edit::Move { .. }) && prior.iter().any(|(_, was_add)| adding != *was_add)
        {
            return Err(invalid(format!(
                "edit {index} conflicts with edit {}: use one explicit move for relocation",
                prior[0].0
            )));
        }
        if !matches!(edit, Edit::Move { .. }) {
            prior.push((index, adding));
        }
    }
    let declared = |kind, name: &str| {
        original
            .dependency_sections()
            .find(|(section, _)| *section == kind)
            .and_then(|(_, values)| values.get(name))
    };
    let mut candidate = original.clone();
    for (index, edit) in edits.iter().enumerate() {
        let result = match *edit {
            Edit::Add { section, spec } => {
                let (name, requirement) = crate::package_spec::parse(spec);
                candidate.insert_dependency_kind(section, name, requirement)
            }
            Edit::Remove { section, name } => {
                if declared(section, name).is_none() {
                    return Err(invalid(format!(
                        "edit {index}: source declaration is absent"
                    )));
                }
                candidate.remove_dependency_kind(section, name)
            }
            Edit::Move { name, from, to } => {
                let requirement = declared(from, name)
                    .ok_or_else(|| invalid(format!("edit {index}: move source is absent")))?;
                if declared(to, name).is_some() {
                    return Err(invalid(format!(
                        "edit {index}: move destination is occupied"
                    )));
                }
                candidate
                    .remove_dependency_kind(from, name)
                    .and_then(|next| next.insert_dependency_kind(to, name, requirement))
            }
        };
        candidate = result.map_err(|error| invalid(format!("edit {index}: {error}")))?;
    }
    // Check every final declaration, including peer-only requirements, before
    // any installer acquisition can recover a pending journal or write state.
    for (kind, values) in candidate.dependency_sections() {
        for (name, requirement) in values {
            crate::online::dep_parts(name).map_err(invalid)?;
            if requirement.starts_with("workspace:") {
                return Err(invalid(format!(
                    "unsupported workspace dependency reference: {name}"
                )));
            }
            if crate::online::copied_declaration(requirement)
                .map_err(invalid)?
                .is_some()
            {
                if kind == DependencyKind::PeerDependencies {
                    return Err(invalid(format!(
                        "copied peer dependency '{name}' is unsupported"
                    )));
                }
                if selection.manifest_path != selection.root_dir.join("package.json") {
                    return Err(invalid(format!(
                        "copied workspace member dependency '{name}' is unsupported"
                    )));
                }
            } else {
                requirement
                    .parse::<Requirement>()
                    .map_err(|error| invalid(format!("invalid dependency '{name}': {error}")))?;
            }
        }
    }
    crate::online::validate_manifest_roots(&selection.root_dir, &candidate)?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_test_support::TempProject;

    #[test]
    fn batch_plans_mixed_sections_and_exact_alias_move_without_writes() {
        let project = TempProject::new("batch-plan-mixed").unwrap();
        let bytes = br#"{"name":"app","version":"1.0.0","dependencies":{"old":"^1","local":"npm:actual@~2.3.0"},"peerDependencies":{"peer":"^1"}}"#;
        project.write("package.json", bytes).unwrap();
        let selection = resolve_workspace(project.path(), None).unwrap();
        let candidate = plan(
            &selection,
            &[
                Edit::Add {
                    section: DependencyKind::Dependencies,
                    spec: "runtime@^1",
                },
                Edit::Add {
                    section: DependencyKind::DevDependencies,
                    spec: "dev@^2",
                },
                Edit::Remove {
                    section: DependencyKind::Dependencies,
                    name: "old",
                },
                Edit::Move {
                    name: "local",
                    from: DependencyKind::Dependencies,
                    to: DependencyKind::DevDependencies,
                },
            ],
        )
        .unwrap();
        assert_eq!(candidate.dependencies()["runtime"], "^1");
        assert_eq!(candidate.dev_dependencies()["dev"], "^2");
        assert_eq!(candidate.dev_dependencies()["local"], "npm:actual@~2.3.0");
        assert_eq!(candidate.peer_dependencies()["peer"], "^1");
        assert!(!candidate.dependencies().contains_key("old"));
        assert!(!candidate.dependencies().contains_key("local"));
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            bytes
        );
        assert!(!project.path().join("tapid.lock").exists());
    }

    #[test]
    fn batch_move_preserves_requirement_bytes_and_independent_removals_are_allowed() {
        let project = TempProject::new("batch-move-bytes").unwrap();
        project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"local":" npm:actual@~2.3.0 ","old":"^1"},"devDependencies":{"old":"^2"}}"#).unwrap();
        let selection = resolve_workspace(project.path(), None).unwrap();
        let candidate = plan(
            &selection,
            &[
                Edit::Move {
                    name: "local",
                    from: DependencyKind::Dependencies,
                    to: DependencyKind::DevDependencies,
                },
                Edit::Remove {
                    section: DependencyKind::Dependencies,
                    name: "old",
                },
                Edit::Remove {
                    section: DependencyKind::DevDependencies,
                    name: "old",
                },
            ],
        )
        .unwrap();
        assert_eq!(candidate.dev_dependencies()["local"], " npm:actual@~2.3.0 ");
        assert!(!candidate.dependencies().contains_key("old"));
        assert!(!candidate.dev_dependencies().contains_key("old"));
    }

    #[test]
    fn batch_rejects_conflicts_in_either_order_and_invalid_last_edit_before_recovery() {
        let project = TempProject::new("batch-invalid-pending").unwrap();
        let bytes = br#"{"name":"app","version":"1.0.0","dependencies":{"old":"^1"},"devDependencies":{"occupied":"^1","old":"^2"}}"#;
        project.write("package.json", bytes).unwrap();
        let journal = b"pending journal must not be read or recovered by planning";
        project
            .write(".tapid-lifecycle-journal.json", journal)
            .unwrap();
        let selection = resolve_workspace(project.path(), None).unwrap();
        let add = Edit::Add {
            section: DependencyKind::Dependencies,
            spec: "new@^1",
        };
        let bad = [
            vec![add, add],
            vec![
                add,
                Edit::Remove {
                    section: DependencyKind::DevDependencies,
                    name: "new",
                },
            ],
            vec![
                add,
                Edit::Add {
                    section: DependencyKind::Dependencies,
                    spec: "npm:new@^2",
                },
            ],
            vec![
                add,
                Edit::Add {
                    section: DependencyKind::DevDependencies,
                    spec: "bad@npm:",
                },
            ],
            vec![
                add,
                Edit::Add {
                    section: DependencyKind::PeerDependencies,
                    spec: "peer@not a range!",
                },
            ],
            vec![
                add,
                Edit::Add {
                    section: DependencyKind::Dependencies,
                    spec: "../escape@1",
                },
            ],
            vec![
                add,
                Edit::Add {
                    section: DependencyKind::Dependencies,
                    spec: "local@workspace:*",
                },
            ],
            vec![
                add,
                Edit::Add {
                    section: DependencyKind::Dependencies,
                    spec: "local@file:../escape.tgz",
                },
            ],
            vec![
                add,
                Edit::Remove {
                    section: DependencyKind::DevDependencies,
                    name: "absent",
                },
            ],
            vec![
                add,
                Edit::Move {
                    name: "old",
                    from: DependencyKind::Dependencies,
                    to: DependencyKind::DevDependencies,
                },
            ],
            vec![
                add,
                Edit::Move {
                    name: "old",
                    from: DependencyKind::Dependencies,
                    to: DependencyKind::Dependencies,
                },
            ],
            vec![
                Edit::Move {
                    name: "old",
                    from: DependencyKind::Dependencies,
                    to: DependencyKind::PeerDependencies,
                },
                Edit::Remove {
                    section: DependencyKind::PeerDependencies,
                    name: "old",
                },
            ],
        ];
        for mut edits in bad {
            for _ in 0..2 {
                let (_, counts) = crate::application::dependency_edit_evaluation::measure(|| {
                    let failure = run(
                        project.path(),
                        None,
                        &project.path().join("store"),
                        &project.path().join("absent-fixture.json"),
                        &edits,
                    )
                    .unwrap_err();
                    assert_eq!(failure.error.kind, ErrorKind::InvalidRequest, "{edits:?}");
                    assert_eq!(
                        failure.outcome.state,
                        super::super::super::outcome::ChangeState::Unchanged
                    );
                });
                assert_eq!(counts.batch_entries, 1);
                assert_eq!(counts.graph_operations, 0);
                assert_eq!(counts.durable_decisions, 0);
                assert_eq!(counts.materializations, 0);
                assert_eq!(counts.activations, 0);
                assert_eq!(counts.artifact_requests, 0);
                assert_eq!(
                    fs::read(project.path().join("package.json")).unwrap(),
                    bytes
                );
                assert_eq!(
                    fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
                    journal
                );
                assert!(!project.path().join("tapid.lock").exists());
                assert!(!project.path().join("node_modules").exists());
                edits.reverse();
            }
        }
        assert!(plan(&selection, &[]).is_err());
        assert!(plan(&selection, &vec![add; 257]).is_err());
    }

    #[test]
    fn batch_supports_independent_overlaps_and_preserves_optional_peer_declarations() {
        let project = TempProject::new("batch-section-overlap").unwrap();
        project
            .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
            .unwrap();
        let selection = resolve_workspace(project.path(), None).unwrap();
        let candidate = plan(
            &selection,
            &[
                Edit::Add {
                    section: DependencyKind::Dependencies,
                    spec: "local@npm:actual@1",
                },
                Edit::Add {
                    section: DependencyKind::DevDependencies,
                    spec: "local@npm:actual@2",
                },
                Edit::Add {
                    section: DependencyKind::OptionalDependencies,
                    spec: "optional@^3",
                },
                Edit::Add {
                    section: DependencyKind::PeerDependencies,
                    spec: "peer@^4",
                },
            ],
        )
        .unwrap();
        assert_eq!(candidate.dependencies()["local"], "npm:actual@1");
        assert_eq!(candidate.dev_dependencies()["local"], "npm:actual@2");
        assert_eq!(candidate.optional_dependencies()["optional"], "^3");
        assert_eq!(candidate.peer_dependencies()["peer"], "^4");
    }

    #[test]
    fn batch_rejects_copied_peer_requirements_before_recovery() {
        use base64::Engine as _;
        let project = TempProject::new("batch-copied-peer").unwrap();
        project
            .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
            .unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/npm-aliases.json")).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(
                fixture["packages"][0]["artifact"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("base64:")
                    .unwrap(),
            )
            .unwrap();
        project.write("vendor/pkg.tgz", &bytes).unwrap();
        project
            .write(".tapid-lifecycle-journal.json", b"must remain untouched")
            .unwrap();
        let failure = run(
            project.path(),
            None,
            &project.path().join("store"),
            &project.path().join("registry.json"),
            &[Edit::Add {
                section: DependencyKind::PeerDependencies,
                spec: "peer@file:vendor/pkg.tgz",
            }],
        )
        .unwrap_err();
        assert_eq!(failure.error.kind, ErrorKind::InvalidRequest);
        assert_eq!(
            fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
            b"must remain untouched"
        );
    }

    #[test]
    fn batch_validates_member_selection_and_rejects_copied_member_sources_without_writes() {
        let project = TempProject::new("batch-member-validation").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"]}"#,
            )
            .unwrap();
        project
            .write(
                "packages/member/package.json",
                br#"{"name":"member","version":"1.0.0"}"#,
            )
            .unwrap();
        let edits = [Edit::Add {
            section: DependencyKind::Dependencies,
            spec: "local@file:vendor/pkg.tgz",
        }];
        for selector in ["missing", "workspace:member", "../member", "member"] {
            let failure = run(
                project.path(),
                Some(selector),
                &project.path().join("store"),
                &project.path().join("registry.json"),
                &edits,
            )
            .unwrap_err();
            assert_eq!(failure.error.kind, ErrorKind::InvalidRequest);
            assert!(!project.path().join("tapid.lock").exists());
            assert!(!project.path().join("store").exists());
        }
    }
}
