use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path};

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
        for path in read.iter().chain(&write) {
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
        let timeout_seconds = validate_nonzero("timeout_seconds", timeout_seconds, None)?;
        let max_output_bytes = validate_nonzero("max_output_bytes", max_output_bytes, None)?;
        let max_memory_bytes = validate_nonzero("max_memory_bytes", max_memory_bytes, None)?;
        if max_processes == Some(0) {
            return Err(invalid_limit("max_processes", "must be greater than zero"));
        }
        Ok(Self {
            timeout_seconds,
            max_output_bytes,
            max_processes,
            max_memory_bytes,
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
        for name in &environment {
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
        let run = parse_run_section(input)?;
        let defaults = apply_profile(SandboxPolicy::default(), run.defaults)?;
        let mut scripts = BTreeMap::new();
        for (name, profile) in run.scripts {
            scripts.insert(name, apply_profile(defaults.clone(), profile)?);
        }
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

#[derive(Debug, Default)]
struct RunSection {
    defaults: RawProfile,
    scripts: BTreeMap<String, RawProfile>,
}

#[derive(Debug, Default)]
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

#[derive(Clone, Debug)]
enum CurrentProfile {
    Defaults,
    Script(String),
}

fn parse_run_section(input: &str) -> Result<RunSection, ConfigError> {
    let mut run = RunSection::default();
    let mut current: Option<CurrentProfile> = None;

    for (index, original) in input.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(original)?.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                return Err(malformed(line_number, "unterminated table header"));
            }
            let table = line[1..line.len() - 1].trim();
            current = if table == "run.defaults" {
                Some(CurrentProfile::Defaults)
            } else if let Some(name) = table.strip_prefix("run.scripts.") {
                Some(CurrentProfile::Script(parse_script_name(
                    name,
                    line_number,
                )?))
            } else {
                return Err(ConfigError::new(
                    ConfigErrorCategory::UnknownKey,
                    format!("line {line_number}: unknown table {table:?}"),
                ));
            };
            continue;
        }

        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| malformed(line_number, "expected key = value"))?;
        let key = key.trim();
        let value = value.trim();
        let profile = match current.as_ref() {
            Some(CurrentProfile::Defaults) => &mut run.defaults,
            Some(CurrentProfile::Script(name)) => run.scripts.entry(name.clone()).or_default(),
            None => {
                return Err(malformed(
                    line_number,
                    "fields must be inside a run profile",
                ));
            }
        };
        assign_field(profile, key, value, line_number)?;
    }

    Ok(run)
}

fn assign_field(
    profile: &mut RawProfile,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), ConfigError> {
    macro_rules! assign {
        ($field:ident, $parsed:expr) => {{
            if profile.$field.is_some() {
                return Err(malformed(
                    line,
                    concat!("duplicate field ", stringify!($field)),
                ));
            }
            profile.$field = Some($parsed?);
        }};
    }

    match key {
        "read" => assign!(read, parse_string_array(value, line)),
        "write" => assign!(write, parse_string_array(value, line)),
        "network" => assign!(network, parse_bool(value, line)),
        "environment" => assign!(environment, parse_string_array(value, line)),
        "subprocess" => assign!(subprocess, parse_bool(value, line)),
        "timeout_seconds" => assign!(timeout_seconds, parse_limit(value, key, line)),
        "max_output_bytes" => assign!(max_output_bytes, parse_limit(value, key, line)),
        "max_processes" => assign!(max_processes, parse_limit(value, key, line)),
        "max_memory_bytes" => assign!(max_memory_bytes, parse_limit(value, key, line)),
        _ => {
            return Err(ConfigError::new(
                ConfigErrorCategory::UnknownKey,
                format!("line {line}: unknown run profile field {key:?}"),
            ));
        }
    }
    Ok(())
}

fn parse_bool(value: &str, line: usize) -> Result<bool, ConfigError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(malformed(line, "expected a boolean")),
    }
}

fn parse_limit(value: &str, name: &str, line: usize) -> Result<u64, ConfigError> {
    value.parse::<u64>().map_err(|_| {
        ConfigError::new(
            ConfigErrorCategory::InvalidLimit,
            format!("line {line}: {name} must be an unsigned 64-bit integer"),
        )
    })
}

fn parse_script_name(value: &str, line: usize) -> Result<String, ConfigError> {
    let value = value.trim();
    if value.starts_with('"') {
        return parse_quoted_string(value, line);
    }
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(malformed(line, "invalid script profile name"));
    }
    Ok(value.to_owned())
}

