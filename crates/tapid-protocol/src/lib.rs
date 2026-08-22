//! Wire-facing contracts for Tapid package identity.
//!
//! These DTOs intentionally keep transport strings separate from the validated
//! domain types in `tapid-core`.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use tapid_core::{PackageInstanceId, PackageName, PackageVersion, RegistryOrigin};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInstanceWire {
    pub registry: String,
    pub name: String,
    pub version: String,
}

impl From<&PackageInstanceId> for PackageInstanceWire {
    fn from(value: &PackageInstanceId) -> Self {
        Self {
            registry: value.registry.to_string(),
            name: value.name.to_string(),
            version: value.version.to_string(),
        }
    }
}

impl TryFrom<PackageInstanceWire> for PackageInstanceId {
    type Error = tapid_core::DomainError;

    fn try_from(value: PackageInstanceWire) -> Result<Self, Self::Error> {
        Ok(Self::new(
            value.registry.parse::<RegistryOrigin>()?,
            value.name.parse::<PackageName>()?,
            value.version.parse::<PackageVersion>()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_round_trip_validates_at_boundary() {
        let domain = PackageInstanceId::new(
            "https://registry.example".parse().unwrap(),
            "tapid".parse().unwrap(),
            "1.0.0".parse().unwrap(),
        );
        let wire = PackageInstanceWire::from(&domain);
        assert_eq!(PackageInstanceId::try_from(wire).unwrap(), domain);
    }

    #[test]
    fn wire_rejects_untrusted_identity_strings() {
        let wire = PackageInstanceWire {
            registry: "file:///tmp".into(),
            name: "../escape".into(),
            version: "latest".into(),
        };
        assert!(PackageInstanceId::try_from(wire).is_err());
    }
}
