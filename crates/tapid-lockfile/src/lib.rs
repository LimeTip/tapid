//! Deterministic lockfile models and canonical JSON serialization for Tapid.

#![deny(unsafe_code)]

mod error;
mod model;
mod validation;

#[cfg(test)]
mod tests;

pub use error::LockfileError;
pub use model::{LockedPackage, Lockfile};

/// Returns the current crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The first supported Tapid lockfile schema.
pub const LOCKFILE_VERSION: u32 = 1;
