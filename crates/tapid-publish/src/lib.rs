//! Package packing and publishing foundations for Tapid.
//!
//! This crate is an experimental workspace boundary. Public APIs will be added
//! only when the corresponding Tapid behavior is implemented and tested.

#![deny(unsafe_code)]

/// Returns the current crate version.
///
/// This small API keeps the initial scaffold non-empty while the crate
/// boundary is being established.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_present() {
        assert!(!VERSION.is_empty());
    }
}
