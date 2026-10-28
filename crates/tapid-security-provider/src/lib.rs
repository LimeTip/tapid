//! Authorized retrieval of verified security evidence.
#![deny(unsafe_code)]

mod retrieval;
pub use retrieval::{Disclosure, RetrievalError, retrieve, retrieve_recorded};
