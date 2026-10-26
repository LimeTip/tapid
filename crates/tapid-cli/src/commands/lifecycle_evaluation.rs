use crate::application::dependency_edit_evaluation::measure;
use crate::application::{
    install::{self, InstallMode, InstallRequest, ManifestMutation},
    lifecycle::batch::{self, Edit},
};
use crate::commands::lifecycle::{AddArgs, CommonArgs, RemoveArgs, add, remove};
use std::{fs, path::Path, time::Instant};
use tapid_manifest::{DependencyKind as Kind, PackageManifest};
use tapid_test_support::{TempHome, TempProject};

const FIXTURE: &str = include_str!("../../tests/fixtures/npm-aliases.json");

fn common(project: &TempProject, home: &TempHome, workspace: bool) -> CommonArgs {
    CommonArgs {
        project_dir: project.path().to_owned(),
        workspace: workspace.then(|| "member".to_owned()),
        store_dir: Some(home.path().join("store")),
        registry_fixture: Some(project.path().join("registry.json")),
        allow_unverified_registry_artifacts: false,
    }
}

fn ordinary_add(
    project: &TempProject,
    home: &TempHome,
    workspace: bool,
    dev: bool,
    specs: &[&str],
) {
    assert_eq!(
        add(
            AddArgs {
                dry_run: false,
                packages: specs.iter().map(|s| (*s).to_owned()).collect(),
                dev,
                optional: false,
                peer: false,
                common: common(project, home, workspace),
            },
            false
        ),
        std::process::ExitCode::SUCCESS
    );
}

fn candidate(project: &TempProject, home: &TempHome, workspace: bool, edits: &[Edit<'_>]) {
    batch::run(
        project.path(),
        workspace.then_some("member"),
        &home.path().join("store"),
        &project.path().join("registry.json"),
        edits,
    )
    .unwrap();
}

fn initialize(project: &TempProject, workspace: bool, empty: bool) {
    project.write("registry.json", FIXTURE.as_bytes()).unwrap();
    let dependencies = if empty {
        serde_json::json!({})
    } else {
        serde_json::json!({"old":"npm:h3@1.0.0"})
    };
    let selected = serde_json::json!({"name": if workspace { "member" } else { "app" }, "version":"1.0.0", "private":true, "dependencies": dependencies});
    if workspace {
        project
            .write(
                "package.json",
                br#"{"name":"app","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#,
            )
            .unwrap();
        project
            .write(
                "packages/member/package.json",
                selected.to_string().as_bytes(),
            )
            .unwrap();
        project
            .write(
                "packages/untouched/package.json",
                br#"{"name":"untouched","version":"1.0.0","dependencies":{"old":"npm:h3@1.0.0"}}"#,
            )
            .unwrap();
    } else {
        project
            .write("package.json", selected.to_string().as_bytes())
            .unwrap();
    }
}

fn install_initial(project: &TempProject, home: &TempHome) {
    install::run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&home.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&project.path().join("registry.json")),
            allow_unverified_registry_artifacts: false,
        },
        |_| {},
    )
    .unwrap();
}

fn edits(case: &str) -> Vec<Edit<'static>> {
    match case {
        "single" => vec![
            Edit::Add {
                section: Kind::Dependencies,
                spec: "h3@2.0.0",
            },
            Edit::Add {
                section: Kind::Dependencies,
                spec: "@actual/scoped@1.2.0",
            },
        ],
        "mixed" => vec![
            Edit::Add {
                section: Kind::Dependencies,
                spec: "h3@2.0.0",
            },
            Edit::Add {
                section: Kind::DevDependencies,
                spec: "@actual/scoped@1.2.0",
            },
            Edit::Remove {
                section: Kind::Dependencies,
                name: "old",
            },
        ],
        "workspace" => vec![
            Edit::Move {
                name: "old",
                from: Kind::Dependencies,
                to: Kind::DevDependencies,
            },
            Edit::Add {
                section: Kind::DevDependencies,
                spec: "@actual/scoped@1.2.0",
            },
            Edit::Add {
                section: Kind::Dependencies,
                spec: "h3@2.0.0",
            },
        ],
        _ => unreachable!(),
    }
}

