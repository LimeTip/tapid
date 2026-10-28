use crate::application::outcome::{ErrorKind, OperationalError};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::Path,
};
use tapid_core::{PackageName, RegistryOrigin};

const NPM: &str = "https://registry.npmjs.org";
const JSR: &str = "https://jsr.io";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

fn user_config_path(
    platform: &str,
    mut read_env: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> Result<Option<std::path::PathBuf>, String> {
    let candidates: &[(&str, &str)] = match platform {
        "linux" => &[
            ("XDG_CONFIG_HOME", "tapid/config.toml"),
            ("HOME", ".config/tapid/config.toml"),
        ],
        "macos" => &[("HOME", "Library/Application Support/tapid/config.toml")],
        "windows" => &[("APPDATA", "tapid/config.toml")],
        _ => return Err("unsupported user configuration platform".into()),
    };
    for (variable, suffix) in candidates {
        let Some(value) = read_env(variable).filter(|value| !value.is_empty()) else {
            continue;
        };
        let path = std::path::PathBuf::from(value);
        if !path.is_absolute() {
            return Err("user configuration location must be absolute".into());
        }
        return Ok(Some(path.join(suffix)));
    }
    Ok(None)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    #[serde(default)]
    network: NetworkDocument,
    #[serde(default)]
    registries: BTreeMap<String, RegistryEntry>,
    #[serde(default, rename = "run")]
    _run: Option<toml::Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkDocument {
    proxy: Option<String>,
    #[serde(rename = "ca-files")]
    ca_files: Option<Vec<std::path::PathBuf>>,
}

#[derive(Clone, Debug, Default)]
struct NetworkConfig {
    proxy: Option<String>,
    ca_files: Vec<std::path::PathBuf>,
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
    pub(crate) policy: String,
}

#[derive(Clone, Default)]
pub(crate) struct RegistryConfig {
    network: NetworkConfig,
    network_snapshot:
        std::sync::Arc<std::sync::OnceLock<Result<tapid_registry_client::NetworkOptions, String>>>,
    network_provenance: BTreeMap<&'static str, (&'static str, String)>,
    routes: BTreeMap<String, RegistryEntry>,
    route_layers: BTreeMap<String, &'static str>,
    route_sources: BTreeMap<String, String>,
    credential_names: BTreeSet<String>,
    layer_availability: BTreeMap<&'static str, (&'static str, Option<String>)>,
}

