use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path};

/// Maximum accepted size of a checked-in TOML configuration document.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;
/// Maximum number of per-script profiles in one configuration.
pub const MAX_PROFILE_COUNT: usize = 256;
/// Maximum combined read and write grants in one profile.
pub const MAX_GRANT_COUNT: usize = 256;
/// Maximum environment variable names allowlisted by one profile.
pub const MAX_ENVIRONMENT_COUNT: usize = 256;
/// Maximum UTF-8 byte length of a profile name, grant, or environment name.
pub const MAX_STRING_BYTES: usize = 255;

/// Whether execution must use containment or was explicitly overridden by a caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxMode {
    /// Fail closed unless the requested policy can be enforced.
    Required,
    /// Explicit programmatic override. Checked-in configuration cannot select this mode.
    Disabled,
}

/// Project-relative filesystem grants for a sandboxed process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemPolicy {
    read: Vec<String>,
    write: Vec<String>,
}

impl FilesystemPolicy {
    pub fn new(read: Vec<String>, write: Vec<String>) -> Result<Self, ConfigError> {
        if read.len().saturating_add(write.len()) > MAX_GRANT_COUNT {
            return Err(ConfigError::new(
                ConfigErrorCategory::CapacityExceeded,
                format!("filesystem policy exceeds {MAX_GRANT_COUNT} grants"),
            ));
        }
        for path in read.iter().chain(&write) {
            validate_string_length("filesystem grant", path)?;
            validate_project_path(path)?;
        }
        Ok(Self { read, write })
    }

    pub fn read(&self) -> &[String] {
        &self.read
    }

    pub fn write(&self) -> &[String] {
        &self.write
    }
}

/// Optional, validated execution resource ceilings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutionLimits {
    timeout_seconds: Option<u64>,
    max_output_bytes: Option<u64>,
    max_processes: Option<u32>,
    max_memory_bytes: Option<u64>,
}

impl ExecutionLimits {
    pub fn new(
        timeout_seconds: Option<u64>,
        max_output_bytes: Option<u64>,
        max_processes: Option<u32>,
        max_memory_bytes: Option<u64>,
    ) -> Result<Self, ConfigError> {
        Ok(Self {
            timeout_seconds: nonzero("timeout_seconds", timeout_seconds)?,
            max_output_bytes: nonzero("max_output_bytes", max_output_bytes)?,
            max_processes: nonzero("max_processes", max_processes)?,
            max_memory_bytes: nonzero("max_memory_bytes", max_memory_bytes)?,
        })
    }

    pub fn timeout_seconds(&self) -> Option<u64> {
        self.timeout_seconds
    }
    pub fn max_output_bytes(&self) -> Option<u64> {
        self.max_output_bytes
    }
    pub fn max_processes(&self) -> Option<u32> {
        self.max_processes
    }
    pub fn max_memory_bytes(&self) -> Option<u64> {
        self.max_memory_bytes
    }
}

/// Complete declarative sandbox policy. This type does not imply enforcement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxPolicy {
    mode: SandboxMode,
    filesystem: FilesystemPolicy,
    network: bool,
    environment: Vec<String>,
    subprocess: bool,
    limits: ExecutionLimits,
}

impl SandboxPolicy {
    pub fn new(
        mode: SandboxMode,
        filesystem: FilesystemPolicy,
        network: bool,
        environment: Vec<String>,
        subprocess: bool,
        limits: ExecutionLimits,
    ) -> Result<Self, ConfigError> {
        if environment.len() > MAX_ENVIRONMENT_COUNT {
            return Err(ConfigError::new(
                ConfigErrorCategory::CapacityExceeded,
                format!("sandbox policy exceeds {MAX_ENVIRONMENT_COUNT} environment names"),
            ));
        }
        for name in &environment {
            validate_string_length("environment variable name", name)?;
            validate_environment_name(name)?;
        }
        Ok(Self {
            mode,
            filesystem,
            network,
            environment,
            subprocess,
            limits,
        })
    }

