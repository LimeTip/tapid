//! Portable invocation permission arguments and policy selection.
use clap::Args;
use tapid_runner::{
    AssuranceLevel, ConfigError, ExecutionLimits, FilesystemPolicy, RunConfig, SandboxMode,
    SandboxPolicy,
};

#[derive(Debug, Default, Args)]
pub(crate) struct InvocationPermissions {
    /// Allow reading a portable project-relative file or directory. Repeat for multiple paths.
    #[arg(long, value_name = "PATH")]
    pub allow_read: Vec<String>,
    /// Allow writing a portable project-relative file or directory. Repeat for multiple paths.
    #[arg(long, value_name = "PATH")]
    pub allow_write: Vec<String>,
    /// Allow an environment variable by name, never NAME=VALUE. Repeat for multiple names.
    #[arg(long, value_name = "NAME")]
    pub allow_env: Vec<String>,
    /// Allow subprocess creation within the same containment policy.
    #[arg(long)]
    pub allow_subprocess: bool,
    /// Request unrestricted networking; unsupported backends reject before execution.
    #[arg(long, visible_alias = "allow-net")]
    pub allow_network: bool,
}

pub(super) struct PolicySelection {
    pub policy: SandboxPolicy,
    pub ceiling: Option<SandboxPolicy>,
    pub source: &'static str,
}

impl PolicySelection {
    pub(super) fn provenance(&self, invocation: &InvocationPermissions) -> serde_json::Value {
        let policy = |policy: &SandboxPolicy| {
            serde_json::json!({
                "assurance": format!("{:?}", policy.assurance()),
                "read": policy.filesystem().read(), "write": policy.filesystem().write(),
                "environment": policy.environment(), "network": policy.network(), "subprocess": policy.subprocess(),
                "limits": {"timeout_seconds": policy.limits().timeout_seconds(), "max_output_bytes": policy.limits().max_output_bytes(),
                    "max_processes": policy.limits().max_processes(), "max_memory_bytes": policy.limits().max_memory_bytes()}
            })
        };
        serde_json::json!({
            "source": self.source,
            "configuration": self.ceiling.as_ref().map(policy),
            "invocation": {"read": invocation.allow_read, "write": invocation.allow_write,
                "environment": invocation.allow_env, "network": invocation.allow_network,
                "subprocess": invocation.allow_subprocess},
            "effective": policy(&self.policy),
        })
    }
}

impl InvocationPermissions {
    pub(super) fn enabled(&self) -> bool {
        !self.allow_read.is_empty()
            || !self.allow_write.is_empty()
            || !self.allow_env.is_empty()
            || self.allow_subprocess
            || self.allow_network
    }