impl std::fmt::Debug for RegistryConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistryConfig")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default)]
pub(crate) struct ConfigSelection {
    user: Option<std::path::PathBuf>,
    explicit: Option<std::path::PathBuf>,
}
impl ConfigSelection {
    pub(crate) fn discover(explicit: Option<&Path>) -> Result<Self, String> {
        let cwd = env::current_dir()
            .map_err(|_| "cannot select configuration working directory".to_owned())?;
        Self::selected(explicit, &cwd, env::consts::OS, |name| env::var_os(name))
    }
    fn selected(
        explicit: Option<&Path>,
        cwd: &Path,
        platform: &str,
        read_env: impl FnMut(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Self, String> {
        if !cwd.is_absolute() {
            return Err("configuration working directory must be absolute".into());
        }
        Ok(Self {
            user: user_config_path(platform, read_env)?,
            explicit: explicit.map(|path| cwd.join(path)),
        })
    }
    pub(crate) fn load_for_script(
        &self,
        project: &Path,
        bytes: &[u8],
    ) -> Result<RegistryConfig, String> {
        RegistryConfig::load_layers_with_project_bytes(
            self.user.as_deref(),
            Some(&project.join("tapid.toml")),
            self.explicit.as_deref(),
            Some(bytes),
        )
    }
    pub(crate) fn load(&self, project: Option<&Path>) -> Result<RegistryConfig, String> {
        let project = project.map(|path| path.join("tapid.toml"));
        RegistryConfig::load_layers(
            self.user.as_deref(),
            project.as_deref(),
            self.explicit.as_deref(),
        )
    }
}

impl RegistryConfig {
    #[cfg(test)]
    pub(crate) fn load_selected(
        project: Option<&Path>,
        explicit: Option<&Path>,
        cwd: &Path,
        platform: &str,
        read_env: impl FnMut(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Self, String> {
        ConfigSelection::selected(explicit, cwd, platform, read_env)?.load(project)
    }
    /// Resolve selected filenames in increasing precedence, validating even shadowed
    /// documents. Location discovery belongs to the caller; this never reads tokens.
    pub(crate) fn load_layers(
        user: Option<&Path>,
        project: Option<&Path>,
        explicit: Option<&Path>,
    ) -> Result<Self, String> {
        Self::load_layers_with_project_bytes(user, project, explicit, None)
    }

    fn load_layers_with_project_bytes(
        user: Option<&Path>,
        project: Option<&Path>,
        explicit: Option<&Path>,
        project_bytes: Option<&[u8]>,
    ) -> Result<Self, String> {
        if project_bytes.is_some_and(|bytes| bytes.len() > MAX_CONFIG_BYTES as usize) {
            return Err("registry configuration exceeds 64 KiB".into());
        }
        let mut effective = Self::default();
        for (layer, path, optional) in [
            ("user", user, true),
            ("project", project, true),
            ("explicit", explicit, false),
        ] {
            let Some(path) = path else {
                effective.layer_availability.insert(
                    layer,
                    (
                        if layer == "project" {
                            "excluded"
                        } else {
                            "unavailable"
                        },
                        None,
                    ),
                );
                continue;
            };
            let input = if layer == "project" {
                project_bytes
                    .map(|bytes| Some(bytes.to_vec()))
                    .map(Ok)
                    .unwrap_or_else(|| Self::read_layer(path, optional))?
            } else {
                Self::read_layer(path, optional)?
            };
            let Some(input) = input else {
                effective
                    .layer_availability
                    .insert(layer, ("absent", Some(sanitized_path(path))));
                continue;
            };
            effective
                .layer_availability
                .insert(layer, ("loaded", Some(sanitized_path(path))));
            let input = std::str::from_utf8(&input)
                .map_err(|_| "registry configuration is not valid UTF-8".to_owned())?;
            let document: RegistryDocument =
                toml::from_str(input).map_err(|_| "invalid registry configuration".to_owned())?;
            if layer != "project" && document._run.is_some() {
                return Err("global registry configuration cannot contain run policy".into());
            }
            let network = document.network.clone();
            let config = Self::from_document(document)?;
            if let Some(proxy) = network.proxy {
                effective.network.proxy = (proxy != "direct").then_some(proxy);
                effective
                    .network_provenance
                    .insert("proxy", (layer, sanitized_path(path)));
            }
            if let Some(paths) = network.ca_files {
                let directory = path.parent().unwrap_or_else(|| Path::new("."));
                effective.network.ca_files =
                    paths.iter().map(|path| directory.join(path)).collect();
                effective
                    .network_provenance
                    .insert("ca-files", (layer, sanitized_path(path)));
            }
            effective.credential_names.extend(config.credential_names);
            for (scope, entry) in config.routes {
                effective.route_layers.insert(scope.clone(), layer);
                effective
                    .route_sources
                    .insert(scope.clone(), sanitized_path(path));
                effective.routes.insert(scope, entry);
            }
        }
        Ok(effective)
    }

    #[cfg(test)]
    pub(crate) fn load(project: &std::path::Path) -> Result<Self, String> {
        Self::load_layers(None, Some(&project.join("tapid.toml")), None)
    }

    fn read_layer(path: &Path, optional: bool) -> Result<Option<Vec<u8>>, String> {
        let input =
            match crate::filesystem::read::read_bounded_file(path, MAX_CONFIG_BYTES as usize) {
                Ok(bytes) => bytes,
                Err(crate::filesystem::read::ReadError::Io(error))
                    if optional && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Ok(None);
                }
                Err(crate::filesystem::read::ReadError::CapacityExceeded) => {
                    return Err(
                        "registry configuration must be a regular file no larger than 64 KiB"
                            .into(),
                    );
                }
                Err(crate::filesystem::read::ReadError::Io(_)) => {
                    return Err("cannot read registry configuration".into());
                }
            };
        Ok(Some(input))
    }

    #[cfg(test)]
    pub(crate) fn parse_toml_bytes(input: &[u8]) -> Result<Self, String> {
        let input = std::str::from_utf8(input)
            .map_err(|_| "registry configuration is not valid UTF-8".to_owned())?;
        Self::parse(input)
    }

    #[cfg(test)]
    fn parse(input: &str) -> Result<Self, String> {
        let document: RegistryDocument =
            toml::from_str(input).map_err(|_| "invalid registry configuration".to_owned())?;
        Self::from_document(document)
    }

    fn from_document(document: RegistryDocument) -> Result<Self, String> {
        let proxy = document.network.proxy.filter(|proxy| proxy != "direct");
        tapid_registry_client::NetworkOptions::new(proxy.as_deref(), &[])
            .map_err(|_| "invalid registry network configuration".to_owned())?;
        let ca_files = document.network.ca_files.unwrap_or_default();
        if ca_files.len() > 16 || ca_files.iter().any(|path| path.as_os_str().is_empty()) {
            return Err("invalid registry CA file collection".into());
        }
        let mut routes = BTreeMap::new();
        for (scope, entry) in document.registries {
            if scope != "default" && !valid_scope(&scope) {
                return Err("invalid registry scope".into());
            }
            let origin: RegistryOrigin = entry
                .url
                .parse()
                .map_err(|_| "registry URL must be a canonical HTTPS origin".to_owned())?;
            if origin.to_string() != entry.url {
                return Err("registry URL must be canonical".into());
            }
            if entry
                .token_env
                .as_deref()
                .is_some_and(|name| !valid_env_name(name))
            {
                return Err("invalid credential environment variable name".into());
            }
            routes.insert(scope, entry);
        }
        let credential_names = routes
            .values()
            .filter_map(|entry| entry.token_env.clone())
            .collect();
        Ok(Self {
            network: NetworkConfig { proxy, ca_files },
            network_snapshot: Default::default(),
            network_provenance: BTreeMap::new(),
            routes,
            credential_names,
            layer_availability: BTreeMap::new(),
            route_layers: BTreeMap::new(),
            route_sources: BTreeMap::new(),
        })
    }

    /// Reads and validates additional trust once, only when online transport is needed.
    pub(crate) fn network_options(&self) -> Result<&tapid_registry_client::NetworkOptions, String> {
        self.network_snapshot
            .get_or_init(|| {
                let mut bundles = Vec::new();
                let mut total = 0;
                for path in &self.network.ca_files {
                    let bytes = crate::filesystem::read::read_bounded_file(path, 1024 * 1024)
                        .map_err(|_| "cannot read registry CA file".to_owned())?;
                    total += bytes.len();
                    if total > 4 * 1024 * 1024 {
                        return Err("registry CA input limit exceeded".into());
                    }
                    bundles.push(bytes);
                }
                tapid_registry_client::NetworkOptions::new(self.network.proxy.as_deref(), &bundles)
                    .map_err(|_| "invalid registry CA configuration".to_owned())
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    fn selected_entry<'a>(&'a self, name: &PackageName) -> (Option<&'a RegistryEntry>, &'a str) {
        if let Some((scope, _)) = name.as_str().split_once('/')
            && let Some((policy, entry)) = self.routes.get_key_value(scope)
        {
            return (Some(entry), policy);
        }
        match self.routes.get("default") {
            Some(entry) => (Some(entry), "default"),
            None => (None, "public-fallback"),
        }
    }

    pub(crate) fn origin_for(&self, package: &str) -> Result<RegistryOrigin, String> {
        let name: PackageName = package
            .parse()
            .map_err(|_| "invalid package name".to_owned())?;
        self.origin_for_name(&name)
    }

    pub(crate) fn origin_for_name(&self, name: &PackageName) -> Result<RegistryOrigin, String> {
        self.selected_entry(name)
            .0
            .map(|entry| entry.url.as_str())
            .unwrap_or(NPM)
            .parse()
            .map_err(|_| "invalid registry origin".to_owned())
    }

    pub(crate) fn registry_for_dependency(
        &self,
        parent: &tapid_core::PackageSource,
        dependency: &PackageName,
    ) -> Result<tapid_core::PackageSource, String> {
        if parent.to_string() == JSR {
            Ok(parent.clone())
        } else {
            self.origin_for_name(dependency).map(Into::into)
        }
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

    pub(crate) fn route(&self, package: &str) -> Result<RegistryRoute, OperationalError> {
        self.route_with_env(package, |name| env::var(name).ok())
    }

    pub(crate) fn route_with_env(
        &self,
        package: &str,
        mut read_env: impl FnMut(&str) -> Option<String>,
    ) -> Result<RegistryRoute, OperationalError> {
        let name: PackageName = package.parse().map_err(|_| {
            OperationalError::new(ErrorKind::RegistryConfiguration, "invalid package name")
        })?;
        let (entry, policy) = self.selected_entry(&name);
        let origin = entry
            .map(|entry| entry.url.as_str())
            .unwrap_or(NPM)
            .parse()
            .map_err(|_| {
                OperationalError::new(ErrorKind::RegistryConfiguration, "invalid registry origin")
            })?;
        let token = if let Some(variable) = entry.and_then(|entry| entry.token_env.as_deref()) {
            let value = read_env(variable)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    OperationalError::new(ErrorKind::RegistryCredentialMissing, format!("required registry credential environment variable {variable} is missing or empty"))
                })?;
            Some(value)
        } else {
            None
        };
        Ok(RegistryRoute {
            origin,
            token,
            policy: policy.to_owned(),
        })
    }

    /// Credential-free configuration for local project inspection. Never reads the environment.
    /// Origins and token variable names describe configuration only; no credential
    /// values, network reachability, or authentication success are established.
    pub(crate) fn inspection(&self) -> serde_json::Value {
        let mut provenance = self
            .network_provenance
            .iter()
            .map(|(setting, (layer, source))| {
                (
                    (*setting).to_owned(),
                    serde_json::json!({"layer": layer, "source": source}),
                )
            })
            .collect::<serde_json::Map<String, serde_json::Value>>();
        for setting in ["proxy", "ca-files"] {
            provenance
                .entry(setting)
                .or_insert_with(|| serde_json::json!({"layer":"builtin", "source":null}));
        }
        let mut layers =
            vec![serde_json::json!({"layer":"builtin", "state":"loaded", "source":null})];
        layers.extend(["user", "project", "explicit"].into_iter().filter_map(|layer| self.layer_availability.get(layer).map(|(state, source)| serde_json::json!({"layer":layer, "state":state, "source":source}))));
        let routes = self.routes.iter().map(|(scope, entry)| serde_json::json!({
            "scope": scope, "origin": entry.url, "credential_environment_name": entry.token_env,
            "layer": self.route_layers.get(scope).copied().unwrap_or("project"),
            "source": self.route_sources.get(scope)
        })).collect::<Vec<_>>();
        serde_json::json!({
            "layers": layers,
            "bounds": {"config_bytes":MAX_CONFIG_BYTES, "ca_files":16, "ca_file_bytes":1048576, "ca_total_bytes":4194304, "ca_certificates":128, "request_timeout_seconds":20, "redirect_hops":10, "get_attempts":3, "retry_delays_ms":[100,200], "retry_after_seconds":60, "metadata_bytes":33554432, "artifact_bytes":536870912},
            "npm_configuration":"ignored",
            "unsupported":["proxy_authentication", "https_proxy", "pac", "socks", "client_certificates", "insecure_tls", "credential_helpers"],
            "network": {
                "proxy": self.network.proxy.as_deref().unwrap_or("direct"),
                "ca_files": self.network.ca_files.iter().map(|path| sanitized_path(path)).collect::<Vec<_>>(),
                "provenance": provenance,
                "ambient_proxy": "ignored",
                "ca_contents": "not_read"
            },
            "origins": self.configured_origins(),
            "routes": routes,
            "default_origin": self.routes.get("default").map_or(NPM, |entry| entry.url.as_str()),
            "jsr_origin": JSR,
            "credential_environment_names": self.credential_environment_names(),
            "credentials": "not_read"
        })
    }

    /// Return configured origins in route order, retaining repeated origins.
    pub(crate) fn configured_origins(&self) -> Vec<String> {
        self.routes
            .values()
            .map(|entry| entry.url.clone())
            .collect()
    }

    #[cfg(test)]
    fn credentials_for_with_env(
        &self,
        packages: &[String],
        mut read_env: impl FnMut(&str) -> Option<String>,
    ) -> Result<Vec<(String, String)>, String> {
        let mut credentials = BTreeMap::new();
        for package in packages {
            let route = self
                .route_with_env(package, &mut read_env)
                .map_err(|error| error.to_string())?;
            if let Some(token) = route.token {
                insert_credential(&mut credentials, route.origin.to_string(), token)?;
            }
        }
        Ok(credentials.into_iter().collect())
    }

    /// Return configured token variable names without reading their values.
    pub(crate) fn credential_environment_names(&self) -> Vec<String> {
        self.credential_names.iter().cloned().collect()
    }

    pub(crate) fn allowlist_contains_registry_credentials(
        &self,
        names: &[String],
        case_insensitive: bool,
    ) -> bool {
        self.credential_names.iter().any(|credential_name| {
            names.iter().any(|name| {
                if case_insensitive {
                    name.eq_ignore_ascii_case(credential_name)
                } else {
                    name == credential_name
                }
            })
        })
    }

    #[cfg(test)]
    pub(crate) fn from_toml(input: &str) -> Result<Self, String> {
        Self::parse(input)
    }
}

#[cfg(test)]
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

fn sanitized_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(|character| {
            if character.is_control() {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
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
    fn effective_inspection_reports_layer_availability_and_fixed_bounds() {
        let project = tapid_test_support::TempProject::new("configuration-layers").unwrap();
        let explicit = project
            .write("explicit.toml", b"[network]\nproxy='direct'\n")
            .unwrap();
        let config = ConfigSelection {
            user: Some(project.path().join("missing.toml")),
            explicit: Some(explicit),
        }
        .load(None)
        .unwrap();
        let inspection = config.inspection();
        let layers = inspection["layers"].as_array().unwrap();
        for (name, state) in [
            ("builtin", "loaded"),
            ("user", "absent"),
            ("project", "excluded"),
            ("explicit", "loaded"),
        ] {
            assert!(
                layers
                    .iter()
                    .any(|layer| layer["layer"] == name && layer["state"] == state),
                "{inspection}"
            );
        }
        assert_eq!(
            inspection["network"]["provenance"]["ca-files"]["layer"],
            "builtin"
        );
        assert_eq!(inspection["bounds"]["get_attempts"], 3);
        assert_eq!(inspection["bounds"]["ca_files"], 16);
        assert_eq!(inspection["npm_configuration"], "ignored");
    }

    #[test]
    fn script_credentials_use_the_same_project_bytes_as_run_policy() {
        let project = tapid_test_support::TempProject::new("script-config-snapshot").unwrap();
        let original =
            b"[registries.default]\nurl='https://registry.example'\ntoken-env='ORIGINAL_SECRET'\n";
        project
            .write(
                "tapid.toml",
                b"[registries.default]\nurl='https://replacement.example'\n",
            )
            .unwrap();
        let config = ConfigSelection::default()
            .load_for_script(project.path(), original)
            .unwrap();
        assert_eq!(config.credential_environment_names(), ["ORIGINAL_SECRET"]);
        assert!(config.allowlist_contains_registry_credentials(&["original_secret".into()], true));
        assert!(
            !config.allowlist_contains_registry_credentials(&["original_secret".into()], false)
        );
    }

    #[test]
    fn ca_snapshot_is_lazy_bounded_and_stable() {
        let home = tapid_test_support::TempProject::new("registry-ca-snapshot").unwrap();
        let document = home
            .write("tapid.toml", b"[network]\nca-files = ['root.pem']\n")
            .unwrap();
        let config = RegistryConfig::load_layers(None, Some(&document), None).unwrap();
        let cloned = config.clone();
        let ca = include_bytes!("../../tapid-registry-client/tests/fixtures/local-test-ca.pem");
        let root = home.write("root.pem", ca).unwrap();
        assert!(config.network_options().is_ok());
        std::fs::remove_file(&root).unwrap();
        assert!(config.network_options().is_ok());
        assert!(cloned.network_options().is_ok());
        let fresh = RegistryConfig::load_layers(None, Some(&document), None).unwrap();
        assert_eq!(
            fresh.network_options().err().unwrap(),
            "cannot read registry CA file"
        );
        home.write("root.pem", &vec![b'x'; 1024 * 1024 + 1])
            .unwrap();
        let oversized = RegistryConfig::load_layers(None, Some(&document), None).unwrap();
        assert_eq!(
            oversized.network_options().err().unwrap(),
            "cannot read registry CA file"
        );
    }

    #[test]
    fn network_fields_inherit_clear_and_resolve_declaring_paths() {
        let home = tapid_test_support::TempProject::new("registry-network-layers").unwrap();
        let user = home
            .write(
                "user.toml",
                b"[network]\nproxy = 'http://proxy.example:8080'\nca-files = ['certs/root.pem']\n",
            )
            .unwrap();
        let project = home
            .write("project.toml", b"[network]\nproxy = 'direct'\n")
            .unwrap();
        let explicit = home
            .write("explicit.toml", b"[network]\nca-files = []\n")
            .unwrap();
        let inherited = RegistryConfig::load_layers(Some(&user), None, None).unwrap();
        assert_eq!(
            inherited.network.proxy.as_deref(),
            Some("http://proxy.example:8080")
        );
        assert_eq!(
            inherited.network.ca_files,
            vec![home.path().join("certs/root.pem")]
        );
        let direct = RegistryConfig::load_layers(Some(&user), Some(&project), None).unwrap();
        assert!(direct.network.proxy.is_none());
        assert_eq!(direct.network.ca_files, inherited.network.ca_files);
        let cleared =
            RegistryConfig::load_layers(Some(&user), Some(&project), Some(&explicit)).unwrap();
        assert!(cleared.network.proxy.is_none());
        assert!(cleared.network.ca_files.is_empty());
    }

    #[test]
    fn network_inspection_reports_winners_without_opening_ca_files() {
        let home = tapid_test_support::TempProject::new("registry-network-inspection").unwrap();
        let user = home
            .write(
                "user.toml",
                b"[network]\nproxy = 'http://proxy.example:8080'\nca-files = ['missing.pem']\n",
            )
            .unwrap();
        let explicit = home
            .write("explicit.toml", b"[network]\nproxy = 'direct'\n")
            .unwrap();
        let config = RegistryConfig::load_layers(Some(&user), None, Some(&explicit)).unwrap();
        let value = config.inspection();
        assert_eq!(value["network"]["proxy"], "direct");
        assert_eq!(value["network"]["provenance"]["proxy"]["layer"], "explicit");
        assert_eq!(value["network"]["provenance"]["ca-files"]["layer"], "user");
        assert_eq!(
            value["network"]["ca_files"][0],
            sanitized_path(&home.path().join("missing.pem"))
        );
        assert_eq!(value["credentials"], "not_read");
    }

    #[test]
    fn shadowed_network_configuration_is_strict_and_redacted() {
        let home = tapid_test_support::TempProject::new("registry-hostile-network").unwrap();
        let explicit = home
            .write(
                "explicit.toml",
                b"[network]\nproxy = 'direct'\nca-files = []\n",
            )
            .unwrap();
        for input in [
            "[network]\nproxy = 'http://secret-sentinel@proxy.example:8080'\n",
            "[network]\nsecret-sentinel = true\n",
            "[network]\nca-files = ['']\n",
            "[network]\nproxy = 'https://secret-sentinel.example:8080'\n",
        ] {
            let user = home.write("user.toml", input.as_bytes()).unwrap();
            let error =
                RegistryConfig::load_layers(Some(&user), None, Some(&explicit)).unwrap_err();
            assert!(!error.contains("secret-sentinel"));
        }
        let input = format!(
            "[network]\nca-files = [{}]\n",
            vec!["'root.pem'"; 17].join(",")
        );
        let user = home.write("user.toml", input.as_bytes()).unwrap();
        assert!(RegistryConfig::load_layers(Some(&user), None, Some(&explicit)).is_err());
    }
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    #[test]
    fn user_locations_use_platform_contract_and_linux_fallback() {
        let home = tapid_test_support::TempProject::new("registry-location").unwrap();
        for (platform, variable, suffix) in [
            ("linux", "XDG_CONFIG_HOME", "tapid/config.toml"),
            ("linux", "HOME", ".config/tapid/config.toml"),
            (
                "macos",
                "HOME",
                "Library/Application Support/tapid/config.toml",
            ),
            ("windows", "APPDATA", "tapid/config.toml"),
        ] {
            let actual = user_config_path(platform, |name| {
                if name == variable {
                    Some(home.path().as_os_str().to_owned())
                } else if name == "XDG_CONFIG_HOME" {
                    Some("".into())
                } else {
                    None
                }
            })
            .unwrap();
            assert_eq!(actual, Some(home.path().join(suffix)));
        }
        assert_eq!(user_config_path("linux", |_| None).unwrap(), None);
        let error =
            user_config_path("linux", |_| Some("relative-secret-sentinel".into())).unwrap_err();
        assert!(!error.contains("secret-sentinel"));
    }

    #[test]
    fn selected_configuration_resolves_relative_explicit_and_sanitizes_provenance() {
        let home = tapid_test_support::TempProject::new("registry-selected-location").unwrap();
        home.write(
            ".config/tapid/config.toml",
            b"[registries.default]\nurl='https://user.example'\n",
        )
        .unwrap();
        home.write(
            "explicit.toml",
            b"[registries.default]\nurl='https://explicit.example'\n",
        )
        .unwrap();
        let config = RegistryConfig::load_selected(
            None,
            Some(Path::new("explicit.toml")),
            home.path(),
            "linux",
            |name| (name == "HOME").then(|| home.path().as_os_str().to_owned()),
        )
        .unwrap();
        assert_eq!(
            config.inspection()["routes"][0]["source"],
            home.path().join("explicit.toml").to_string_lossy().as_ref()
        );
        assert_eq!(
            config.origin_for("widget").unwrap().to_string(),
            "https://explicit.example"
        );
        #[cfg(unix)]
        {
            let path = home
                .write(
                    "control\u{1b}[31m.toml",
                    b"[registries.default]\nurl='https://safe.example'\n",
                )
                .unwrap();
            let config = RegistryConfig::load_layers(None, None, Some(&path)).unwrap();
            let source = config.inspection()["routes"][0]["source"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(!source.chars().any(char::is_control));
            assert!(source.contains("\\u{1b}"));
        }
    }

    #[test]
    fn layered_configuration_validates_shadowed_input_without_disclosing_it() {
        let project = tapid_test_support::TempProject::new("registry-hostile-layer").unwrap();
        let user = project
            .write(
                "user.toml",
                b"[registries.'secret-sentinel']\nurl='https://user.example'\n",
            )
            .unwrap();
        let explicit = project
            .write(
                "explicit.toml",
                b"[registries.default]\nurl='https://explicit.example'\n",
            )
            .unwrap();
        let error = RegistryConfig::load_layers(Some(&user), None, Some(&explicit)).unwrap_err();
        assert!(!error.contains("secret-sentinel"));
    }

    #[test]
    fn layered_configuration_preserves_scope_isolation_and_optional_absence() {
        let project = tapid_test_support::TempProject::new("registry-scopes").unwrap();
        let user = project.write("user.toml", b"[registries.default]\nurl='https://user.example'\ntoken-env='USER_TOKEN'\n[registries.'@acme']\nurl='https://scope.example'\n").unwrap();
        let selected = project
            .write(
                "tapid.toml",
                b"[registries.default]\nurl='https://project.example'\n",
            )
            .unwrap();
        let config = RegistryConfig::load_layers(Some(&user), Some(&selected), None).unwrap();
        assert_eq!(
            config
                .route_with_env("@acme/widget", |_| panic!("no inherited credential"))
                .unwrap()
                .origin
                .to_string(),
            "https://scope.example"
        );
        assert_eq!(
            config.origin_for("widget").unwrap().to_string(),
            "https://project.example"
        );
        assert_eq!(config.inspection()["routes"][0]["layer"], "user");
        let missing = project.path().join("absent.toml");
        let defaults = RegistryConfig::load_layers(Some(&missing), Some(&missing), None).unwrap();
        assert_eq!(defaults.origin_for("widget").unwrap().to_string(), NPM);
        assert!(RegistryConfig::load_layers(None, None, Some(&missing)).is_err());
    }

    #[test]
    fn global_configuration_rejects_execution_authority_and_unknown_fields() {
        let project = tapid_test_support::TempProject::new("registry-authority").unwrap();
        for input in [
            b"[run.defaults]\nnetwork=true\n".as_slice(),
            b"[unknown]\nenabled=true\n",
        ] {
            let path = project.write("global.toml", input).unwrap();
            assert!(RegistryConfig::load_layers(Some(&path), None, None).is_err());
            assert!(RegistryConfig::load_layers(None, None, Some(&path)).is_err());
        }
        let path = project
            .write("tapid.toml", b"[run.defaults]\nnetwork=false\n")
            .unwrap();
        assert!(RegistryConfig::load_layers(None, Some(&path), None).is_ok());
    }

    #[test]
    fn layered_registry_replacement_drops_lower_token() {
        let project = tapid_test_support::TempProject::new("registry-layers").unwrap();
        let user = project
            .write(
                "user.toml",
                b"[registries.default]\nurl='https://user.example'\ntoken-env='USER_TOKEN'\n",
            )
            .unwrap();
        let selected = project
            .write(
                "tapid.toml",
                b"[registries.default]\nurl='https://project.example'\ntoken-env='PROJECT_TOKEN'\n",
            )
            .unwrap();
        let explicit = project
            .write(
                "explicit.toml",
                b"[registries.default]\nurl='https://explicit.example'\n",
            )
            .unwrap();
        let config =
            RegistryConfig::load_layers(Some(&user), Some(&selected), Some(&explicit)).unwrap();
        let route = config
            .route_with_env("widget", |_| panic!("replacement must remove credentials"))
            .unwrap();
        assert_eq!(route.origin.to_string(), "https://explicit.example");
        assert!(route.token.is_none());
        assert_eq!(
            config.credential_environment_names(),
            vec!["PROJECT_TOKEN", "USER_TOKEN"]
        );
        assert!(config.allowlist_contains_registry_credentials(&["user_token".into()], true));
        assert_eq!(config.inspection()["routes"][0]["layer"], "explicit");
    }
    static NEXT_TEST_PROJECT: AtomicU64 = AtomicU64::new(0);

    struct TestProject(PathBuf);

    impl TestProject {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "tapid-registry-config-{}-{}",
                std::process::id(),
                NEXT_TEST_PROJECT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn run_and_registry_policies_parse_from_the_same_configuration_snapshot() {
        let input = br#"[registries.'@acme']
url='https://packages.example'
token-env='ACME_TOKEN'
[run.scripts.dev]
environment=['ACME_TOKEN']
"#;
        let run = tapid_runner::RunConfig::parse_toml_bytes(input).unwrap();
        let registry = RegistryConfig::parse_toml_bytes(input).unwrap();
        assert!(registry.allowlist_contains_registry_credentials(
            run.exact_profile("dev").unwrap().environment(),
            false,
        ));
        let route = registry
            .route_with_env("@acme/widget", |name| {
                assert_eq!(name, "ACME_TOKEN");
                Some("fixture-value".to_owned())
            })
            .unwrap();
        assert_eq!(route.origin.to_string(), "https://packages.example");
        assert_eq!(route.token.as_deref(), Some("fixture-value"));
    }

    #[test]
    fn load_accepts_a_configuration_exactly_at_the_byte_limit() {
        let project = TestProject::new();
        fs::write(
            project.0.join("tapid.toml"),
            vec![b'#'; MAX_CONFIG_BYTES as usize],
        )
        .unwrap();

        let config = RegistryConfig::load(&project.0).unwrap();

        assert_eq!(config.origin_for("left-pad").unwrap().to_string(), NPM);
    }

    #[test]
    fn load_rejects_configuration_over_the_byte_limit() {
        let project = TestProject::new();
        fs::write(
            project.0.join("tapid.toml"),
            vec![b'#'; MAX_CONFIG_BYTES as usize + 1],
        )
        .unwrap();

        let error = RegistryConfig::load(&project.0).unwrap_err();

        assert!(error.contains("64 KiB"));
    }

    #[test]
    fn missing_configuration_uses_public_registry_defaults() {
        let project = TestProject::new();

        let config = RegistryConfig::load(&project.0).unwrap();

        assert_eq!(config.origin_for("left-pad").unwrap().to_string(), NPM);
    }

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
        assert_eq!(missing.kind, ErrorKind::RegistryCredentialMissing);
        assert!(missing.to_string().contains("TAPID_ACME_TOKEN"));
        assert!(!missing.to_string().contains("secret"));
        let empty = config
            .route_with_env("@acme/widget", |_| Some(String::new()))
            .err()
            .unwrap();
        assert_eq!(empty.kind, ErrorKind::RegistryCredentialMissing);
        assert!(empty.to_string().contains("empty"));
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
