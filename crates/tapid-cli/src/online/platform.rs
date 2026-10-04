//! npm platform constraints and selected lockfile context.

use tapid_registry_client::PackagePlatform;

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

pub(super) fn selected_platform_context_for(
    os: &str,
    cpu: &str,
    libc: Option<&str>,
    constraints: &PackagePlatform,
) -> Result<tapid_core::PlatformContext, String> {
    let libc_context = if constraints.libc.is_empty()
        || npm_os(os) != "linux"
        || (constraints.libc.iter().all(|value| value.starts_with('!')) && libc.is_none())
    {
        None
    } else {
        Some(libc.ok_or("selected package requires a libc platform context")?)
    };
    tapid_core::PlatformContext::new(
        (!constraints.os.is_empty()).then_some(npm_os(os)),
        (!constraints.cpu.is_empty()).then_some(npm_cpu(cpu)),
        libc_context,
    )
    .map_err(|error| error.to_string())
}

pub(super) fn current_libc() -> Option<&'static str> {
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    {
        Some("musl")
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        Some("glibc")
    }
    #[cfg(any(
        not(target_os = "linux"),
        all(target_os = "linux", not(any(target_env = "musl", target_env = "gnu")))
    ))]
    {
        None
    }
}

pub(super) fn platform_matches_for(
    os: &str,
    cpu: &str,
    libc: Option<&str>,
    platform: &PackagePlatform,
) -> bool {
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
        || (libc.is_none() && platform.libc.iter().all(|value| value.starts_with('!')))
    {
        true
    } else {
        value_matches(&platform.libc, libc)
    };

    value_matches(&platform.os, Some(os)) && value_matches(&platform.cpu, Some(cpu)) && libc_matches
}

pub(super) fn current_platform_matches(platform: &PackagePlatform) -> bool {
    platform_matches_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        current_libc(),
        platform,
    )
}
