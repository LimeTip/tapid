use serde::Deserialize;
use std::{collections::BTreeMap, env};
use tapid_core::{PackageName, RegistryOrigin};

const NPM: &str = "https://registry.npmjs.org";
const JSR: &str = "https://jsr.io";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    #[serde(default)]
    registries: BTreeMap<String, RegistryEntry>,
    #[serde(default, rename = "run")]
    _run: Option<toml::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryEntry {
    url: String,
    #[serde(default, rename = "token-env")]
    token_env: Option<String>,
}

#[derive(Clone)]
pub(crate) struct RegistryRoute {
    pub(crate) origin: RegistryOrigin,
    pub(crate) token: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RegistryConfig {
    routes: BTreeMap<String, RegistryEntry>,
}

impl RegistryConfig {
    pub(crate) fn load(project: &std::path::Path) -> Result<Self, String> {
        let path = project.join("tapid.toml");
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("cannot inspect registry configuration: {error}")),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_CONFIG_BYTES {
            return Err(
                "registry configuration must be a regular file no larger than 64 KiB".into(),
            );
        }
        let input = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read registry configuration: {error}"))?;
        Self::parse(&input)
    }

    fn parse(input: &str) -> Result<Self, String> {
        let document: RegistryDocument =
            toml::from_str(input).map_err(|_| "invalid registry configuration".to_owned())?;
        let mut routes = BTreeMap::new();
        for (scope, entry) in document.registries {
            if scope != "default" && !valid_scope(&scope) {
                return Err(format!("invalid registry scope {scope:?}"));
            }
            let origin: RegistryOrigin = entry.url.parse().map_err(|_| {
                format!("registry URL for {scope:?} must be a canonical HTTPS origin")
            })?;
            if origin.to_string() != entry.url {
                return Err(format!("registry URL for {scope:?} must be canonical"));
            }
            if entry
                .token_env
                .as_deref()
                .is_some_and(|name| !valid_env_name(name))
            {
                return Err(format!(
                    "invalid credential environment variable name for {scope:?}"
                ));
            }
            routes.insert(scope, entry);
        }
        Ok(Self { routes })
    }

    fn selected_entry(&self, package: &str) -> Result<Option<&RegistryEntry>, String> {
        let name: PackageName = package
            .parse()
            .map_err(|_| "invalid package name".to_owned())?;
        let key = name
            .as_str()
            .split_once('/')
            .and_then(|(scope, _)| scope.starts_with('@').then_some(scope));
        Ok(key
            .and_then(|scope| self.routes.get(scope))
            .or_else(|| self.routes.get("default")))
    }

    pub(crate) fn origin_for(&self, package: &str) -> Result<RegistryOrigin, String> {
        self.selected_entry(package)?
            .map(|entry| entry.url.as_str())
            .unwrap_or(NPM)
            .parse()
            .map_err(|_| "invalid registry origin".to_owned())
    }

    pub(crate) fn identity_for_spec(
        &self,
        spec: &str,
    ) -> Result<(RegistryOrigin, PackageName), String> {
        if let Some(raw) = spec.strip_prefix("jsr:") {
            let package = raw
                .parse()
                .map_err(|error: tapid_core::DomainError| error.to_string())?;
            let origin = JSR
                .parse()
                .map_err(|error: tapid_core::DomainError| error.to_string())?;
            return Ok((origin, package));
        }
        let raw = spec.strip_prefix("npm:").unwrap_or(spec);
        let package: PackageName = raw
            .parse()
            .map_err(|error: tapid_core::DomainError| error.to_string())?;
        let origin = self.origin_for(package.as_str())?;
        Ok((origin, package))
    }

    pub(crate) fn route(&self, package: &str) -> Result<RegistryRoute, String> {
        self.route_with_env(package, |name| env::var(name).ok())
    }

