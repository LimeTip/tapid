//! Host platform detection for package compatibility.
use tapid_core::PackagePlatform;

pub(crate) fn selection_target() -> tapid_lockfile::SelectionTarget {
    tapid_lockfile::SelectionTarget {
        os: std::env::consts::OS.into(),
        cpu: std::env::consts::ARCH.into(),
        libc: current_libc().map(str::to_owned),
    }
}

pub(crate) fn current_libc() -> Option<&'static str> {
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

pub(super) fn current_platform_matches(platform: &PackagePlatform) -> bool {
    platform.matches(std::env::consts::OS, std::env::consts::ARCH, current_libc())
}
