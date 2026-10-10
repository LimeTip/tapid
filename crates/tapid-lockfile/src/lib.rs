//! Deterministic lockfile models and canonical JSON serialization for Tapid.

#![deny(unsafe_code)]

mod error;
mod model;
mod npm_import;
mod validation;

#[cfg(test)]
mod tests;

pub use error::LockfileError;
pub use model::{
    DerivedHookOutput, LocalWorkspaceSource, LockedPackage, LockedWorkspacePackage, Lockfile,
    LockfilePackageKey, LockfilePackageSource, RegistryIntegrityProvenance,
};

/// Returns the current crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Current schema with source-qualified registry and workspace package identities.
pub const LOCKFILE_VERSION: u32 = 7;
/// Lifecycle-enabled locks require a reader which validates derived-output approvals.
pub const LIFECYCLE_LOCKFILE_VERSION: u32 = 9;
/// Locks with immutable copied file and Git sources.
pub const COPIED_SOURCE_LOCKFILE_VERSION: u32 = 10;
const ROOTS_LEGACY_LOCKFILE_VERSION: u32 = 6;
const REGISTRY_ONLY_LOCKFILE_VERSION: u32 = ROOTS_LEGACY_LOCKFILE_VERSION;
const LEGACY_LOCKFILE_VERSION: u32 = 4;
const PROVENANCE_LEGACY_LOCKFILE_VERSION: u32 = 5;

pub use npm_import::{
    ImportedNpmArtifactReceipt, ImportedNpmGraph, ImportedNpmLockfile, ImportedNpmPackage,
    NpmImportError,
};
