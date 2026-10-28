use super::*;

/// The exact graph exposed to dependency-enabled hooks and bound into recipes.
pub(super) struct DependencyInputs {
    bindings: BTreeMap<String, BTreeMap<String, String>>,
    has_workspace: bool,
    pub(super) order: Vec<String>,
}

pub(super) fn identity(key: &str) -> Result<tapid_linker::InstanceKey, OperationalError> {
    let typed: tapid_lockfile::LockfilePackageKey = key.parse().map_err(error)?;
    Ok(tapid_linker::InstanceKey {
        peer_context: typed.peer_context().map_err(error)?,
        platform_context: typed.platform_context().map_err(error)?,
        id: tapid_core::PackageInstanceId::from_source(
            typed
                .source
                .package_source()
                .ok_or_else(|| error("lifecycle package has no artifact source"))?,
            typed.name,
            typed.version,
        ),
    })
}

impl DependencyInputs {
    pub(super) fn new(lock: &Lockfile) -> Result<Self, OperationalError> {
        let mut bindings = BTreeMap::new();
        for (key, package) in lock.packages() {
            let mut children = package.dependencies().clone();
            let typed: tapid_lockfile::LockfilePackageKey = key.parse().map_err(error)?;
            for (name, version) in typed.peer_context().map_err(error)?.entries() {
                // Native resolution binds peers to direct root providers. Do not
                // infer an origin or context from a matching name and version.
                let candidates = if let Some(target) = children.get(name.as_str()) {
                    vec![target.clone()]
                } else if let Some(target) = lock.root_bindings().get(name.as_str()) {
                    vec![target.clone()]
                } else if lock.root_bindings().is_empty() {
                    lock.roots()
                        .iter()
                        .filter_map(|root| {
                            let target = identity(root).ok()?;
                            (target.id.name == *name && target.id.version == *version)
                                .then(|| root.clone())
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                if candidates.len() != 1 {
                    return Err(error(format!(
                        "unsupported lifecycle peer provider for {key}: {name}@{version} requires one exact root binding"
                    )));
                }
                let target = &candidates[0];
                let provider = identity(target)?;
                if provider.id.name != *name
                    || provider.id.version != *version
                    || !lock.packages().contains_key(target)
                {
                    return Err(error(format!(
                        "mismatched lifecycle peer provider for {key}: {name}@{version}"
                    )));
                }
                children.insert(name.to_string(), target.clone());
            }
            children.retain(|_, target| lock.packages().contains_key(target));
            bindings.insert(key.clone(), children);
        }
        fn visit(
            key: &str,
            bindings: &BTreeMap<String, BTreeMap<String, String>>,
            active: &mut BTreeSet<String>,
            done: &mut BTreeSet<String>,
            order: &mut Vec<String>,
        ) -> Result<(), OperationalError> {
            if done.contains(key) {
                return Ok(());
            }
            if active.len() >= 1024 || !active.insert(key.into()) {
                return Err(error(
                    "approved dependency lifecycle graphs must be acyclic and no deeper than 1024 packages",
                ));
            }
            for child in bindings[key].values() {
                visit(child, bindings, active, done, order)?;
            }
            active.remove(key);
            done.insert(key.into());
            order.push(key.into());
            Ok(())
        }
        let mut order = Vec::new();
        let mut done = BTreeSet::new();
        let mut active = BTreeSet::new();
        for key in bindings.keys() {
            visit(key, &bindings, &mut active, &mut done, &mut order)?;
        }
        let mut depths = BTreeMap::new();
        for key in &order {
            let depth = bindings[key]
                .values()
                .map(|child| depths[child])
                .max()
                .unwrap_or(0)
                + 1;
            if depth > 1024 {
                return Err(error(
                    "approved dependency lifecycle graphs must be acyclic and no deeper than 1024 packages",
                ));
            }
            depths.insert(key.clone(), depth);
        }
        Ok(Self {
            bindings,
            has_workspace: !lock.workspace_packages().is_empty(),
            order,
        })
    }

    pub(super) fn closure(&self, key: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut pending = self.bindings[key].values().cloned().collect::<Vec<_>>();
        while let Some(next) = pending.pop() {
            if found.insert(next.clone()) {
                pending.extend(self.bindings[&next].values().cloned());
            }
        }
        found
    }

    pub(super) fn evidence(
        &self,
        key: &str,
        dependencies: bool,
        source_graph: &str,
        completed: &BTreeMap<String, Vec<DerivedHookOutput>>,
        outputs: &[DerivedHookOutput],
    ) -> Result<String, OperationalError> {
        let readable = if dependencies {
            self.closure(key)
        } else {
            BTreeSet::new()
        };
        let bound_outputs = completed
            .iter()
            .filter(|(key, _)| readable.contains(*key))
            .collect::<BTreeMap<_, _>>();
        serde_json::to_string(&(
            "tapid-lifecycle-inputs-v2",
            key,
            source_graph,
            bound_outputs,
            outputs,
        ))
        .map_err(error)
    }

    pub(super) fn layout(
        &self,
        key: &str,
        input: &NamedLayoutInput,
    ) -> Result<NamedLayoutInput, OperationalError> {
        let closure = self.closure(key);
        if self.has_workspace {
            return Err(error(
                "dependency lifecycle scripts cannot read live workspace packages; use published verified dependencies",
            ));
        }
        let identities = closure
            .iter()
            .map(|key| identity(key))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let dependencies =
            |parent: &str| -> Result<Vec<tapid_linker::NamedDependency>, OperationalError> {
                self.bindings[parent]
                    .iter()
                    .map(|(name, target)| {
                        Ok(tapid_linker::NamedDependency {
                            name: name.parse().map_err(error)?,
                            child: identity(target)?,
                        })
                    })
                    .collect()
            };
        let instances = input
            .instances
            .iter()
            .filter(|instance| identities.contains(&tapid_linker::InstanceKey::from(*instance)))
            .cloned()
            .collect::<Vec<_>>();
        if instances.len() != identities.len() {
            return Err(error("lifecycle dependency input mapping lost"));
        }
        let mut dependency_edges = Vec::new();
        for parent in &closure {
            for dependency in dependencies(parent)? {
                dependency_edges.push(tapid_linker::NamedDependencyEdge {
                    parent: identity(parent)?,
                    dependency,
                });
            }
        }
        Ok(NamedLayoutInput {
            instances,
            root_dependencies: dependencies(key)?,
            dependency_edges,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str) -> tapid_lockfile::LockedPackage {
        contextual_package(name, &tapid_core::PeerContext::default())
    }

    fn contextual_package(
        name: &str,
        peer: &tapid_core::PeerContext,
    ) -> tapid_lockfile::LockedPackage {
        tapid_lockfile::LockedPackage::new_with_context_and_provenance(
            "https://registry.npmjs.org",
            name,
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "0".repeat(64)),
            (
                peer,
                &tapid_core::PlatformContext::new(None, None, None).unwrap(),
            ),
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap()
    }

    #[test]
    fn peer_inputs_require_an_exact_provider_and_participate_in_ordering() {
        let runtime = package("runtime");
        let mut provider = package("host");
        provider.add_dependency("runtime", &runtime.key()).unwrap();
        let selected = contextual_package(
            "plugin",
            &tapid_core::PeerContext::default()
                .with("host".parse().unwrap(), "1.0.0".parse().unwrap()),
        );
        let mut lock = Lockfile::new(&format!("sha256-{}", "0".repeat(64))).unwrap();
        for p in [&runtime, &provider, &selected] {
            lock.insert_package(p.clone()).unwrap();
        }
        assert!(
            DependencyInputs::new(&lock)
                .err()
                .unwrap()
                .to_string()
                .contains("requires one exact root binding")
        );
        lock.set_roots([provider.key(), selected.key()]).unwrap();
        let graph = DependencyInputs::new(&lock).unwrap();
        assert_eq!(
            graph.closure(&selected.key()),
            BTreeSet::from([provider.key(), runtime.key()])
        );
        let position = |key: &str| graph.order.iter().position(|k| k == key).unwrap();
        assert!(position(&runtime.key()) < position(&provider.key()));
        assert!(position(&provider.key()) < position(&selected.key()));
        assert_eq!(graph.bindings[&selected.key()]["host"], provider.key());
        lock.set_root_bindings(BTreeMap::from([
            ("renamed-host".into(), provider.key()),
            ("plugin".into(), selected.key()),
        ]))
        .unwrap();
        assert!(
            DependencyInputs::new(&lock)
                .err()
                .unwrap()
                .to_string()
                .contains("requires one exact root binding")
        );
        // An explicit conflicting local binding cannot be repaired by choosing
        // another registry package that merely has the expected version.
        lock.set_roots([runtime.key(), provider.key(), selected.key()])
            .unwrap();
        lock.set_root_bindings(BTreeMap::from([
            ("host".into(), runtime.key()),
            ("actual-host".into(), provider.key()),
            ("plugin".into(), selected.key()),
        ]))
        .unwrap();
        assert!(
            DependencyInputs::new(&lock)
                .err()
                .unwrap()
                .to_string()
                .contains("mismatched lifecycle peer provider")
        );
    }

    #[test]
    fn dependency_inputs_reject_cycles_and_depth_even_with_cached_visits() {
        let mut a = package("a");
        let mut b = package("b");
        a.add_dependency("b", &b.key()).unwrap();
        b.add_dependency("a", &a.key()).unwrap();
        let mut lock = Lockfile::new(&format!("sha256-{}", "0".repeat(64))).unwrap();
        lock.insert_graph([a, b], []).unwrap();
        assert!(
            DependencyInputs::new(&lock)
                .err()
                .unwrap()
                .to_string()
                .contains("acyclic")
        );
        let mut lock = Lockfile::new(&format!("sha256-{}", "0".repeat(64))).unwrap();
        let mut previous = None::<tapid_lockfile::LockedPackage>;
        for index in 0..1025 {
            let mut next = package(&format!("n{index:04}"));
            if let Some(prior) = previous {
                next.add_dependency(prior.name(), &prior.key()).unwrap();
            }
            lock.insert_package(next.clone()).unwrap();
            previous = Some(next);
        }
        assert!(
            DependencyInputs::new(&lock)
                .err()
                .unwrap()
                .to_string()
                .contains("no deeper than 1024")
        );
    }

    #[test]
    fn projection_contains_transitive_inputs_but_not_unrelated_outputs() {
        let c = package("c");
        let mut b = package("b");
        b.add_dependency("c", &c.key()).unwrap();
        let mut a = package("a");
        a.add_alias_dependency("local-b", &b.key()).unwrap();
        let unrelated = package("unrelated");
        let mut lock = Lockfile::new(&format!("sha256-{}", "0".repeat(64))).unwrap();
        for p in [&c, &b, &a, &unrelated] {
            lock.insert_package(p.clone()).unwrap();
        }
        let graph = DependencyInputs::new(&lock).unwrap();
        assert_eq!(graph.closure(&a.key()), BTreeSet::from([b.key(), c.key()]));
        let project = tapid_test_support::TempProject::new("projected-inputs").unwrap();
        let input = NamedLayoutInput {
            instances: lock
                .packages()
                .keys()
                .map(|key| {
                    let typed: tapid_lockfile::LockfilePackageKey = key.parse().unwrap();
                    let source = PathBuf::from("sources").join(typed.name.as_str());
                    project.write(source.join("package.json"), &serde_json::to_vec(&serde_json::json!({
                        "name": typed.name.as_str(), "version": "1.0.0", "bin": {format!("{}-tool", typed.name): "bin.js"}
                    })).unwrap()).unwrap();
                    project.write(source.join("bin.js"), b"#!/usr/bin/env node\n").unwrap();
                    let source = project.path().join(source);
                    let digest = tapid_archive::canonical_tree_digest(&source).unwrap();
                    tapid_linker::PackageInstance {
                        id: tapid_core::PackageInstanceId::from_source(
                            typed.source.package_source().unwrap(),
                            typed.name,
                            typed.version,
                        ),
                        peer_context: tapid_core::PeerContext::default(),
                        platform_context: tapid_core::PlatformContext::new(None, None, None)
                            .unwrap(),
                        tree: tapid_linker::VerifiedTreeReference::new(
                            &digest,
                            source,
                        )
                        .unwrap(),
                    }
                })
                .collect(),
            root_dependencies: vec![],
            dependency_edges: vec![],
        };
        let projected = graph.layout(&a.key(), &input).unwrap();
        assert_eq!(projected.instances.len(), 2);
        assert_eq!(projected.root_dependencies.len(), 1);
        assert_eq!(projected.root_dependencies[0].name.as_str(), "local-b");
        assert_eq!(projected.dependency_edges.len(), 1);
        assert!(
            projected
                .instances
                .iter()
                .all(|p| p.id.name.as_str() != "unrelated" && p.id.name.as_str() != "a")
        );
        let stage = project.path().join("stage");
        let plan = tapid_linker::plan_named_layout(
            tapid_linker::ManagedRoot::new(&stage).unwrap(),
            projected.clone(),
            crate::application::replay::current_platform(),
        )
        .unwrap();
        let trees = lock
            .packages()
            .keys()
            .map(|key| {
                let id = identity(key).unwrap();
                (
                    key.clone(),
                    input
                        .instances
                        .iter()
                        .find(|instance| tapid_linker::InstanceKey::from(*instance) == id)
                        .unwrap()
                        .tree
                        .root
                        .clone(),
                )
            })
            .collect();
        crate::filesystem::tree::materialize_stage_with_workspace_links(
            &stage,
            &plan,
            &projected.instances,
            &trees,
            false,
            &tapid_linker::WorkspaceLinkPlan::default(),
            project.path(),
            |_, _| {},
        )
        .unwrap();
        assert!(stage.join("node_modules/local-b/package.json").is_file());
        let shim = if cfg!(windows) {
            "b-tool.cmd"
        } else {
            "b-tool"
        };
        assert!(stage.join("node_modules/.bin").join(shim).is_file());
        assert!(!stage.join("node_modules/unrelated").exists());
        assert!(!stage.join("node_modules/.bin/unrelated-tool").exists());
        assert!(!stage.join("node_modules/.bin/unrelated-tool.cmd").exists());
        assert!(
            plan.entries
                .iter()
                .all(|entry| !entry.source.ends_with("unrelated"))
        );
    }

    #[test]
    fn recipe_evidence_tracks_only_readable_generated_inputs_and_prior_hooks() {
        let dependency = package("dependency");
        let mut selected = package("selected");
        selected
            .add_dependency("dependency", &dependency.key())
            .unwrap();
        let unrelated = package("unrelated");
        let mut lock = Lockfile::new(&format!("sha256-{}", "0".repeat(64))).unwrap();
        for p in [&dependency, &selected, &unrelated] {
            lock.insert_package(p.clone()).unwrap();
        }
        let graph = DependencyInputs::new(&lock).unwrap();
        let output = DerivedHookOutput::new(
            "install",
            &format!("sha256-{}", "1".repeat(64)),
            &format!("sha256-{}", "2".repeat(64)),
            &format!("hmac-sha256-{}", "3".repeat(64)),
        )
        .unwrap();
        let mut completed = BTreeMap::new();
        let before = graph
            .evidence(&selected.key(), true, "source graph", &completed, &[])
            .unwrap();
        completed.insert(unrelated.key(), vec![output.clone()]);
        assert_eq!(
            before,
            graph
                .evidence(&selected.key(), true, "source graph", &completed, &[])
                .unwrap()
        );
        completed.insert(dependency.key(), vec![output.clone()]);
        assert_ne!(
            before,
            graph
                .evidence(&selected.key(), true, "source graph", &completed, &[])
                .unwrap()
        );
        assert_eq!(
            graph
                .evidence(
                    &selected.key(),
                    false,
                    "source graph",
                    &BTreeMap::new(),
                    &[]
                )
                .unwrap(),
            graph
                .evidence(&selected.key(), false, "source graph", &completed, &[])
                .unwrap()
        );
        assert_ne!(
            before,
            graph
                .evidence(
                    &selected.key(),
                    true,
                    "source graph",
                    &BTreeMap::new(),
                    &[output]
                )
                .unwrap()
        );
    }
}
