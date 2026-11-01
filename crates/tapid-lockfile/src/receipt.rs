use serde::{Deserialize, Serialize};
use tapid_core::ArtifactDigest;

use crate::{LockedPackage, LockfileError};

/// Verified source trees bound to an exact selected package and archive.
/// This records verification evidence; constructing it does not verify an archive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceTreeReceipt {
    package: String,
    artifact_url: Option<String>,
    artifact_integrity: String,
    registry_integrity_declared: Option<bool>,
    unpacked_digest: String,
    tree_digest: String,
}

impl SourceTreeReceipt {
    pub fn new(
        package: &LockedPackage,
        unpacked_digest: &str,
        tree_digest: &str,
    ) -> Result<Self, LockfileError> {
        let receipt = Self::from_evidence(package, unpacked_digest, tree_digest);
        receipt.validate_binding(package)?;
        Ok(receipt)
    }

    pub(crate) fn from_evidence(
        package: &LockedPackage,
        unpacked_digest: &str,
        tree_digest: &str,
    ) -> Self {
        Self {
            package: package.key(),
            artifact_url: package.artifact_url().map(str::to_owned),
            artifact_integrity: package.artifact_integrity().to_owned(),
            registry_integrity_declared: package.registry_integrity_declared(),
            unpacked_digest: unpacked_digest.to_owned(),
            tree_digest: tree_digest.to_owned(),
        }
    }

    pub fn unpacked_digest(&self) -> &str {
        &self.unpacked_digest
    }

    pub fn tree_digest(&self) -> &str {
        &self.tree_digest
    }

    pub(crate) fn validate_binding(&self, package: &LockedPackage) -> Result<(), LockfileError> {
        for value in [&self.unpacked_digest, &self.tree_digest] {
            let digest: ArtifactDigest = value.parse().map_err(LockfileError::Domain)?;
            if digest.as_str() != value || !value.starts_with("sha256-") {
                return Err(LockfileError::InvalidSourceTreeReceipt(
                    "source trees require canonical SHA-256 digests".into(),
                ));
            }
        }
        if self.package != package.key()
            || self.artifact_url.as_deref() != package.artifact_url()
            || self.artifact_integrity != package.artifact_integrity()
            || self.registry_integrity_declared != package.registry_integrity_declared()
        {
            return Err(LockfileError::InvalidSourceTreeReceipt(
                "selected source or artifact evidence changed".into(),
            ));
        }
        Ok(())
    }
}

/// Normalizes schema-specific receipt fields for the shared graph validator.
pub(crate) fn decode_receipts(
    value: &mut serde_json::Value,
) -> Result<std::collections::BTreeMap<String, Option<SourceTreeReceipt>>, LockfileError> {
    let schema_11 =
        value["lockfileVersion"].as_u64() == Some(crate::PENDING_RECEIPT_LOCKFILE_VERSION.into());
    let mut receipts = std::collections::BTreeMap::new();
    if let Some(packages) = value["packages"].as_object_mut() {
        for (key, record) in packages {
            if schema_11 {
                if record.get("unpackedDigest").is_some() || record.get("treeDigest").is_some() {
                    return Err(LockfileError::InvalidSourceTreeReceipt(
                        "schema 11 forbids legacy digest fields".into(),
                    ));
                }
                let raw = record
                    .as_object_mut()
                    .and_then(|record| record.remove("sourceTreeReceipt"))
                    .ok_or_else(|| {
                        LockfileError::InvalidSourceTreeReceipt(
                            "schema 11 requires an explicit nullable receipt".into(),
                        )
                    })?;
                let receipt: Option<SourceTreeReceipt> =
                    serde_json::from_value(raw).map_err(LockfileError::Serialization)?;
                if let Some(receipt) = &receipt {
                    record["unpackedDigest"] = receipt.unpacked_digest().into();
                    record["treeDigest"] = receipt.tree_digest().into();
                }
                receipts.insert(key.clone(), receipt);
            } else if record.get("sourceTreeReceipt").is_some()
                || record
                    .get("unpackedDigest")
                    .is_none_or(serde_json::Value::is_null)
                || record
                    .get("treeDigest")
                    .is_none_or(serde_json::Value::is_null)
            {
                return Err(LockfileError::InvalidSourceTreeReceipt(
                    "legacy schemas require both tree digests and forbid sourceTreeReceipt".into(),
                ));
            }
        }
    }
    Ok(receipts)
}

/// Source-tree evidence is either absent or a complete pair, including in memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TreeEvidence {
    Pending,
    Verified { unpacked: String, tree: String },
}

impl TreeEvidence {
    pub(crate) fn digests(&self) -> Option<(&str, &str)> {
        match self {
            Self::Pending => None,
            Self::Verified { unpacked, tree } => Some((unpacked, tree)),
        }
    }

    pub(crate) fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

// Legacy fields remain together and in their original serialization order.
impl Serialize for TreeEvidence {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(if self.is_pending() { 0 } else { 2 }))?;
        if let Some((unpacked, tree)) = self.digests() {
            map.serialize_entry("unpackedDigest", unpacked)?;
            map.serialize_entry("treeDigest", tree)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for TreeEvidence {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            unpacked_digest: Option<String>,
            tree_digest: Option<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        match (wire.unpacked_digest, wire.tree_digest) {
            (None, None) => Ok(Self::Pending),
            (Some(unpacked), Some(tree)) => Ok(Self::Verified { unpacked, tree }),
            _ => Err(serde::de::Error::custom(
                "source-tree evidence requires both digests",
            )),
        }
    }
}
