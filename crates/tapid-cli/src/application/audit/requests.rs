use std::collections::BTreeMap;
use tapid_lockfile::{LockedGraph, LockfilePackageKey};
use tapid_protocol::{
    EvidenceAudience, PackageInstanceWire, SECURITY_CONTRACT_V2, SecurityCheck, SecurityPlatform,
    SecurityRequest, SecuritySubject,
};

pub(super) struct Inventory {
    /// Exact native key retained locally for paths; never substituted for wire placement.
    pub(super) requests: Vec<(String, SecurityRequest)>,
    /// Reachable incoming names without an exact representable security subject.
    pub(super) excluded: usize,
}

/// Projection is not disclosure consent. Callers must independently authorize
/// the entire request before handing it to a provider.
pub(super) fn native(graph: &LockedGraph) -> Inventory {
    let reachable = graph.reachable();
    let mut inventory = Inventory {
        requests: Vec::new(),
        excluded: 0,
    };
    for (key, names) in &reachable.occurrences {
        let identity = &graph.nodes()[key];
        let subject = (|| {
            let wire_package = package(identity)?;
            // Native peer context records name/version, not provider origin.
            // Explanation edges are candidates even when only one matches.
            if !identity.peer_context().ok()?.entries().is_empty() {
                return None;
            }
            let platform = identity.platform_context().ok()?;
            // The wire contract describes a concrete platform, not predicates
            // such as negation or a list of acceptable hosts.
            if [&platform.os, &platform.cpu, &platform.libc]
                .into_iter()
                .flatten()
                .any(|s| {
                    !s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
                })
            {
                return None;
            }
            Some(SecuritySubject {
                package: wire_package,
                requested_name: String::new(),
                // A portable opaque graph identifier avoids putting source URLs
                // or local workspace paths in the placement field.
                placement: format!(
                    "graph/{}",
                    crate::filesystem::atomic::digest_bytes(key.as_bytes())
                        .trim_start_matches("sha256-")
                ),
                peers: BTreeMap::new(),
                platform: SecurityPlatform {
                    os: platform.os,
                    cpu: platform.cpu,
                    libc: platform.libc,
                },
            })
        })();
        for name in names {
            let Some(mut subject) = subject.clone() else {
                inventory.excluded += 1;
                continue;
            };
            subject.requested_name = name.clone();
            let request = SecurityRequest {
                version: SECURITY_CONTRACT_V2.into(),
                subject,
                check: SecurityCheck::Advisory,
                coverage: vec!["advisory".into(), "reported_malware".into()],
                audience: EvidenceAudience::Public,
            };
            if request.validate_v2().is_ok() {
                inventory.requests.push((key.clone(), request));
            } else {
                inventory.excluded += 1;
            }
        }
    }
    inventory
}

fn package(identity: &LockfilePackageKey) -> Option<PackageInstanceWire> {
    Some(PackageInstanceWire {
        registry: identity.source.registry()?.to_string(),
        name: identity.name.to_string(),
        version: identity.version.to_string(),
    })
}

