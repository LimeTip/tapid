//! Authorized retrieval of verified security evidence.
#![deny(unsafe_code)]

mod retrieval;
pub use retrieval::{Disclosure, RetrievalError, retrieve, retrieve_recorded};

mod collection;
pub use collection::{
    CollectionInput, CollectionLimit, MAX_COLLECTION_REQUESTS, SecurityCollection, collect_recorded,
};

#[cfg(unix)]
mod replay;
#[cfg(unix)]
pub use replay::ReplayStore;
