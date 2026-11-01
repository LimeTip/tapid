use super::*;
use tapid_core::{PackageInstanceId, PeerContext, PlatformContext};
use tapid_linker::{InstanceKey, LayoutInput, MaterializationPlan, VerifiedTreeReference};
use tapid_test_support::{TempHome, TempProject};

struct Fixture {
    project: TempProject,
    _home: TempHome,
    snapshots: Vec<tapid_store::VerifiedTreeSnapshot>,
    input: LayoutInput,
    trees: BTreeMap<String, PathBuf>,
    plan: MaterializationPlan,
}

impl Fixture {
    fn new(placements: usize, sources: usize, snapshot: bool) -> Self {
        let project = TempProject::new("materialization-sources").unwrap();
        let home = TempHome::new("materialization-sources").unwrap();
        let store = tapid_store::Store::new(home.path().join("store"));
        let mut roots = Vec::new();
        let mut snapshots = Vec::new();
        for index in 0..sources {
            let root = project.path().join(format!("source-{index}"));
            fs::create_dir_all(&root).unwrap();
            // Equal bytes at different paths must still be verified separately.
            fs::write(
                root.join("package.json"),
                br#"{"name":"shared","version":"1.0.0"}"#,
            )
            .unwrap();
            fs::write(root.join("index.js"), b"original").unwrap();
            let digest = tapid_archive::canonical_tree_digest(&root).unwrap();
            let root = if snapshot {
                let digest = digest.parse().unwrap();
                store.activate_verified_tree(&digest, &root).unwrap();
                let owner = store.verified_tree_snapshot(&digest).unwrap();
                let path = owner.path().to_owned();
                snapshots.push(owner);
                path
            } else {
                root
            };
            roots.push((root, digest));
        }
        let instances = (0..placements)
            .map(|index| {
                let (root, digest) = &roots[index % sources];
                PackageInstance {
                    id: PackageInstanceId::new(
                        "https://registry.example".parse().unwrap(),
                        format!("package-{index}").parse().unwrap(),
                        "1.0.0".parse().unwrap(),
                    ),
                    peer_context: PeerContext::default(),
                    platform_context: PlatformContext::new(None, None, None).unwrap(),
                    tree: VerifiedTreeReference::new(digest, root).unwrap(),
                }
            })
            .collect::<Vec<_>>();
        let input = LayoutInput {
            root_dependencies: instances.iter().map(InstanceKey::from).collect(),
            instances,
            dependency_edges: Vec::new(),
        };
        let plan = tapid_linker::plan_layout(
            ManagedRoot::new(project.path()).unwrap(),
            input.clone(),
            tapid_linker::Platform::Unix,
        )
        .unwrap();
        assert_eq!(plan.activation.steps.len(), placements);
        let trees = roots
            .into_iter()
            .enumerate()
            .map(|(index, (root, _))| (index.to_string(), root))
            .collect();
        Self {
            project,
            _home: home,
            snapshots,
            input,
            trees,
            plan,
        }
    }

    fn run(&self, snapshot: bool, progress: impl FnMut(usize, usize)) -> Result<(), String> {
        PRE_COPY_DIGEST_COUNT.set(0);
        FINAL_DIGEST_COUNT.set(0);
        PLACEMENT_COUNT.set(0);
        materialize_stage_with_workspace_links(
            &self.project.path().join("stage"),
            &self.plan,
            &self.input.instances,
            &self.trees,
            snapshot,
            &Default::default(),
            self.project.path(),
            progress,
        )
    }

    fn assert_placements(&self) {
        for step in &self.plan.activation.steps {
            let target = self.project.path().join("stage").join(
                step.target
                    .strip_prefix(&self.plan.managed_root.path)
                    .unwrap(),
            );
            assert_eq!(fs::read(target.join("index.js")).unwrap(), b"original");
        }
    }
}

#[test]
fn materialization_distinct_source_counters() {
    for snapshot in [false, true] {
        for (placements, sources) in [(2, 1), (100, 1), (100, 4)] {
            let mut fixture = Fixture::new(placements, sources, snapshot);
            let mut completed = false;
            fixture
                .run(snapshot, |done, total| {
                    if done == total {
                        assert_eq!(FINAL_DIGEST_COUNT.get(), sources);
                        fixture.assert_placements();
                        completed = true;
                    }
                })
                .unwrap();
            assert!(completed);
            assert_eq!(
                PRE_COPY_DIGEST_COUNT.get(),
                if snapshot { 0 } else { sources }
            );
            assert_eq!(FINAL_DIGEST_COUNT.get(), sources);
            assert_eq!(PLACEMENT_COUNT.get(), placements);
            println!(
                "snapshot={snapshot} P={placements} U={sources} pre={} final={} placements={}",
                PRE_COPY_DIGEST_COUNT.get(),
                FINAL_DIGEST_COUNT.get(),
                PLACEMENT_COUNT.get()
            );
            let paths = fixture
                .snapshots
                .iter()
                .map(|owner| owner.path().to_owned())
                .collect::<Vec<_>>();
            fixture.snapshots.clear();
            assert!(paths.iter().all(|path| !path.exists()));
        }
    }
}

