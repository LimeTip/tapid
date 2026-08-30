//! Pure, deterministic planning for project dependency materialization.
//!
//! The planner describes filesystem work; it deliberately does not create
//! links, mutate directories, execute processes, or claim to provide an OS
//! sandbox. Policy and runner crates consume the plan through the small types
//! in this crate's contract seam.

#![deny(unsafe_code)]

mod layout;
mod platform;
mod shims;

pub use layout::{
    ActivationStep, DependencyEdge, InstanceKey, LayoutInput, LinkKind, ManagedRoot,
    MaterializationEntry, MaterializationInput, MaterializationPlan, PackageInstance, PlanError,
    StagedActivationPlan, VerifiedTreeReference, plan_layout, plan_materialization,
};
pub use platform::{Capability, Platform, PlatformCapabilities};
pub use shims::{ShimEntry, ShimPackage, ShimPlan, ShimStrategy, plan_shims};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