    pub fn mode(&self) -> SandboxMode {
        self.mode
    }
    pub fn filesystem(&self) -> &FilesystemPolicy {
        &self.filesystem
    }
    pub fn network(&self) -> bool {
        self.network
    }
    pub fn environment(&self) -> &[String] {
        &self.environment
    }
    pub fn subprocess(&self) -> bool {
        self.subprocess
    }
    pub fn limits(&self) -> &ExecutionLimits {
        &self.limits
    }
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            mode: SandboxMode::Required,
            filesystem: FilesystemPolicy {
                read: vec![".".to_owned()],
                write: Vec::new(),
            },
            network: false,
            environment: Vec::new(),
            // Node package scripts currently require a shell child.
            subprocess: true,
            limits: ExecutionLimits::default(),
        }
    }
}

/// Stable category for checked-in run configuration failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigErrorCategory {
    Malformed,
    UnknownKey,
    InvalidPath,
    InvalidEnvironment,
    InvalidLimit,
    CapacityExceeded,
}

/// A validation or syntax error in checked-in run configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError {
    category: ConfigErrorCategory,
    message: String,
}

impl ConfigError {
    fn new(category: ConfigErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    pub fn category(&self) -> ConfigErrorCategory {
        self.category
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ConfigError {}

/// Parsed checked-in run policy with deterministic per-script overrides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunConfig {
    defaults: SandboxPolicy,
    scripts: BTreeMap<String, SandboxPolicy>,
}

impl RunConfig {
    pub fn parse_toml(input: &str) -> Result<Self, ConfigError> {
        Self::parse_toml_bytes(input.as_bytes())
    }

    /// Parses UTF-8 TOML after enforcing the input byte ceiling before deserialization.
    pub fn parse_toml_bytes(input: &[u8]) -> Result<Self, ConfigError> {
        if input.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::new(
                ConfigErrorCategory::CapacityExceeded,
                format!("run configuration exceeds {MAX_CONFIG_BYTES} bytes"),
            ));
        }
        let input = std::str::from_utf8(input).map_err(|error| {
            ConfigError::new(
                ConfigErrorCategory::Malformed,
                format!("run configuration is not valid UTF-8: {error}"),
            )
        })?;
        let root: ConfigDocument = toml::from_str(input).map_err(deserialize_error)?;
        if root.run.scripts.len() > MAX_PROFILE_COUNT {
            return Err(ConfigError::new(
                ConfigErrorCategory::CapacityExceeded,
                format!("run configuration exceeds {MAX_PROFILE_COUNT} script profiles"),
            ));
        }
        for name in root.run.scripts.keys() {
            validate_string_length("script profile name", name)?;
        }
        let defaults = apply_profile(SandboxPolicy::default(), root.run.defaults)?;
        let scripts = root
            .run
            .scripts
            .into_iter()
            .map(|(name, profile)| {
                apply_profile(defaults.clone(), profile).map(|policy| (name, policy))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { defaults, scripts })
    }

    pub fn defaults(&self) -> &SandboxPolicy {
        &self.defaults
    }

    /// Returns the exact script profile, or a clone of defaults when no override exists.
    pub fn profile_for(&self, script: &str) -> SandboxPolicy {
        self.scripts
            .get(script)
            .cloned()
            .unwrap_or_else(|| self.defaults.clone())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigDocument {
    run: RunSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RunSection {
    defaults: RawProfile,
    scripts: BTreeMap<String, RawProfile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawProfile {
    read: Option<Vec<String>>,
    write: Option<Vec<String>>,
    network: Option<bool>,
    environment: Option<Vec<String>>,
    subprocess: Option<bool>,
    timeout_seconds: Option<u64>,
    max_output_bytes: Option<u64>,
    max_processes: Option<u64>,
    max_memory_bytes: Option<u64>,
}

fn deserialize_error(error: toml::de::Error) -> ConfigError {
    let message = error.to_string();
    let category = if message.contains("unknown field") {
        ConfigErrorCategory::UnknownKey
    } else if [
        "timeout_seconds",
        "max_output_bytes",
        "max_processes",
        "max_memory_bytes",
    ]
    .iter()
    .any(|field| message.contains(field))
        && (message.contains("too large")
            || message.contains("out of range")
            || message.contains("expected u64"))
    {
        ConfigErrorCategory::InvalidLimit
    } else {
        ConfigErrorCategory::Malformed
    };
    ConfigError::new(category, message)
}

fn apply_profile(mut policy: SandboxPolicy, raw: RawProfile) -> Result<SandboxPolicy, ConfigError> {
    policy.filesystem = FilesystemPolicy::new(
        raw.read.unwrap_or(policy.filesystem.read),
        raw.write.unwrap_or(policy.filesystem.write),
    )?;
    policy.network = raw.network.unwrap_or(policy.network);
    if let Some(environment) = raw.environment {
        if environment.len() > MAX_ENVIRONMENT_COUNT {
            return Err(ConfigError::new(
                ConfigErrorCategory::CapacityExceeded,
                format!("sandbox policy exceeds {MAX_ENVIRONMENT_COUNT} environment names"),
            ));
        }
        for name in &environment {
            validate_string_length("environment variable name", name)?;
            validate_environment_name(name)?;
        }
        policy.environment = environment;
    }
    policy.subprocess = raw.subprocess.unwrap_or(policy.subprocess);
    policy.limits = ExecutionLimits::new(
        raw.timeout_seconds.or(policy.limits.timeout_seconds),
        raw.max_output_bytes.or(policy.limits.max_output_bytes),
        match raw.max_processes {
            Some(value) => Some(u32::try_from(value).map_err(|_| {
                invalid_limit("max_processes", "must fit in an unsigned 32-bit integer")
            })?),
            None => policy.limits.max_processes,
        },
        raw.max_memory_bytes.or(policy.limits.max_memory_bytes),
    )?;
    Ok(policy)
}

fn nonzero<T>(name: &str, value: Option<T>) -> Result<Option<T>, ConfigError>
where
    T: Copy + Default + PartialEq,
{
    if value == Some(T::default()) {
        Err(invalid_limit(name, "must be greater than zero"))
    } else {
        Ok(value)
    }
}

fn invalid_limit(name: &str, reason: &str) -> ConfigError {
    ConfigError::new(
        ConfigErrorCategory::InvalidLimit,
        format!("{name} {reason}"),
    )
}

fn validate_string_length(kind: &str, value: &str) -> Result<(), ConfigError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(ConfigError::new(
            ConfigErrorCategory::CapacityExceeded,
            format!("{kind} exceeds {MAX_STRING_BYTES} UTF-8 bytes"),
        ));
    }
    Ok(())
}

fn validate_project_path(value: &str) -> Result<(), ConfigError> {
    let path = Path::new(value);
    let windows_absolute = value.starts_with(['\\', '/'])
        || value.as_bytes().get(1) == Some(&b':')
            && value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic);
    let components: Vec<_> = value.split(['/', '\\']).collect();
    let invalid_component = components.iter().any(|component| {
        component.is_empty()
            || (*component == "." && value != ".")
            || *component == ".."
            || (*component != "." && component.ends_with(['.', ' ']))
            || component.chars().any(|character| {
                character.is_control()
                    || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
            })
            || is_windows_reserved_component(component)
    });
    if value.is_empty()
        || path.is_absolute()
        || windows_absolute
        || invalid_component
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ConfigError::new(
            ConfigErrorCategory::InvalidPath,
            format!("filesystem grant is not a portable project-relative path: {value:?}"),
        ));
    }
    Ok(())
}