    fn route_with_env(
        &self,
        package: &str,
        mut read_env: impl FnMut(&str) -> Option<String>,
    ) -> Result<RegistryRoute, String> {
        let entry = self.selected_entry(package)?;
        let origin = self.origin_for(package)?;
        let token = if let Some(variable) = entry.and_then(|entry| entry.token_env.as_deref()) {
            let value = read_env(variable)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    format!(
                        "required registry credential environment variable {variable} is missing or empty"
                    )
                })?;
            Some(value)
        } else {
            None
        };
        Ok(RegistryRoute { origin, token })
    }

    pub(crate) fn configured_origins(&self) -> Vec<String> {
        self.routes
            .values()
            .map(|entry| entry.url.clone())
            .collect()
    }

    pub(crate) fn credentials_for(
        &self,
        packages: &[String],
    ) -> Result<Vec<(String, String)>, String> {
        self.credentials_for_with_env(packages, |name| env::var(name).ok())
    }

    fn credentials_for_with_env(
        &self,
        packages: &[String],
        mut read_env: impl FnMut(&str) -> Option<String>,
    ) -> Result<Vec<(String, String)>, String> {
        let mut credentials = BTreeMap::new();
        for package in packages {
            let route = self.route_with_env(package, &mut read_env)?;
            if let Some(token) = route.token {
                insert_credential(&mut credentials, route.origin.to_string(), token)?;
            }
        }
        Ok(credentials.into_iter().collect())
    }

    pub(crate) fn credential_environment_names(&self) -> Vec<String> {
        self.routes
            .values()
            .filter_map(|entry| entry.token_env.clone())
            .collect()
    }

    pub(crate) fn allowlist_contains_registry_credentials(
        &self,
        names: &[String],
        case_insensitive: bool,
    ) -> bool {
        self.routes.values().any(|entry| {
            entry.token_env.as_ref().is_some_and(|credential_name| {
                names.iter().any(|name| {
                    if case_insensitive {
                        name.eq_ignore_ascii_case(credential_name)
                    } else {
                        name == credential_name
                    }
                })
            })
        })
    }

    #[cfg(test)]
    pub(crate) fn from_toml(input: &str) -> Result<Self, String> {
        Self::parse(input)
    }
}

fn insert_credential(
    credentials: &mut BTreeMap<String, String>,
    origin: String,
    token: String,
) -> Result<(), String> {
    if credentials
        .get(&origin)
        .is_some_and(|existing| existing != &token)
    {
        return Err("conflicting credentials configured for registry origin".into());
    }
    credentials.insert(origin, token);
    Ok(())
}

fn valid_scope(value: &str) -> bool {
    let Some(scope) = value.strip_prefix('@') else {
        return false;
    };
    !scope.is_empty()
        && scope
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
}

