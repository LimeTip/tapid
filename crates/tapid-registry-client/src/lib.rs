//! Read-only registry metadata clients with an injected HTTP boundary.
#![deny(unsafe_code)]

mod artifact;
mod client;
mod engines;
mod errors;
mod jsr;
mod models;
mod npm;
mod response;
mod transport;

pub use artifact::download_artifact;
pub use client::{RegistryClient, RegistryTransport};
pub use engines::NodeEngineRequirement;
pub use errors::{ClientError, MetadataError, RegistryClientError, TransportError};
pub use jsr::JsrRegistry;
pub use models::{
    FetchMode, PackageMetadata, RawPackageMetadata, RawRegistrySnapshot, RegistryArtifact,
    RegistryKind, RegistryPackageId, RegistrySnapshot,
};
pub use npm::{NpmPackageEvidence, NpmRegistry};
pub use transport::{HttpResponse, HttpTransport, HttpsTransport};

mod git;
pub use git::GitDependency;

pub use tapid_core::PackagePlatform;
