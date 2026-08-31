//! Read-only registry metadata clients with an injected HTTP boundary.
#![deny(unsafe_code)]

mod artifact;
mod client;
mod errors;
mod jsr;
mod models;
mod npm;
mod transport;

pub use client::{RegistryClient, RegistryTransport};
pub use errors::{ClientError, MetadataError, RegistryClientError, TransportError};
pub use jsr::JsrRegistry;
pub use models::{
    FetchMode, PackageMetadata, PackagePlatform, RawPackageMetadata, RawRegistrySnapshot,
    RegistryArtifact, RegistryKind, RegistryPackageId, RegistrySnapshot,
};
pub use npm::NpmRegistry;
pub use transport::{HttpResponse, HttpTransport, HttpsTransport};
