//! Authorized retrieval of verified security evidence.
#![deny(unsafe_code)]

mod configuration;
pub use configuration::{CredentialMode, InvalidProviderConfig, ProviderConfig};

mod http;
pub use http::HttpsProvider;

mod retrieval;
pub use retrieval::{Disclosure, RetrievalError, retrieve, retrieve_recorded};

mod collection;
pub use collection::{
    CollectionInput, CollectionLimit, MAX_COLLECTION_REQUESTS, SecurityCollection, collect_recorded,
};

#[cfg(any(unix, windows))]
mod replay;
#[cfg(any(unix, windows))]
pub use replay::ReplayStore;
