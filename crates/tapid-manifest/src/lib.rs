//! Parsing and validation for npm-compatible `package.json` manifests.

#![deny(unsafe_code)]

mod error;
mod lifecycle;
mod model;
mod parse;
mod workspace;

pub use error::ManifestError;
pub use lifecycle::{dependency_lifecycle_commands, dependency_lifecycle_hooks};
pub use model::{BinTarget, DependencyKind, PackageBin, PackageManifest};
pub use workspace::{Workspace, WorkspaceMember};

/// Returns the current crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_supported_metadata() {
        let manifest = PackageManifest::parse(r#"{"name":"example-app","version":"1.2.3","private":true,"dependencies":{"kleur":"^4.1.5"},"scripts":{"test":"cargo test"}}"#).unwrap();
        assert_eq!(manifest.name().as_str(), "example-app");
        assert_eq!(manifest.version().to_string(), "1.2.3");
        assert!(manifest.is_private());
        assert_eq!(manifest.dependencies()["kleur"], "^4.1.5");
        assert_eq!(manifest.scripts()["test"], "cargo test");
    }

    #[test]
    fn parses_and_preserves_simple_overrides_when_updating_dependencies() {
        let manifest = PackageManifest::parse(
            r#"{"name":"example-app","version":"1.2.3","overrides":{"postcss":"8.5.28"}}"#,
        )
        .unwrap();

        assert_eq!(manifest.overrides()["postcss"], "8.5.28");
        let updated = manifest.with_dependency("next", "15.5.27").unwrap();
        let value: serde_json::Value = serde_json::from_str(&updated.to_json()).unwrap();
        assert_eq!(value["overrides"]["postcss"], "8.5.28");
    }

    #[test]
    fn preserves_version_qualified_override_selectors_from_package_manifests() {
        let manifest = PackageManifest::parse(
            r#"{"name":"typescript","version":"5.9.3","overrides":{"typescript@*":"$typescript"}}"#,
        )
        .unwrap();

        assert_eq!(manifest.overrides()["typescript@*"], "$typescript");
        let value: serde_json::Value = serde_json::from_str(&manifest.to_json()).unwrap();
        assert_eq!(value["overrides"]["typescript@*"], "$typescript");
    }

    #[test]
    fn discovers_workspace_members_in_deterministic_order() {
        let root = unique_temp_dir("workspace-array");
        std::fs::create_dir_all(root.join("packages/zeta")).unwrap();
        std::fs::create_dir_all(root.join("packages/alpha")).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("packages/zeta/package.json"),
            r#"{"name":"zeta","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("packages/alpha/package.json"),
            r#"{"name":"alpha","version":"1.0.0"}"#,
        )
        .unwrap();

        let workspace = Workspace::discover(&root).unwrap();
        assert_eq!(
            workspace
                .members()
                .iter()
                .map(|m| m.name())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert_eq!(
            workspace.select(Some("zeta")).unwrap().name().as_str(),
            "zeta"
        );
        assert_eq!(workspace.select(None).unwrap().name().as_str(), "root");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_single_string_workspace_pattern() {
        let root = unique_temp_dir("workspace-string");
        std::fs::create_dir_all(root.join("packages/web")).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"root","version":"1.0.0","workspaces":"packages/*"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("packages/web/package.json"),
            r#"{"name":"web","version":"1.0.0"}"#,
        )
        .unwrap();

        let workspace = Workspace::discover(&root).unwrap();
        assert_eq!(
            workspace
                .members()
                .iter()
                .map(WorkspaceMember::name)
                .collect::<Vec<_>>(),
            ["web"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ignores_file_symlink_entries_during_workspace_glob_expansion() {
        use std::os::unix::fs::symlink;

        let root = unique_temp_dir("workspace-file-symlink");
        std::fs::create_dir_all(root.join("packages/web")).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("packages/web/package.json"),
            r#"{"name":"web","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(root.join("notes.txt"), "not a workspace directory").unwrap();
        symlink(root.join("notes.txt"), root.join("packages/notes-link")).unwrap();

        let workspace = Workspace::discover(&root).unwrap();
        assert_eq!(
            workspace
                .members()
                .iter()
                .map(WorkspaceMember::name)
                .collect::<Vec<_>>(),
            ["web"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_workspace_glob_directory_symlinks_that_escape_the_project() {
        use std::os::unix::fs::symlink;

        let root = unique_temp_dir("workspace-symlink");
        let outside = unique_temp_dir("workspace-symlink-outside");
        std::fs::create_dir_all(root.join("packages")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
        std::fs::write(
            outside.join("package.json"),
            r#"{"name":"outside","version":"1.0.0"}"#,
        )
        .unwrap();
        symlink(&outside, root.join("packages/escape")).unwrap();

        let error = Workspace::discover(&root).unwrap_err();
        assert!(
            error.contains("outside project"),
            "unexpected error: {error}"
        );

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn rejects_duplicate_package_names_across_distinct_workspace_members() {
        let root = unique_temp_dir("workspace-duplicate-name");
        for member in ["packages/first", "packages/second"] {
            std::fs::create_dir_all(root.join(member)).unwrap();
            std::fs::write(
                root.join(member).join("package.json"),
                r#"{"name":"shared","version":"1.0.0"}"#,
            )
            .unwrap();
        }
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();

        let error = Workspace::discover(&root).unwrap_err();
        assert!(
            error.contains("duplicate workspace package name"),
            "{error}"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_object_workspace_packages_and_rejects_unknown_selection() {
        let root = unique_temp_dir("workspace-object");
        std::fs::create_dir_all(root.join("apps/web")).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"root","version":"1.0.0","workspaces":{"packages":["apps/*"]}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("apps/web/package.json"),
            r#"{"name":"web","version":"1.0.0"}"#,
        )
        .unwrap();

        let workspace = Workspace::discover(&root).unwrap();
        assert_eq!(
            workspace.members()[0].path(),
            std::fs::canonicalize(root.join("apps/web/package.json")).unwrap()
        );
        assert!(workspace.select(Some("missing")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_object_workspace_packages_from_single_string() {
        let root = unique_temp_dir("workspace-object-string");
        std::fs::create_dir_all(root.join("apps/web")).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"root","version":"1.0.0","workspaces":{"packages":"apps/*"}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("apps/web/package.json"),
            r#"{"name":"web","version":"1.0.0"}"#,
        )
        .unwrap();

        let workspace = Workspace::discover(&root).unwrap();
        assert_eq!(
            workspace
                .members()
                .iter()
                .map(WorkspaceMember::name)
                .collect::<Vec<_>>(),
            ["web"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("tapid-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn preserves_bin_when_serializing_after_dependency_update() {
        let manifest = PackageManifest::parse(
            r#"{"name":"tool","version":"1.0.0","bin":{"tool":"./cli.js"}}"#,
        )
        .unwrap()
        .with_dependency("is-char", "*")
        .unwrap();
        let json = manifest.to_json();
        assert!(json.contains("\"bin\""));
        assert!(json.contains("\"tool\": \"cli.js\""));
        assert!(json.contains("\"is-char\": \"*\""));
    }

    #[test]
    fn mutates_each_dependency_kind_without_cross_contamination() {
        let manifest = PackageManifest::new("example-app", "1.2.3", true)
            .unwrap()
            .with_dependency_kind(DependencyKind::Dependencies, "is-char", "*")
            .unwrap()
            .with_dependency_kind(DependencyKind::DevDependencies, "tapid-dev", "^1.0.0")
            .unwrap()
            .with_dependency_kind(
                DependencyKind::OptionalDependencies,
                "optional-pkg",
                "~2.0.0",
            )
            .unwrap()
            .with_dependency_kind(DependencyKind::PeerDependencies, "peer-pkg", ">=3.0.0")
            .unwrap();

        assert_eq!(manifest.dependencies()["is-char"], "*");
        assert_eq!(manifest.dev_dependencies()["tapid-dev"], "^1.0.0");
        assert_eq!(manifest.optional_dependencies()["optional-pkg"], "~2.0.0");
        assert_eq!(manifest.peer_dependencies()["peer-pkg"], ">=3.0.0");

        let manifest = manifest.without_dependency("is-char").unwrap();
        assert!(!manifest.dependencies().contains_key("is-char"));
        assert!(manifest.dev_dependencies().contains_key("tapid-dev"));
        assert!(
            manifest
                .optional_dependencies()
                .contains_key("optional-pkg")
        );
        assert!(manifest.peer_dependencies().contains_key("peer-pkg"));
    }

    #[test]
    fn updates_existing_dependency_sections_without_moving_declarations() {
        let manifest = PackageManifest::parse(
            r#"{"name":"app","version":"1.0.0","dependencies":{"npm:foo":"^1"},"devDependencies":{"npm:foo":"^2"},"optionalDependencies":{"npm:foo":"^3"},"peerDependencies":{"npm:foo":"^4"},"customMetadata":{"kept":true}}"#,
        ).unwrap();
        for (kind, field) in [
            (DependencyKind::Dependencies, "dependencies"),
            (DependencyKind::DevDependencies, "devDependencies"),
            (DependencyKind::OptionalDependencies, "optionalDependencies"),
            (DependencyKind::PeerDependencies, "peerDependencies"),
        ] {
            let updated = manifest
                .clone()
                .update_dependency_kind(kind, "npm:foo", " * ")
                .unwrap();
            let mut expected: serde_json::Value =
                serde_json::from_str(&manifest.to_json()).unwrap();
            expected[field]["npm:foo"] = "*".into();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&updated.to_json()).unwrap(),
                expected
            );

            let error = manifest
                .clone()
                .update_dependency_kind(kind, "missing", "*")
                .unwrap_err();
            assert!(
                matches!(error, ManifestError::DependencyNotDeclared { field: actual, name } if actual == field && name == "missing")
            );
        }
        let moved = manifest
            .with_dependency_kind(DependencyKind::DevDependencies, "npm:foo", "*")
            .unwrap();
        assert_eq!(moved.dev_dependencies()["npm:foo"], "*");
        assert!(!moved.dependencies().contains_key("npm:foo"));
        assert!(!moved.optional_dependencies().contains_key("npm:foo"));
        assert!(!moved.peer_dependencies().contains_key("npm:foo"));
    }

    #[test]
    fn preserves_unmodeled_package_json_fields_when_updating_dependencies() {
        let manifest = PackageManifest::parse(
            r#"{
                "name": "example-app",
                "version": "1.2.3",
                "private": true,
                "type": "module",
                "engines": {"node": ">=20"},
                "exports": {".": "./src/index.js"},
                "customMetadata": ["kept", 42],
                "dependencies": {"kleur": "^4.1.5"}
            }"#,
        )
        .unwrap()
        .with_dependency("chalk", "^5.3.0")
        .unwrap();

        let value: serde_json::Value = serde_json::from_str(&manifest.to_json()).unwrap();
        assert_eq!(value["type"], "module");
        assert_eq!(value["engines"]["node"], ">=20");
        assert_eq!(value["exports"]["."], "./src/index.js");
        assert_eq!(value["customMetadata"], serde_json::json!(["kept", 42]));
        assert_eq!(value["dependencies"]["kleur"], "^4.1.5");
        assert_eq!(value["dependencies"]["chalk"], "^5.3.0");
    }

    #[test]
    fn rejects_malformed_and_invalid_documents() {
        assert!(PackageManifest::parse("not json").is_err());
        assert!(PackageManifest::parse(r#"{"version":"1.0.0"}"#).is_err());
        assert!(PackageManifest::parse(r#"{"name":"app","version":"1"}"#).is_err());
    }

    #[test]
    fn serializes_a_deterministic_minimal_manifest() {
        let manifest = PackageManifest::new("example-app", "0.1.0", true).unwrap();
        assert_eq!(
            manifest.to_json(),
            "{\n  \"name\": \"example-app\",\n  \"version\": \"0.1.0\",\n  \"private\": true\n}\n"
        );
    }

    #[test]
    fn parses_string_and_object_bin_forms_deterministically() {
        let manifest = PackageManifest::parse(
            r#"{"name":"@scope/tool","version":"1.0.0","bin":{"z":"./z.js","tool":"bin/tool.js"}}"#,
        )
        .unwrap();
        assert_eq!(manifest.bin().unwrap().command_names(), &["tool", "z"]);
        assert_eq!(
            manifest.bin().unwrap().targets()[0].target,
            std::path::Path::new("bin/tool.js")
        );

        let manifest =
            PackageManifest::parse(r#"{"name":"tool","version":"1.0.0","bin":"./cli.js"}"#)
                .unwrap();
        assert_eq!(manifest.bin().unwrap().command_names(), &["tool"]);
    }

    #[test]
    fn accepts_explicit_registry_prefixes_in_dependency_keys() {
        let manifest = PackageManifest::parse(
            r#"{"name":"app","version":"1.0.0","dependencies":{"jsr:@std/path":"^1.0.0","npm:foo":"^1.0.0"}}"#,
        )
        .unwrap();

        assert_eq!(manifest.dependencies()["jsr:@std/path"], "^1.0.0");
        assert_eq!(manifest.dependencies()["npm:foo"], "^1.0.0");
    }

    #[test]
    fn rejects_malformed_bin_values_commands_and_targets() {
        for bin in ["null", "[]", "true", "{}"] {
            assert!(
                PackageManifest::parse(&format!(
                    r#"{{"name":"tool","version":"1.0.0","bin":{bin}}}"#
                ))
                .is_err()
            );
        }
        for bin in [
            r#"{"tool":"../escape.js"}"#,
            r#"{"tool":"/absolute.js"}"#,
            r#"{"tool":""}"#,
            r#"{"tool":123}"#,
            r#"{"tool/name":"cli.js"}"#,
        ] {
            assert!(
                PackageManifest::parse(&format!(
                    r#"{{"name":"tool","version":"1.0.0","bin":{bin}}}"#
                ))
                .is_err()
            );
        }
    }

    proptest! {
        #[test]
        fn dependency_kind_mutation_keeps_one_source_identity_in_one_section(
            kind_indices in prop::collection::vec(0usize..4, 1..8),
            requirement in "[[:ascii:]]{0,32}",
            source_prefix in prop::sample::select(vec!["", "npm:", "jsr:"]),
        ) {
            let dependency = format!("{source_prefix}tapid-property");
            let mut manifest = PackageManifest::new("example-app", "1.0.0", false).unwrap();
            for kind_index in kind_indices {
                let kind = match kind_index {
                    0 => DependencyKind::Dependencies,
                    1 => DependencyKind::DevDependencies,
                    2 => DependencyKind::OptionalDependencies,
                    _ => DependencyKind::PeerDependencies,
                };
                manifest = manifest.with_dependency_kind(kind, &dependency, &requirement).unwrap();
            }
            let kind = manifest.dependency_kind(&dependency).unwrap();
            prop_assert_eq!(manifest.dependencies().contains_key(&dependency), kind == DependencyKind::Dependencies);
            prop_assert_eq!(manifest.dev_dependencies().contains_key(&dependency), kind == DependencyKind::DevDependencies);
            prop_assert_eq!(manifest.optional_dependencies().contains_key(&dependency), kind == DependencyKind::OptionalDependencies);
            prop_assert_eq!(manifest.peer_dependencies().contains_key(&dependency), kind == DependencyKind::PeerDependencies);
        }
    }
}
