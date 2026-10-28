//! Explicit install/CI materialization policy. Never inferred from NODE_ENV.

use clap::{Args, ValueEnum};
use tapid_lockfile::InstallProfile;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Kind {
    Dev,
    Optional,
}

#[derive(Debug, Args)]
pub(super) struct ProfileArgs {
    /// Omit dependency kinds from materialization, preserving locked selections.
    #[arg(long, value_enum)]
    omit: Vec<Kind>,
    /// Include dependency kinds, overriding omission regardless of argument order.
    #[arg(long, value_enum)]
    include: Vec<Kind>,
    /// Omit development dependencies unless explicitly included.
    #[arg(long)]
    production: bool,
}

impl ProfileArgs {
    pub(super) fn effective(&self) -> InstallProfile {
        InstallProfile {
            include_dev: self.include.contains(&Kind::Dev)
                || !(self.production || self.omit.contains(&Kind::Dev)),
            include_optional: self.include.contains(&Kind::Optional)
                || !self.omit.contains(&Kind::Optional),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProfileArgs;
    use clap::Parser;
    use tapid_lockfile::InstallProfile;

    #[derive(Parser)]
    struct Invocation {
        #[command(flatten)]
        profile: ProfileArgs,
    }

    #[test]
    fn profile_arguments_default_to_full_and_select_each_kind_explicitly() {
        for (arguments, expected) in [
            (vec!["test"], InstallProfile::default()),
            (vec!["test", "--production"], InstallProfile::production()),
            (vec!["test", "--omit", "dev"], InstallProfile::production()),
            (
                vec!["test", "--omit", "optional"],
                InstallProfile {
                    include_dev: true,
                    include_optional: false,
                },
            ),
            (
                vec!["test", "--production", "--omit", "optional"],
                InstallProfile {
                    include_dev: false,
                    include_optional: false,
                },
            ),
        ] {
            assert_eq!(
                Invocation::try_parse_from(arguments)
                    .unwrap()
                    .profile
                    .effective(),
                expected
            );
        }
    }

    #[test]
    fn profile_arguments_reject_unknown_kinds_and_missing_values() {
        for arguments in [
            vec!["test", "--omit", "peer"],
            vec!["test", "--include", "production"],
            vec!["test", "--omit", "Dev"],
            vec!["test", "--omit", "dev,optional"],
            vec!["test", "--include"],
        ] {
            assert!(Invocation::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn profile_arguments_are_exposed_by_install_and_ci() {
        for command in ["install", "ci"] {
            for arguments in [
                vec!["--production"],
                vec!["--omit", "dev"],
                vec!["--include", "optional"],
            ] {
                let mut invocation = vec!["tapid", command];
                invocation.extend(arguments);
                assert!(super::super::Cli::try_parse_from(invocation).is_ok());
            }
        }
    }

    #[test]
    fn profile_arguments_include_wins_and_repetitions_are_idempotent() {
        for arguments in [
            vec!["test", "--production", "--include", "dev"],
            vec!["test", "--include", "dev", "--production"],
            vec!["test", "--omit", "dev", "--omit", "dev", "--include", "dev"],
            vec!["test", "--omit", "optional", "--include", "optional"],
        ] {
            assert_eq!(
                Invocation::try_parse_from(arguments)
                    .unwrap()
                    .profile
                    .effective(),
                InstallProfile::default()
            );
        }
    }
}
