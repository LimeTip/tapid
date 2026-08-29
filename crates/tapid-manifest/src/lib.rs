//! Parsing and validation for npm-compatible `package.json` manifests.

#![deny(unsafe_code)]

mod error;
mod model;
mod parse;

pub use error::ManifestError;
pub use model::{BinTarget, PackageBin, PackageManifest};

/// Returns the current crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

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
}