    pub(super) fn select(
        &self,
        config: &RunConfig,
        script: &str,
    ) -> Result<PolicySelection, ConfigError> {
        let (ceiling, source) = if let Some(profile) = config.exact_profile(script) {
            (Some(profile), "script-profile")
        } else if let Some(defaults) = config.configured_defaults() {
            (Some(defaults), "configured-defaults")
        } else {
            (None, "invocation-baseline")
        };
        let baseline = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::Restricted,
            FilesystemPolicy::new(vec![], vec![])?,
            false,
            vec![],
            false,
            ExecutionLimits::default(),
        )?;
        let base = ceiling.unwrap_or(&baseline);
        let selected = |requested: &[String], configured: &[String]| {
            if requested.is_empty() {
                configured.to_vec()
            } else {
                requested.to_vec()
            }
        };
        let policy = SandboxPolicy::new_with_assurance(
            base.mode(),
            base.assurance(),
            FilesystemPolicy::new(
                selected(&self.allow_read, base.filesystem().read()),
                selected(&self.allow_write, base.filesystem().write()),
            )?,
            self.allow_network || base.network(),
            selected(&self.allow_env, base.environment()),
            self.allow_subprocess || base.subprocess(),
            base.limits().clone(),
        )?;
        Ok(PolicySelection {
            policy,
            ceiling: ceiling.cloned(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::InvocationPermissions;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Command {
        #[command(flatten)]
        args: crate::commands::run::Args,
    }

    #[test]
    fn flag_only_policy_grants_only_requested_permissions() {
        let args = Command::try_parse_from([
            "run",
            "build",
            "--allow-read",
            ".",
            "--allow-write",
            "out",
            "--allow-env",
            "CI",
            "--allow-subprocess",
        ])
        .unwrap()
        .args;
        let selection = args
            .permissions
            .select(&tapid_runner::RunConfig::parse_toml("").unwrap(), "build")
            .unwrap();
        assert!(selection.ceiling.is_none());
        assert_eq!(
            selection.policy.assurance(),
            tapid_runner::AssuranceLevel::Restricted
        );
        assert_eq!(selection.policy.mode(), tapid_runner::SandboxMode::Required);
        assert_eq!(selection.policy.filesystem().read(), ["."]);
        assert_eq!(selection.policy.filesystem().write(), ["out"]);
        assert_eq!(selection.policy.environment(), ["CI"]);
        assert!(selection.policy.subprocess());
        assert!(!selection.policy.network());
    }

    #[test]
    fn selection_preserves_profile_assurance_limits_and_unspecified_permissions() {
        let config = tapid_runner::RunConfig::parse_toml("[run.scripts.build]\nread=['.']\nwrite=['out']\nenvironment=['CI']\ntimeout_seconds=5\nmax_output_bytes=1024\nmax_processes=4\nmax_memory_bytes=8192\n").unwrap();
        let args = Command::try_parse_from(["run", "build", "--allow-read", "src"])
            .unwrap()
            .args;
        let selection = args.permissions.select(&config, "build").unwrap();
        let profile = config.exact_profile("build").unwrap();
        assert_eq!(selection.ceiling.as_ref(), Some(profile));
        assert_eq!(selection.policy.filesystem().read(), ["src"]);
        assert_eq!(
            selection.policy.filesystem().write(),
            profile.filesystem().write()
        );
        assert_eq!(selection.policy.environment(), profile.environment());
        assert_eq!(selection.policy.assurance(), profile.assurance());
        assert_eq!(selection.policy.limits(), profile.limits());
    }

    #[test]
    fn explicit_defaults_are_a_ceiling_even_without_a_script_profile() {
        let config = tapid_runner::RunConfig::parse_toml(
            "[run.defaults]\nassurance='restricted'\nsubprocess=false\nwrite=[]\n",
        )
        .unwrap();
        let selection = InvocationPermissions::default()
            .select(&config, "build")
            .unwrap();
        assert_eq!(selection.ceiling.as_ref(), Some(config.defaults()));
        assert!(!selection.policy.subprocess());
        assert!(selection.policy.filesystem().write().is_empty());
    }

    #[test]
    fn repeated_grants_and_aliases_use_one_portable_parser() {
        let parsed = Command::try_parse_from([
            "tapid-run",
            "build",
            "--allow-read",
            ".",
            "--allow-read",
            "src",
            "--allow-write",
            "build output",
            "--allow-env",
            "CI",
            "--allow-subprocess",
            "--allow-net",
            "--",
            "--allow-write",
            "forwarded",
        ])
        .unwrap();
        assert_eq!(parsed.args.permissions.allow_read, [".", "src"]);
        assert_eq!(parsed.args.permissions.allow_write, ["build output"]);
        assert_eq!(parsed.args.permissions.allow_env, ["CI"]);
        assert!(parsed.args.permissions.allow_subprocess);
        assert!(parsed.args.permissions.allow_network);
        assert_eq!(
            parsed.args.arguments,
            ["--allow-write", "forwarded"].map(std::ffi::OsString::from)
        );
    }

    #[test]
    fn scoped_values_are_required_and_network_has_no_host_scope() {
        for flag in ["--allow-read", "--allow-write", "--allow-env"] {
            assert!(Command::try_parse_from(["tapid-run", "build", flag]).is_err());
        }
        assert!(
            Command::try_parse_from(["tapid-run", "build", "--allow-network=example.com"]).is_err()
        );
    }
}
