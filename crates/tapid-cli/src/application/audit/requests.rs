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

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_lockfile::{LockedPackage, Lockfile, RegistryIntegrityProvenance};

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