fn is_windows_reserved_component(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || upper
        .strip_prefix("COM")
        .is_some_and(is_windows_reserved_port)
        || upper
            .strip_prefix("LPT")
            .is_some_and(is_windows_reserved_port)
}

fn is_windows_reserved_port(suffix: &str) -> bool {
    matches!(
        suffix,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

pub(crate) fn validate_environment_name(name: &str) -> Result<(), ConfigError> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic());
    if !valid_start || !chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        return Err(ConfigError::new(
            ConfigErrorCategory::InvalidEnvironment,
            format!("invalid environment variable name: {name:?}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_fail_closed_and_profiles_override_individual_fields() {
        let config = RunConfig::parse_toml(
            r#"
            [run.defaults]
            read = [".", "fixtures"]
            timeout_seconds = 30

            [run.scripts.build]
            write = ["dist"]
            network = true
            environment = ["NODE_ENV"]
            max_output_bytes = 4096
            max_processes = 4
            max_memory_bytes = 1048576
            "#,
        )
        .unwrap();

        let defaults = config.defaults();
        assert_eq!(defaults.mode(), SandboxMode::Required);
        assert_eq!(defaults.filesystem().read(), &[".", "fixtures"]);
        assert!(defaults.filesystem().write().is_empty());
        assert!(!defaults.network());
        assert!(defaults.environment().is_empty());
        assert!(defaults.subprocess());
        assert_eq!(defaults.limits().timeout_seconds(), Some(30));

        let build = config.profile_for("build");
        assert_eq!(build.mode(), SandboxMode::Required);
        assert_eq!(build.filesystem().read(), &[".", "fixtures"]);
        assert_eq!(build.filesystem().write(), &["dist"]);
        assert!(build.network());
        assert_eq!(build.environment(), &["NODE_ENV"]);
        assert!(build.subprocess());
        assert_eq!(build.limits().timeout_seconds(), Some(30));
        assert_eq!(build.limits().max_output_bytes(), Some(4096));
        assert_eq!(build.limits().max_processes(), Some(4));
        assert_eq!(build.limits().max_memory_bytes(), Some(1048576));
    }

    #[test]
    fn empty_config_uses_required_project_only_defaults() {
        let config = RunConfig::parse_toml("").unwrap();
        let defaults = config.defaults();
        assert_eq!(defaults.mode(), SandboxMode::Required);
        assert_eq!(defaults.filesystem().read(), &["."]);
        assert!(defaults.filesystem().write().is_empty());
        assert!(!defaults.network());
        assert!(defaults.environment().is_empty());
        assert!(defaults.subprocess());
        assert_eq!(defaults.limits(), &ExecutionLimits::default());
    }

    #[test]
    fn public_policy_constructors_validate_untrusted_values() {
        assert_eq!(
            FilesystemPolicy::new(vec!["../outside".into()], vec![])
                .unwrap_err()
                .category(),
            ConfigErrorCategory::InvalidPath
        );
        assert_eq!(
            ExecutionLimits::new(Some(0), None, None, None)
                .unwrap_err()
                .category(),
            ConfigErrorCategory::InvalidLimit
        );
        assert_eq!(
            SandboxPolicy::new(
                SandboxMode::Disabled,
                FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
                false,
                vec!["BAD-NAME".into()],
                true,
                ExecutionLimits::default(),
            )
            .unwrap_err()
            .category(),
            ConfigErrorCategory::InvalidEnvironment
        );
    }

    #[test]
    fn rejects_unknown_keys_and_malformed_field_types() {
        let unknown = RunConfig::parse_toml("[run.defaults]\nsandbox = \"disabled\"").unwrap_err();
        assert_eq!(unknown.category(), ConfigErrorCategory::UnknownKey);

        for source in [
            "unexpected = true",
            "[run]\nunexpected = true",
            "[run.scripts.build]\nunexpected = true",
        ] {
            let error = RunConfig::parse_toml(source).unwrap_err();
            assert_eq!(
                error.category(),
                ConfigErrorCategory::UnknownKey,
                "{source}"
            );
        }

        let malformed = RunConfig::parse_toml("[run.defaults]\nnetwork = \"yes\"").unwrap_err();
        assert_eq!(malformed.category(), ConfigErrorCategory::Malformed);
    }

    #[test]
    fn accepts_standard_toml_strings_comments_and_multiline_arrays() {
        let config = RunConfig::parse_toml(
            r#"
            [run.defaults] # standard TOML comments are supported
            read = [
                '.',
                "fixtures\u002fdata",
            ]
            environment = ["NODE_ENV"]

            [run.scripts."build:web"]
            write = ['dist']
            "#,
        )
        .unwrap();

        assert_eq!(
            config.defaults().filesystem().read(),
            &[".", "fixtures/data"]
        );
        assert_eq!(
            config.profile_for("build:web").filesystem().write(),
            &["dist"]
        );
    }

    #[test]
    fn rejects_absolute_and_traversing_grants_on_every_platform() {
        for path in ["/etc", "../secrets", "safe/../../secrets", r"C:\Windows"] {
            let source = format!("[run.defaults]\nread = [{path:?}]");
            let error = RunConfig::parse_toml(&source).unwrap_err();
            assert_eq!(error.category(), ConfigErrorCategory::InvalidPath, "{path}");
        }
    }

    #[test]
    fn rejects_nonportable_and_ambiguous_grant_components_on_every_platform() {
        for path in [
            "has\0nul",
            "file:stream",
            "dir/file:stream",
            "CON",
            "con.txt",
            "NUL.json",
            "aux.data/more",
            "COM1.log",
            "com¹.log",
            "LPT².txt",
            "CONIN$",
            "conout$.txt",
            "lpt9",
            "name.",
            "name ",
            "dir//file",
            "dir/./file",
            "dir/<file>",
            "dir/file?",
            "dir/file*",
            "dir/file|name",
            "dir/file\u{1f}",
        ] {
            let error = FilesystemPolicy::new(vec![path.to_owned()], vec![]).unwrap_err();
            assert_eq!(
                error.category(),
                ConfigErrorCategory::InvalidPath,
                "{path:?}"
            );
        }
    }

    #[test]
    fn accepts_portable_names_that_only_contain_reserved_name_substrings() {
        let policy = FilesystemPolicy::new(
            vec![
                "console".into(),
                "connection.txt".into(),
                "com10.log".into(),
            ],
            vec!["auxiliary/output".into()],
        )
        .unwrap();
        assert_eq!(policy.read().len(), 3);
        assert_eq!(policy.write().len(), 1);
    }

    #[test]
    fn parsing_rejects_config_bytes_above_the_published_limit() {
        let at_limit = vec![b' '; MAX_CONFIG_BYTES];
        assert!(RunConfig::parse_toml_bytes(&at_limit).is_ok());

        let above_limit = vec![b' '; MAX_CONFIG_BYTES + 1];
        let error = RunConfig::parse_toml_bytes(&above_limit).unwrap_err();
        assert_eq!(error.category(), ConfigErrorCategory::CapacityExceeded);
    }

    #[test]
    fn parsing_bounds_script_profile_count_at_the_published_limit() {
        let source = (0..MAX_PROFILE_COUNT)
            .map(|index| format!("[run.scripts.p{index}]\nnetwork = false\n"))
            .collect::<String>();
        assert!(RunConfig::parse_toml(&source).is_ok());

        let above_limit = format!("{source}[run.scripts.overflow]\nnetwork = false\n");
        let error = RunConfig::parse_toml(&above_limit).unwrap_err();
        assert_eq!(error.category(), ConfigErrorCategory::CapacityExceeded);
    }

    #[test]
    fn filesystem_policy_bounds_total_grant_count_at_the_published_limit() {
        let at_limit = vec!["portable".to_owned(); MAX_GRANT_COUNT];
        assert!(FilesystemPolicy::new(at_limit, vec![]).is_ok());

        let above_limit = vec!["portable".to_owned(); MAX_GRANT_COUNT + 1];
        let error = FilesystemPolicy::new(above_limit, vec![]).unwrap_err();
        assert_eq!(error.category(), ConfigErrorCategory::CapacityExceeded);
    }

    #[test]
    fn sandbox_policy_bounds_environment_count_at_the_published_limit() {
        let filesystem = FilesystemPolicy::new(vec![".".into()], vec![]).unwrap();
        let at_limit = vec!["VALID_NAME".to_owned(); MAX_ENVIRONMENT_COUNT];
        assert!(
            SandboxPolicy::new(
                SandboxMode::Required,
                filesystem.clone(),
                false,
                at_limit,
                true,
                ExecutionLimits::default(),
            )
            .is_ok()
        );

        let above_limit = vec!["VALID_NAME".to_owned(); MAX_ENVIRONMENT_COUNT + 1];
        let error = SandboxPolicy::new(
            SandboxMode::Required,
            filesystem,
            false,
            above_limit,
            true,
            ExecutionLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.category(), ConfigErrorCategory::CapacityExceeded);
    }

    #[test]
    fn filesystem_policy_bounds_grant_string_bytes_at_the_published_limit() {
        let at_limit = "a".repeat(MAX_STRING_BYTES);
        assert!(FilesystemPolicy::new(vec![at_limit], vec![]).is_ok());

        let above_limit = "a".repeat(MAX_STRING_BYTES + 1);
        let error = FilesystemPolicy::new(vec![above_limit], vec![]).unwrap_err();
        assert_eq!(error.category(), ConfigErrorCategory::CapacityExceeded);
    }

    #[test]
    fn sandbox_policy_bounds_environment_name_bytes_at_the_published_limit() {
        let filesystem = FilesystemPolicy::new(vec![".".into()], vec![]).unwrap();
        let at_limit = format!("E{}", "A".repeat(MAX_STRING_BYTES - 1));
        assert!(
            SandboxPolicy::new(
                SandboxMode::Required,
                filesystem.clone(),
                false,
                vec![at_limit],
                true,
                ExecutionLimits::default(),
            )
            .is_ok()
        );

        let above_limit = format!("E{}", "A".repeat(MAX_STRING_BYTES));
        let error = SandboxPolicy::new(
            SandboxMode::Required,
            filesystem,
            false,
            vec![above_limit],
            true,
            ExecutionLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.category(), ConfigErrorCategory::CapacityExceeded);
    }

    #[test]
    fn parsing_bounds_profile_name_bytes_at_the_published_limit() {
        let at_limit = "a".repeat(MAX_STRING_BYTES);
        let source = format!("[run.scripts.{at_limit:?}]\nnetwork = false\n");
        assert!(RunConfig::parse_toml(&source).is_ok());

        let above_limit = "a".repeat(MAX_STRING_BYTES + 1);
        let source = format!("[run.scripts.{above_limit:?}]\nnetwork = false\n");
        let error = RunConfig::parse_toml(&source).unwrap_err();
        assert_eq!(error.category(), ConfigErrorCategory::CapacityExceeded);
    }

    #[test]
    fn parsing_applies_environment_count_limit_to_every_profile() {
        let names = (0..=MAX_ENVIRONMENT_COUNT)
            .map(|index| format!("\"E{index}\""))
            .collect::<Vec<_>>()
            .join(",");
        for table in ["run.defaults", "run.scripts.build"] {
            let source = format!("[{table}]\nenvironment = [{names}]\n");
            let error = RunConfig::parse_toml(&source).unwrap_err();
            assert_eq!(error.category(), ConfigErrorCategory::CapacityExceeded);
        }
    }

    #[test]
    fn rejects_invalid_environment_names() {
        for name in ["", "1TOKEN", "BAD-NAME", "UNICODÉ"] {
            let source = format!("[run.defaults]\nenvironment = [{name:?}]");
            let error = RunConfig::parse_toml(&source).unwrap_err();
            assert_eq!(
                error.category(),
                ConfigErrorCategory::InvalidEnvironment,
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_zero_and_overflowing_limits() {
        for field in [
            "timeout_seconds",
            "max_output_bytes",
            "max_processes",
            "max_memory_bytes",
        ] {
            let error = RunConfig::parse_toml(&format!("[run.defaults]\n{field} = 0")).unwrap_err();
            assert_eq!(
                error.category(),
                ConfigErrorCategory::InvalidLimit,
                "{field}"
            );
        }

        let process_overflow =
            RunConfig::parse_toml("[run.defaults]\nmax_processes = 4294967296").unwrap_err();
        assert_eq!(
            process_overflow.category(),
            ConfigErrorCategory::InvalidLimit
        );

        let integer_overflow =
            RunConfig::parse_toml("[run.defaults]\nmax_memory_bytes = 18446744073709551616")
                .unwrap_err();
        assert_eq!(
            integer_overflow.category(),
            ConfigErrorCategory::InvalidLimit,
            "{integer_overflow}"
        );
    }

    #[test]
    fn profile_lookup_is_exact_and_deterministic() {
        let config = RunConfig::parse_toml(
            "[run.scripts.z]\nnetwork = true\n[run.scripts.a]\nsubprocess = false",
        )
        .unwrap();
        assert!(config.profile_for("z").network());
        assert!(!config.profile_for("a").subprocess());
        assert_eq!(config.profile_for("missing"), config.defaults().clone());
        assert_eq!(config.profile_for("A"), config.defaults().clone());
    }
}
