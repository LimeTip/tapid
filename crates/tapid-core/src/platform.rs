//! npm package compatibility and selected platform identity.
use crate::PlatformContext;

/// npm package compatibility constraints used for deterministic platform selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagePlatform {
    /// Supported or negated npm operating-system identifiers.
    pub os: Vec<String>,
    /// Supported or negated npm CPU architecture identifiers.
    pub cpu: Vec<String>,
    /// Supported or negated npm libc identifiers.
    pub libc: Vec<String>,
}
impl PackagePlatform {
    /// Returns constraints that permit every supported platform.
    pub fn unrestricted() -> Self {
        Self {
            os: Vec::new(),
            cpu: Vec::new(),
            libc: Vec::new(),
        }
    }
}
fn npm_os(value: &str) -> &str {
    match value {
        "macos" => "darwin",
        "windows" => "win32",
        value => value,
    }
}

fn npm_cpu(value: &str) -> &str {
    match value {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        value => value,
    }
}

impl PackagePlatform {
    /// Records only dimensions constrained by this package.
    pub fn selected_context(
        &self,
        os: &str,
        cpu: &str,
        libc: Option<&str>,
    ) -> Result<PlatformContext, String> {
        let libc_context = if self.libc.is_empty()
            || npm_os(os) != "linux"
            || (self.libc.iter().all(|value| value.starts_with('!')) && libc.is_none())
        {
            None
        } else {
            Some(libc.ok_or("selected package requires a libc platform context")?)
        };
        PlatformContext::new(
            (!self.os.is_empty()).then_some(npm_os(os)),
            (!self.cpu.is_empty()).then_some(npm_cpu(cpu)),
            libc_context,
        )
        .map_err(|error| error.to_string())
    }

    /// Matches npm aliases, positive constraints, exclusions, and Linux libc rules.
    pub fn matches(&self, os: &str, cpu: &str, libc: Option<&str>) -> bool {
        fn value_matches(values: &[String], current: Option<&str>) -> bool {
            if values.is_empty() {
                return true;
            }
            let Some(current) = current else {
                return false;
            };
            let mut has_positive = false;
            let mut positive_match = false;
            for value in values {
                if let Some(excluded) = value.strip_prefix('!') {
                    if excluded == current {
                        return false;
                    }
                } else {
                    has_positive = true;
                    positive_match |= value == current;
                }
            }
            !has_positive || positive_match
        }

        let os = npm_os(os);
        let cpu = npm_cpu(cpu);
        let libc_matches = if os != "linux"
            || (libc.is_none() && self.libc.iter().all(|value| value.starts_with('!')))
        {
            true
        } else {
            value_matches(&self.libc, libc)
        };

        value_matches(&self.os, Some(os)) && value_matches(&self.cpu, Some(cpu)) && libc_matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_platform_constraints_produce_an_exact_lockfile_context() {
        let platform = PackagePlatform {
            os: vec!["darwin".into()],
            cpu: vec!["arm64".into()],
            libc: Vec::new(),
        };

        let context = platform.selected_context("macos", "aarch64", None).unwrap();

        assert_eq!(context.os.as_deref(), Some("darwin"));
        assert_eq!(context.cpu.as_deref(), Some("arm64"));
        assert_eq!(context.libc, None);
    }

    #[test]
    fn libc_constraints_follow_linux_only_npm_semantics() {
        let positive = PackagePlatform {
            os: Vec::new(),
            cpu: Vec::new(),
            libc: vec!["glibc".into()],
        };
        let exclusion_only = PackagePlatform {
            os: Vec::new(),
            cpu: Vec::new(),
            libc: vec!["!musl".into()],
        };

        assert!(positive.matches("macos", "aarch64", None));
        assert!(!positive.matches("linux", "x86_64", None));
        assert!(exclusion_only.matches("linux", "x86_64", None));
        assert_eq!(
            positive
                .selected_context("macos", "aarch64", None)
                .unwrap()
                .libc,
            None
        );
    }
}
