use super::*;

/// Verify lifecycle derivations without snapshots, acquisition, or execution.
/// The caller retains the store guard and must separately validate observations
/// and the live layout. The identity provider must use the runner's checked
/// dependency toolchain identity; it is evaluated only for approved hooks.
pub(in crate::application) fn verify_installed_provenance(
    lock: &Lockfile,
    policy: &DependencyLifecyclePolicy,
    guard: &tapid_store::StoreReadGuard,
    mut system_identity: impl FnMut() -> Result<String, OperationalError>,
) -> Result<(), OperationalError> {
    let source_graph = lock.source_graph().to_json().map_err(error)?;
    let mut completed = BTreeMap::<String, Vec<DerivedHookOutput>>::new();
    let mut system = None;
    let order = if policy.approvals().is_empty() {
        lock.packages().keys().cloned().collect()
    } else {
        dependency_order(lock)?
    };
    for key in order {
        let package = &lock.packages()[&key];
        let source = guard.verified_tree_path(&package.tree_digest().parse().map_err(error)?)?;
        let hooks = commands(&source)?;
        let requires_install = hooks.requires_install();
        let mut outputs = Vec::new();
        for (hook, script) in hooks.into_commands() {
            let approval = policy
                .approval_for(
                    package.name(),
                    package.version(),
                    package.artifact_integrity(),
                    &hook,
                    &script,
                )
                .map_err(error)?;
            let Some(approval) = approval else {
                if package
                    .derived_hooks()
                    .iter()
                    .any(|output| output.hook() == hook)
                {
                    return Err(error(format!(
                        "lifecycle approval was removed for {key} {hook}"
                    )));
                }
                if requires_install && hook == "install" {
                    return Err(error(format!(
                        "required dependency lifecycle hook install for {key} is not approved"
                    )));
                }
                continue;
            };
            if !package.has_declared_registry_integrity() {
                return Err(error(
                    "approved dependency scripts require registry-declared archive integrity",
                ));
            }
            if system.is_none() {
                system = Some(system_identity()?);
            }
            let toolchain = tool_identity(approval, system.as_deref().unwrap())?;
            let recipe = lifecycle_recipe(
                policy,
                approval,
                &toolchain,
                &source_graph,
                &completed,
                &outputs,
            )?;
            let output = package
                .derived_hooks()
                .get(outputs.len())
                .filter(|output| output.hook() == hook && output.key() == recipe)
                .ok_or_else(|| {
                    error(format!(
                        "missing or mismatched verified lifecycle output for {key} {hook}"
                    ))
                })?;
            guard.verified_lifecycle_tree_path(
                &recipe.parse().map_err(error)?,
                &output.tree_digest().parse().map_err(error)?,
                output.attestation(),
            )?;
            outputs.push(output.clone());
        }
        if package.derived_hooks() != outputs {
            return Err(error(format!(
                "lifecycle approval was removed or changed for {key}"
            )));
        }
        completed.insert(key, outputs);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_provenance_rejects_revoked_approval_without_repair() {
        let home = tapid_test_support::TempHome::new("installed-policy").unwrap();
        let source = tapid_test_support::TempProject::new("installed-policy-source").unwrap();
        source
            .write(
                "package.json",
                br#"{"name":"demo","version":"1.0.0","scripts":{"postinstall":"echo exact"}}"#,
            )
            .unwrap();
        let digest = tapid_archive::canonical_tree_digest(source.path()).unwrap();
        let store = tapid_store::Store::new(home.path().join("store"));
        let mut transaction = store.transaction();
        transaction
            .stage_verified_tree(&digest.parse().unwrap(), source.path())
            .unwrap();
        transaction.publish().unwrap().commit().unwrap();
        let recipe = format!("sha256-{}", "b".repeat(64));
        let tag = store
            .attest_lifecycle_output(&recipe.parse().unwrap(), &digest.parse().unwrap())
            .unwrap();
        let package = tapid_lockfile::LockedPackage::new_with_provenance(
            "https://registry.npmjs.org", "demo", "1.0.0",
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
            &digest, tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        ).unwrap();
        let key = package.key();
        let mut lock = Lockfile::new(&digest).unwrap();
        lock.insert_package(package).unwrap();
        lock.set_roots([key.clone()]).unwrap();
        lock.set_derived_hooks(
            &key,
            vec![DerivedHookOutput::new("postinstall", &recipe, &digest, &tag).unwrap()],
        )
        .unwrap();
        let before = lock.to_json().unwrap();
        let guard = store.read_guard().unwrap();
        let failure = verify_installed_provenance(
            &lock,
            &DependencyLifecyclePolicy::denied(),
            &guard,
            || panic!("revoked approval must fail before native probing"),
        )
        .unwrap_err();
        assert!(failure.to_string().contains("approval was removed"));
        assert_eq!(lock.to_json().unwrap(), before);
        assert!(guard.verified_tree_path(&digest.parse().unwrap()).is_ok());
        source.write("sh", b"fixture tool bytes").unwrap();
        let tool_digest = format!("sha256-{:x}", Sha256::digest(b"fixture tool bytes"));
        let document = toml::to_string(&serde_json::json!({"schema":1,"approvals":[{
            "package":"demo","version":"1.0.0",
            "archive-digest":lock.packages()[&key].artifact_integrity(),
            "hook":"postinstall","script-digest":format!("sha256-{:x}", Sha256::digest(b"echo exact")),
            "system-toolchain":true,"read":["."],"write":["."],"network":false,"environment":{},
            "timeout-seconds":5,"max-output-bytes":1024,"max-processes":32,"max-memory-bytes":134217728,
            "tools":[{"name":"sh","path":source.path().join("sh"),"digest":tool_digest}]
        }]})).unwrap();
        let policy = DependencyLifecyclePolicy::parse(document.as_bytes()).unwrap();
        let failure = verify_installed_provenance(&lock, &policy, &guard, || {
            Ok("checked-test-toolchain".into())
        })
        .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("mismatched verified lifecycle output")
        );
        drop(guard);
        // Construct the expected replay recipe independently of the verifier.
        let graph = serde_json::to_string(&(
            lock.source_graph().to_json().unwrap(),
            BTreeMap::<String, Vec<DerivedHookOutput>>::new(),
            Vec::<DerivedHookOutput>::new(),
        ))
        .unwrap();
        let mut hash = Sha256::new();
        hash.update(b"checked-test-toolchain");
        let tool_path = source.path().join("sh");
        for value in ["sh", tool_path.to_str().unwrap(), &tool_digest] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
        let toolchain = format!("sha256-{:x}", hash.finalize());
        let recipe = policy.derived_key(&policy.approvals()[0], &toolchain, &graph);
        let tag = store
            .attest_lifecycle_output(&recipe.parse().unwrap(), &digest.parse().unwrap())
            .unwrap();
        lock.set_derived_hooks(
            &key,
            vec![DerivedHookOutput::new("postinstall", &recipe, &digest, &tag).unwrap()],
        )
        .unwrap();
        let guard = store.read_guard().unwrap();
        verify_installed_provenance(&lock, &policy, &guard, || {
            Ok("checked-test-toolchain".into())
        })
        .unwrap();
        let verified = lock.packages()[&key].derived_hooks().to_vec();
        lock.set_derived_hooks(
            &key,
            vec![
                DerivedHookOutput::new(
                    "postinstall",
                    &recipe,
                    &digest,
                    &format!("hmac-sha256-{}", "0".repeat(64)),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        assert!(
            verify_installed_provenance(&lock, &policy, &guard, || Ok(
                "checked-test-toolchain".into()
            ))
            .is_err()
        );
        lock.set_derived_hooks(&key, verified).unwrap();
        assert!(
            verify_installed_provenance(&lock, &policy, &guard, || Ok("replaced-toolchain".into()))
                .is_err()
        );
        assert!(
            verify_installed_provenance(
                &lock,
                &DependencyLifecyclePolicy::denied(),
                &guard,
                || panic!("revoked")
            )
            .is_err()
        );
    }
}
