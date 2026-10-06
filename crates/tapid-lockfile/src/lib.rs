//! Deterministic lockfile models and canonical JSON serialization for Tapid.

#![deny(unsafe_code)]

mod error;
mod model;
mod validation;

#[cfg(test)]
mod tests;

pub use error::LockfileError;
pub use model::{
    LocalWorkspaceSource, LockedPackage, LockedWorkspacePackage, Lockfile, LockfilePackageKey,
    LockfilePackageSource, RegistryIntegrityProvenance,
};

/// Returns the current crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Current schema with source-qualified registry and workspace package identities.
pub const LOCKFILE_VERSION: u32 = 7;
const ROOTS_LEGACY_LOCKFILE_VERSION: u32 = 6;
const REGISTRY_ONLY_LOCKFILE_VERSION: u32 = ROOTS_LEGACY_LOCKFILE_VERSION;
const LEGACY_LOCKFILE_VERSION: u32 = 4;
const PROVENANCE_LEGACY_LOCKFILE_VERSION: u32 = 5;