fn valid_env_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_registry_overrides_default_and_unmapped_packages_keep_public_npm() {
        let config = RegistryConfig::from_toml(
            "[registries.default]\nurl='https://mirror.example'\n[registries.'@acme']\nurl='https://packages.example'\n",
        ).unwrap();
        assert_eq!(
            config.route("@acme/widget").unwrap().origin.to_string(),
            "https://packages.example"
        );
        assert_eq!(
            config.route("left-pad").unwrap().origin.to_string(),
            "https://mirror.example"
        );
        assert_eq!(
            RegistryConfig::default()
                .route("left-pad")
                .unwrap()
                .origin
                .to_string(),
            NPM
        );
    }

    #[test]
    fn credential_source_reports_missing_and_empty_values_without_disclosing_them() {
        let config = RegistryConfig::from_toml(
            "[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ACME_TOKEN'\n",
        )
        .unwrap();
        let missing = config
            .route_with_env("@acme/widget", |_| None)
            .err()
            .unwrap();
        assert!(missing.contains("TAPID_ACME_TOKEN"));
        assert!(!missing.contains("secret"));
        let empty = config
            .route_with_env("@acme/widget", |_| Some(String::new()))
            .err()
            .unwrap();
        assert!(empty.contains("empty"));
    }

    #[test]
    fn credential_value_is_returned_only_for_its_selected_scope() {
        let config = RegistryConfig::from_toml(
            "[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ACME_TOKEN'\n",
        )
        .unwrap();
        let private = config
            .route_with_env("@acme/widget", |_| Some("secret-value".to_owned()))
            .unwrap();
        assert_eq!(private.token.as_deref(), Some("secret-value"));
        let public = config
            .route_with_env("public-package", |_| {
                panic!("no credential should be requested")
            })
            .unwrap();
        assert!(public.token.is_none());
    }

    #[test]
    fn scoped_credentials_do_not_inherit_the_default_entry() {
        let config = RegistryConfig::from_toml(
            "[registries.default]\nurl='https://mirror.example'\ntoken-env='DEFAULT_TOKEN'\n[registries.'@acme']\nurl='https://packages.example'\n",
        )
        .unwrap();
        let private = config
            .route_with_env("@acme/widget", |_| panic!("scope has no credential source"))
            .unwrap();
        assert_eq!(private.origin.to_string(), "https://packages.example");
        assert!(private.token.is_none());
        let public = config
            .route_with_env("public-package", |name| {
                assert_eq!(name, "DEFAULT_TOKEN");
                Some("default-token".into())
            })
            .unwrap();
        assert_eq!(public.token.as_deref(), Some("default-token"));
    }

    #[test]
    fn run_configuration_is_accepted_but_unknown_top_level_tables_are_rejected() {
        assert!(RegistryConfig::from_toml("[run.defaults]\nnetwork=false\n").is_ok());
        assert!(
            RegistryConfig::from_toml("[registry.default]\nurl='https://mirror.example'\n")
                .is_err()
        );
    }

    #[test]
    fn conflicting_tokens_for_one_origin_fail_without_disclosure() {
        let config = RegistryConfig::from_toml(
            "[registries.'@acme']\nurl='https://shared.example'\ntoken-env='TOKEN_A'\n[registries.'@contoso']\nurl='https://shared.example'\ntoken-env='TOKEN_B'\n",
        )
        .unwrap();
        let error = config
            .credentials_for_with_env(&["@acme/a".into(), "@contoso/b".into()], |name| {
                Some(match name {
                    "TOKEN_A" => "private-value-a".to_owned(),
                    "TOKEN_B" => "private-value-b".to_owned(),
                    _ => unreachable!(),
                })
            })
            .err()
            .unwrap();
        assert!(error.contains("conflicting"));
        assert!(!error.contains("private-value"));
    }

    #[test]
    fn credentials_for_reads_only_the_selected_package_routes() {
        let config = RegistryConfig::from_toml(
            "[registries.'@direct']\nurl='https://direct.example'\ntoken-env='DIRECT_TOKEN'\n[registries.'@unrelated']\nurl='https://unrelated.example'\ntoken-env='UNRELATED_TOKEN'\n",
        )
        .unwrap();
        let credentials = config
            .credentials_for_with_env(&["@direct/root".into()], |name| match name {
                "DIRECT_TOKEN" => Some("direct-value".to_owned()),
                "UNRELATED_TOKEN" => panic!("unselected route credential must not be read"),
                _ => unreachable!(),
            })
            .unwrap();
        assert_eq!(
            credentials,
            vec![("https://direct.example".into(), "direct-value".into())]
        );
    }

    #[test]
    fn registry_credentials_cannot_be_allowlisted_for_child_processes() {
        let config = RegistryConfig::from_toml(
            "[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ACME_TOKEN'\n",
        )
        .unwrap();
        assert!(
            config.allowlist_contains_registry_credentials(&["TAPID_ACME_TOKEN".into()], false,)
        );
        assert!(
            config.allowlist_contains_registry_credentials(&["tapid_acme_token".into()], true,)
        );
        assert!(!config.allowlist_contains_registry_credentials(&["OTHER_VALUE".into()], false,));
    }

    #[test]
    fn rejects_noncanonical_origins_unknown_fields_and_bad_scope() {
        for input in [
            "[registries.default]\nurl='https://MIRROR.example'\n",
            "[registries.default]\nurl='https://mirror.example/path'\n",
            "[registries.'acme']\nurl='https://mirror.example'\n",
            "[registries.default]\nurl='https://mirror.example'\ntoken='secret'\n",
        ] {
            assert!(RegistryConfig::from_toml(input).is_err(), "{input}");
        }
    }
}