fn parse_string_array(value: &str, line: usize) -> Result<Vec<String>, ConfigError> {
    let inner = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| malformed(line, "expected an array of strings"))?
        .trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }

    let mut values = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in inner.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' && quoted {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if byte == b',' && !quoted {
            values.push(parse_quoted_string(inner[start..index].trim(), line)?);
            start = index + 1;
        }
    }
    if quoted || escaped {
        return Err(malformed(line, "unterminated string in array"));
    }
    values.push(parse_quoted_string(inner[start..].trim(), line)?);
    Ok(values)
}

fn parse_quoted_string(value: &str, line: usize) -> Result<String, ConfigError> {
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| malformed(line, "expected a basic quoted string"))?;
    let mut output = String::new();
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let escaped = chars
            .next()
            .ok_or_else(|| malformed(line, "unterminated string escape"))?;
        output.push(match escaped {
            '"' => '"',
            '\\' => '\\',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            _ => return Err(malformed(line, "unsupported string escape")),
        });
    }
    Ok(output)
}

fn strip_comment(line: &str) -> Result<&str, ConfigError> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' && quoted {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if byte == b'#' && !quoted {
            return Ok(&line[..index]);
        }
    }
    if quoted || escaped {
        return Err(ConfigError::new(
            ConfigErrorCategory::Malformed,
            "unterminated quoted string",
        ));
    }
    Ok(line)
}

fn malformed(line: usize, reason: &str) -> ConfigError {
    ConfigError::new(
        ConfigErrorCategory::Malformed,
        format!("line {line}: {reason}"),
    )
}

fn apply_profile(mut policy: SandboxPolicy, raw: RawProfile) -> Result<SandboxPolicy, ConfigError> {
    let read = raw.read.unwrap_or_else(|| policy.filesystem.read.clone());
    let write = raw.write.unwrap_or_else(|| policy.filesystem.write.clone());
    policy.filesystem = FilesystemPolicy::new(read, write)?;
    if let Some(network) = raw.network {
        policy.network = network;
    }
    if let Some(environment) = raw.environment {
        for name in &environment {
            validate_environment_name(name)?;
        }
        policy.environment = environment;
    }
    if let Some(subprocess) = raw.subprocess {
        policy.subprocess = subprocess;
    }
    policy.limits.timeout_seconds = validate_nonzero(
        "timeout_seconds",
        raw.timeout_seconds,
        policy.limits.timeout_seconds,
    )?;
    policy.limits.max_output_bytes = validate_nonzero(
        "max_output_bytes",
        raw.max_output_bytes,
        policy.limits.max_output_bytes,
    )?;
    policy.limits.max_memory_bytes = validate_nonzero(
        "max_memory_bytes",
        raw.max_memory_bytes,
        policy.limits.max_memory_bytes,
    )?;
    policy.limits.max_processes = match raw.max_processes {
        Some(0) => return Err(invalid_limit("max_processes", "must be greater than zero")),
        Some(value) => Some(u32::try_from(value).map_err(|_| {
            invalid_limit("max_processes", "must fit in an unsigned 32-bit integer")
        })?),
        None => policy.limits.max_processes,
    };
    Ok(policy)
}

fn validate_nonzero(
    name: &str,
    value: Option<u64>,
    inherited: Option<u64>,
) -> Result<Option<u64>, ConfigError> {
    match value {
        Some(0) => Err(invalid_limit(name, "must be greater than zero")),
        Some(value) => Ok(Some(value)),
        None => Ok(inherited),
    }
}

fn invalid_limit(name: &str, reason: &str) -> ConfigError {
    ConfigError::new(
        ConfigErrorCategory::InvalidLimit,
        format!("{name} {reason}"),
    )
}

fn validate_project_path(value: &str) -> Result<(), ConfigError> {
    let path = Path::new(value);
    let windows_absolute = value.starts_with(['\\', '/'])
        || value.as_bytes().get(1) == Some(&b':')
            && value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic);
    let traverses = value.split(['/', '\\']).any(|component| component == "..");
    if value.is_empty()
        || path.is_absolute()
        || windows_absolute
        || traverses
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ConfigError::new(
            ConfigErrorCategory::InvalidPath,
            format!("filesystem grant must be project-relative: {value:?}"),
        ));
    }
    Ok(())
}

fn validate_environment_name(name: &str) -> Result<(), ConfigError> {
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

        let malformed = RunConfig::parse_toml("[run.defaults]\nnetwork = \"yes\"").unwrap_err();
        assert_eq!(malformed.category(), ConfigErrorCategory::Malformed);
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
            ConfigErrorCategory::InvalidLimit
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
