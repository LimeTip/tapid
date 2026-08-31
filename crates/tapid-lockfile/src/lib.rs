//! Deterministic lockfile models and canonical JSON serialization for Tapid.

#![deny(unsafe_code)]

mod error;
mod model;
mod validation;

#[cfg(test)]
mod tests;

pub use error::LockfileError;
pub use model::{LockedPackage, Lockfile, LockfilePackageKey, RegistryIntegrityProvenance};

/// Returns the current crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Current lockfile schema with exact direct roots and registry-integrity provenance.
pub const LOCKFILE_VERSION: u32 = 6;
const LEGACY_LOCKFILE_VERSION: u32 = 4;
const PROVENANCE_LEGACY_LOCKFILE_VERSION: u32 = 5;
