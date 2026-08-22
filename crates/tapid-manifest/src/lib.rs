//! Parsing and validation for npm-compatible `package.json` manifests.

#![deny(unsafe_code)]

mod error;
mod model;
mod parse;

pub use error::ManifestError;
pub use model::PackageManifest;

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
}