#[test]
fn materialization_rejects_conflicts_before_copying() {
    for snapshot in [false, true] {
        let mut fixture = Fixture::new(2, 1, snapshot);
        fixture.input.instances[1].tree.digest = format!("sha256-{}", "0".repeat(64));
        let error = fixture
            .run(snapshot, |_, _| panic!("unexpected progress"))
            .unwrap_err();
        assert!(error.contains("conflicting"), "{error}");
        assert_eq!(
            fs::read_dir(fixture.project.path().join("stage/node_modules"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(PLACEMENT_COUNT.get(), 0);
        assert_eq!(PRE_COPY_DIGEST_COUNT.get(), 0);
        assert_eq!(FINAL_DIGEST_COUNT.get(), 0);
    }
}

#[test]
fn materialization_rejects_initial_corruption_before_copying() {
    let fixture = Fixture::new(2, 1, false);
    fs::write(
        fixture.trees.values().next().unwrap().join("index.js"),
        b"corrupt",
    )
    .unwrap();
    let error = fixture
        .run(false, |_, _| panic!("unexpected progress"))
        .unwrap_err();
    assert!(error.contains("tree changed during replay"), "{error}");
    assert_eq!(PRE_COPY_DIGEST_COUNT.get(), 1);
    assert_eq!(PLACEMENT_COUNT.get(), 0);
}

#[test]
fn materialization_final_verification_detects_mutation() {
    for snapshot in [false, true] {
        let fixture = Fixture::new(2, 1, snapshot);
        let mut events = Vec::new();
        let error = fixture
            .run(snapshot, |done, total| {
                events.push((done, total));
                assert_eq!(done, 1);
                assert_eq!(FINAL_DIGEST_COUNT.get(), 0);
                fs::write(
                    fixture.trees.values().next().unwrap().join("index.js"),
                    b"changed",
                )
                .unwrap();
            })
            .unwrap_err();
        assert!(error.contains("tree changed while replaying"), "{error}");
        assert_eq!(events, [(1, 2)]);
        assert_eq!(PRE_COPY_DIGEST_COUNT.get(), usize::from(!snapshot));
        assert_eq!(FINAL_DIGEST_COUNT.get(), 1);
        assert_eq!(PLACEMENT_COUNT.get(), 2);
    }
}

#[test]
fn materialization_completion_follows_shims_and_workspace_links() {
    let mut fixture = Fixture::new(1, 1, false);
    let source = fixture.trees.values().next().unwrap();
    fs::write(
        source.join("package.json"),
        br#"{"name":"shared","version":"1.0.0","bin":{"shared-tool":"tool.js"}}"#,
    )
    .unwrap();
    fs::write(source.join("tool.js"), b"#!/usr/bin/env node\n").unwrap();
    let digest = tapid_archive::canonical_tree_digest(source).unwrap();
    for instance in &mut fixture.input.instances {
        instance.tree.digest.clone_from(&digest);
    }
    let member = fixture.project.path().join("packages/member");
    fs::create_dir_all(&member).unwrap();
    fs::write(
        member.join("package.json"),
        br#"{"name":"member","version":"1.0.0"}"#,
    )
    .unwrap();
    let links = tapid_linker::WorkspaceLinkPlan {
        links: vec![tapid_linker::WorkspaceLink {
            name: "member".parse().unwrap(),
            version: "1.0.0".parse().unwrap(),
            source: member,
            target: fixture.project.path().join("node_modules/member"),
        }],
    };
    let stage = fixture.project.path().join("stage");
    FINAL_DIGEST_COUNT.set(0);
    let mut completed = false;
    materialize_stage_with_workspace_links(
        &stage,
        &fixture.plan,
        &fixture.input.instances,
        &fixture.trees,
        false,
        &links,
        &fs::canonicalize(fixture.project.path()).unwrap(),
        |done, total| {
            if done == total {
                assert_eq!(FINAL_DIGEST_COUNT.get(), 1);
                fixture.assert_placements();
                assert!(fs::symlink_metadata(stage.join("node_modules/member")).is_ok());
                #[cfg(unix)]
                assert!(
                    fs::symlink_metadata(stage.join("node_modules/.bin/shared-tool"))
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );
                #[cfg(windows)]
                assert!(stage.join("node_modules/.bin/shared-tool.cmd").is_file());
                completed = true;
            }
        },
    )
    .unwrap();
    assert!(completed);
}