fn control(project: &TempProject, home: &TempHome, case: &str) {
    match case {
        "single" => ordinary_add(
            project,
            home,
            false,
            false,
            &["h3@2.0.0", "@actual/scoped@1.2.0"],
        ),
        "mixed" => {
            ordinary_add(project, home, false, false, &["h3@2.0.0"]);
            ordinary_add(project, home, false, true, &["@actual/scoped@1.2.0"]);
            assert_eq!(
                remove(
                    RemoveArgs {
                        dry_run: false,
                        packages: vec!["old".to_owned()],
                        common: common(project, home, false)
                    },
                    false
                ),
                std::process::ExitCode::SUCCESS
            );
        }
        "workspace" => {
            ordinary_add(
                project,
                home,
                true,
                true,
                &["old@npm:h3@1.0.0", "@actual/scoped@1.2.0"],
            );
            ordinary_add(project, home, true, false, &["h3@2.0.0"]);
        }
        _ => unreachable!(),
    }
}

fn final_state(project: &TempProject, workspace: bool) -> serde_json::Value {
    let selected = if workspace {
        "packages/member/package.json"
    } else {
        "package.json"
    };
    let manifest =
        PackageManifest::parse(&fs::read_to_string(project.path().join(selected)).unwrap())
            .unwrap();
    let lock = tapid_lockfile::Lockfile::from_json(
        &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    // Paths in the managed tree differ between isolated projects. Compare exact
    // lock records/bindings, declaration values, and the bytes visible at roots.
    let modules = project.path().join(if workspace {
        "packages/member/node_modules"
    } else {
        "node_modules"
    });
    let mut visible = std::collections::BTreeMap::new();
    for name in ["h3", "@actual/scoped", "old"] {
        let path = modules.join(name).join("package.json");
        visible.insert(
            name,
            fs::read(path)
                .or_else(|_| {
                    fs::read(
                        project
                            .path()
                            .join("node_modules")
                            .join(name)
                            .join("package.json"),
                    )
                })
                .ok(),
        );
    }
    assert!(visible["h3"].is_some());
    assert!(visible["@actual/scoped"].is_some());
    assert_eq!(visible["old"].is_some(), workspace);
    if workspace {
        assert!(
            project
                .path()
                .join("node_modules/old/package.json")
                .is_file()
                || project
                    .path()
                    .join("packages/untouched/node_modules/old/package.json")
                    .is_file()
        );
    }
    let untouched = workspace
        .then(|| fs::read(project.path().join("packages/untouched/package.json")).unwrap());
    serde_json::json!({"manifest": serde_json::from_str::<serde_json::Value>(&manifest.to_json()).unwrap(), "packages": lock.packages(), "bindings": lock.root_bindings(), "workspace": lock.workspace_packages(), "visible": visible, "untouched": untouched})
}

#[test]
fn dependency_edit_measurement_matrix() {
    for case in ["single", "mixed", "workspace"] {
        for warm in [false, true] {
            for sample in 0..5 {
                let workspace = case == "workspace";
                let mut states = Vec::new();
                for proposed in [false, true] {
                    let project = TempProject::new("batch-measurement").unwrap();
                    let home = TempHome::new("batch-measurement").unwrap();
                    initialize(&project, workspace, case == "single");
                    install_initial(&project, &home);
                    if warm {
                        let seed = TempProject::new("batch-warm-seed").unwrap();
                        initialize(&seed, workspace, case == "single");
                        candidate(&seed, &home, workspace, &edits(case));
                    }
                    let start = Instant::now();
                    let (_, counts) = measure(|| {
                        if proposed {
                            candidate(&project, &home, workspace, &edits(case));
                        } else {
                            control(&project, &home, case);
                        }
                    });
                    let elapsed = start.elapsed().as_micros();
                    assert_eq!(
                        counts.graph_operations,
                        if proposed || case == "single" {
                            1
                        } else if workspace {
                            2
                        } else {
                            3
                        }
                    );
                    assert_eq!(counts.durable_decisions, counts.graph_operations);
                    assert_eq!(
                        counts.command_entries,
                        if proposed { 0 } else { counts.graph_operations }
                    );
                    assert_eq!(counts.batch_entries, usize::from(proposed));
                    println!(
                        "BATCH_MEASUREMENT {}",
                        serde_json::json!({"case":case, "warm":warm, "sample":sample, "candidate":proposed, "elapsed_us":elapsed, "counts":counts})
                    );
                    states.push(final_state(&project, workspace));
                }
                assert_eq!(
                    states[0], states[1],
                    "final-state mismatch: {case}, warm={warm}, sample={sample}"
                );
            }
        }
    }
}