/// Imported peers bind exact placements and therefore retain their own origins.
/// Platform lists and negations cannot be represented by a concrete wire host.
pub(super) fn imported(graph: &tapid_lockfile::ImportedNpmGraph) -> Inventory {
    let mut inventory = Inventory {
        requests: Vec::new(),
        excluded: 0,
    };
    let wire = |package: &tapid_lockfile::ImportedNpmPackage| PackageInstanceWire {
        registry: package.registry.to_string(),
        name: package.name.to_string(),
        version: package.version.to_string(),
    };
    for (key, names) in graph.reachable().occurrences {
        for name in names {
            let request = (|| {
                let package = graph.packages.get(&key)?;
                let mut dimensions = Vec::new();
                for values in [&package.os, &package.cpu, &package.libc] {
                    dimensions.push(match values.as_slice() {
                        [] => None,
                        [value]
                            if value
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)) =>
                        {
                            Some(value.clone())
                        }
                        _ => return None,
                    });
                }
                Some(SecurityRequest {
                    version: SECURITY_CONTRACT_V2.into(),
                    subject: SecuritySubject {
                        package: wire(package),
                        requested_name: name,
                        placement: format!(
                            "graph/{}",
                            crate::filesystem::atomic::digest_bytes(key.as_bytes())
                                .trim_start_matches("sha256-")
                        ),
                        peers: package
                            .peers
                            .iter()
                            .map(|(name, path)| (name.clone(), wire(&graph.packages[path])))
                            .collect(),
                        platform: SecurityPlatform {
                            os: dimensions[0].clone(),
                            cpu: dimensions[1].clone(),
                            libc: dimensions[2].clone(),
                        },
                    },
                    check: SecurityCheck::Advisory,
                    coverage: vec!["advisory".into(), "reported_malware".into()],
                    audience: EvidenceAudience::Public,
                })
            })();
            match request {
                Some(request) if request.validate_v2().is_ok() => {
                    inventory.requests.push((key.clone(), request))
                }
                _ => inventory.excluded += 1,
            }
        }
    }
    inventory
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_lockfile::{LockedPackage, Lockfile, RegistryIntegrityProvenance};

    #[test]
    fn imported_requests_preserve_exact_peer_origins_and_exclude_platform_alternatives() {
        let mut input: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tapid-lockfile/tests/fixtures/npm-import-package-lock.json"
        ))
        .unwrap();
        input["packages"]["node_modules/shared"]["resolved"] =
            serde_json::json!("https://private.example/shared/-/shared-1.0.0.tgz");
        input["packages"]["node_modules/consumer"]["resolved"] =
            serde_json::json!("https://private.example/consumer/-/consumer-1.0.0.tgz");
        input["packages"]["node_modules/native"]["os"] = serde_json::json!(["linux", "darwin"]);
        input["packages"]["node_modules/parent"]["dependencies"] =
            serde_json::json!({"renamed":"npm:shared@2.0.0"});
        let mut nested = input["packages"]
            .as_object_mut()
            .unwrap()
            .remove("node_modules/parent/node_modules/shared")
            .unwrap();
        nested["name"] = serde_json::json!("shared");
        input["packages"]["node_modules/parent/node_modules/renamed"] = nested;
        let lock = tapid_lockfile::ImportedNpmLockfile::import(
            &input.to_string(),
            &input["packages"][""].to_string(),
            &format!("sha256-{}", "a".repeat(64)),
        )
        .unwrap();
        let graph = lock.graph().unwrap();
        let inventory = imported(&graph);
        assert_eq!(inventory.excluded, 1);
        assert_eq!(inventory.requests.len(), 4);
        let (_, consumer) = inventory
            .requests
            .iter()
            .find(|(key, _)| key == "node_modules/consumer")
            .unwrap();
        assert_eq!(
            consumer.subject.peers["shared"].registry,
            "https://private.example"
        );
        assert_eq!(consumer.subject.peers["shared"].version, "1.0.0");
        let (_, nested) = inventory
            .requests
            .iter()
            .find(|(key, _)| key == "node_modules/parent/node_modules/renamed")
            .unwrap();
        assert_eq!(nested.subject.requested_name, "renamed");
        assert_eq!(nested.subject.package.name, "shared");
        assert_eq!(
            nested.subject.package.registry,
            "https://registry.npmjs.org"
        );
        assert_eq!(nested.subject.package.version, "2.0.0");
        assert!(
            inventory
                .requests
                .iter()
                .all(|(_, r)| r.validate_v2().is_ok())
        );
        assert_eq!(inventory.requests, imported(&graph).requests);
    }

    fn package(registry: &str, name: &str) -> LockedPackage {
        LockedPackage::new_with_provenance(
            registry,
            name,
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "a".repeat(64)),
            RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap()
    }

    #[test]
    fn native_requests_preserve_registry_alias_and_exact_graph_mapping_without_orphans() {
        let public = package("https://registry.npmjs.org", "shared");
        let private = package("https://private.example", "shared");
        let orphan = package("https://registry.npmjs.org", "orphan");
        let mut root = package("https://registry.npmjs.org", "root");
        root.add_dependency("shared", &public.key()).unwrap();
        root.add_alias_dependency("renamed", &private.key())
            .unwrap();
        root.add_alias_dependency("second-name", &private.key())
            .unwrap();
        let root_key = root.key();
        let private_key = private.key();
        let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
        lock.insert_packages([root, public, private, orphan])
            .unwrap();
        lock.set_roots([root_key]).unwrap();
        let graph = lock.graph().unwrap();
        let inventory = native(&graph);
        assert_eq!(inventory.requests.len(), 4);
        assert_eq!(inventory.excluded, 0);
        let (key, request) = inventory
            .requests
            .iter()
            .find(|(_, r)| r.subject.requested_name == "renamed")
            .unwrap();
        assert_eq!(key, &private_key);
        assert_eq!(request.subject.package.name, "shared");
        assert_eq!(request.subject.package.registry, "https://private.example");
        assert_eq!(request.coverage, ["advisory", "reported_malware"]);
        assert_eq!(request.version, "tapid-security-v2");
        assert!(
            inventory
                .requests
                .iter()
                .all(|(_, r)| r.validate_v2().is_ok())
        );
        assert!(
            inventory
                .requests
                .iter()
                .all(|(_, r)| !r.subject.placement.contains("private.example"))
        );
        assert_eq!(inventory.requests, native(&graph).requests);
        let alias = inventory
            .requests
            .iter()
            .find(|(_, r)| r.subject.requested_name == "second-name")
            .unwrap();
        assert_eq!(alias.0, private_key);
        assert_eq!(alias.1.subject.placement, request.subject.placement);
    }

    #[test]
    fn native_requests_exclude_candidate_peers_even_with_one_same_origin_provider() {
        let peer = tapid_core::PeerContext::default()
            .with("provider".parse().unwrap(), "1.0.0".parse().unwrap());
        let platform =
            tapid_core::PlatformContext::new(Some("linux"), Some("arm64"), Some("musl")).unwrap();
        let consumer = LockedPackage::new_with_context_and_provenance(
            "https://private.example",
            "consumer",
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "a".repeat(64)),
            (&peer, &platform),
            RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap();
        let key = consumer.key();
        let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
        lock.insert_packages([consumer, package("https://private.example", "provider")])
            .unwrap();
        lock.set_roots([key]).unwrap();
        let inventory = native(&lock.graph().unwrap());
        assert_eq!(
            inventory.excluded, 1,
            "a unique candidate is not proof of peer origin"
        );
        assert_eq!(inventory.requests.len(), 1);
        assert_eq!(inventory.requests[0].1.subject.package.name, "provider");
        let alternative = LockedPackage::new_with_context_and_provenance(
            "https://private.example",
            "provider",
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "a".repeat(64)),
            (&tapid_core::PeerContext::default(), &platform),
            RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap();
        lock.insert_package(alternative).unwrap();
        let ambiguous = native(&lock.graph().unwrap());
        assert_eq!(ambiguous.excluded, 1);
        assert!(
            ambiguous
                .requests
                .iter()
                .all(|(_, r)| r.subject.package.name == "provider")
        );
    }

    #[test]
    fn native_requests_preserve_platforms_and_exclude_predicates_and_copied_sources() {
        let contexts = ["linux", "!linux"];
        let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
        let mut roots = Vec::new();
        for os in contexts {
            let platform =
                tapid_core::PlatformContext::new(Some(os), Some("arm64"), Some("musl")).unwrap();
            let package = LockedPackage::new_with_context_and_provenance(
                "https://registry.npmjs.org",
                "native",
                "1.0.0",
                &format!("sha512-{}==", "A".repeat(86)),
                &format!("sha256-{}", "a".repeat(64)),
                (&tapid_core::PeerContext::default(), &platform),
                RegistryIntegrityProvenance::RegistryDeclared,
            )
            .unwrap();
            roots.push(package.key());
            lock.insert_package(package).unwrap();
        }
        let copied = package(
            &format!("file:vendor/tool.tgz#sha256-{}", "a".repeat(64)),
            "copied",
        );
        roots.push(copied.key());
        lock.insert_package(copied).unwrap();
        lock.set_roots(roots).unwrap();
        let inventory = native(&lock.graph().unwrap());
        assert_eq!(inventory.excluded, 2);
        assert_eq!(inventory.requests.len(), 1);
        let subject = &inventory.requests[0].1.subject;
        assert_eq!(subject.platform.os.as_deref(), Some("linux"));
        assert_eq!(subject.platform.cpu.as_deref(), Some("arm64"));
        assert_eq!(subject.platform.libc.as_deref(), Some("musl"));
        assert!(subject.peers.is_empty());
    }
}
