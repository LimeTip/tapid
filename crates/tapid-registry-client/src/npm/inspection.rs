use super::*;
use tapid_core::PackageIntegrity;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Registry-reported evidence for one exact npm version.
/// None means the registry omitted that field. These claims are not verified
/// artifact bytes, signatures, or build provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmPackageEvidence {
    pub identity: RegistryPackageId,
    pub integrity: Option<PackageIntegrity>,
    pub artifact_url: Option<String>,
    pub signature_count: Option<usize>,
    pub attestation_url: Option<String>,
    /// Registry-reported publication time of the selected version, not verified.
    pub published_at: Option<String>,
    /// Registry-reported modification time of the package metadata, not verified.
    pub modified_at: Option<String>,
}

impl<T: HttpTransport> NpmRegistry<T> {
    /// Inspect full metadata without downloading artifacts or attestations.
    /// Unlike install metadata, absent integrity and tarball fields are retained.
    pub fn inspect(
        &self,
        package: &str,
        version: &PackageVersion,
    ) -> Result<NpmPackageEvidence, RegistryClientError> {
        let name: PackageName = package.parse().map_err(|_| {
            RegistryClientError::Metadata(MetadataError::InvalidPackageName(package.into()))
        })?;
        let url = format!("{}/{}", self.origin, package.replace('/', "%2F"));
        let response = self
            .transport
            .get_with_accept(&url, NPM_FULL_METADATA_ACCEPT)
            .map_err(RegistryClientError::Transport)?;
        NpmPackageEvidence::from_metadata(&self.origin, name, version, json_response(&response)?)
            .map_err(RegistryClientError::Metadata)
    }
}

impl NpmPackageEvidence {
    /// Validate an npm full-metadata snapshot for an exact registry identity.
    pub fn from_metadata(
        origin: &RegistryOrigin,
        name: PackageName,
        version: &PackageVersion,
        body: &[u8],
    ) -> Result<NpmPackageEvidence, MetadataError> {
        let root = json_object(body)?;
        if required_str(&root, "name")? != name.as_str() {
            return Err(MetadataError::ConflictingField("name".into()));
        }
        let versions = root
            .get("versions")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| MetadataError::MissingField("versions".into()))?;
        let key = version.to_string();
        let entry = versions
            .get(&key)
            .ok_or_else(|| MetadataError::MissingField(format!("versions.{key}")))?
            .as_object()
            .ok_or_else(|| MetadataError::InvalidJson("version entry must be an object".into()))?;
        if required_str(entry, "name")? != name.as_str() || required_str(entry, "version")? != key {
            return Err(MetadataError::ConflictingField("package identity".into()));
        }
        let timestamps = root
            .get("time")
            .map(|value| {
                value
                    .as_object()
                    .ok_or_else(|| MetadataError::InvalidJson("time must be an object".into()))
            })
            .transpose()?;
        let mut evidence = NpmPackageEvidence {
            identity: RegistryPackageId::new(origin.clone(), name, version.clone()),
            integrity: None,
            artifact_url: None,
            signature_count: None,
            attestation_url: None,
            published_at: reported_timestamp(timestamps, &key)?,
            modified_at: reported_timestamp(timestamps, "modified")?,
        };
        let Some(dist) = entry.get("dist") else {
            return Ok(evidence);
        };
        let dist = dist
            .as_object()
            .ok_or_else(|| MetadataError::InvalidJson("dist must be an object".into()))?;
        if dist.contains_key("integrity") {
            let value = required_str(dist, "integrity")?;
            evidence.integrity = Some(
                value
                    .parse()
                    .map_err(|_| MetadataError::InvalidIntegrity(value.into()))?,
            );
        }
        if dist.contains_key("tarball") {
            evidence.artifact_url = Some(evidence_url(required_str(dist, "tarball")?)?);
        }
        if let Some(signatures) = dist.get("signatures") {
            let signatures = signatures
                .as_array()
                .ok_or_else(|| MetadataError::InvalidJson("signatures must be an array".into()))?;
            for signature in signatures {
                let signature = signature.as_object().ok_or_else(|| {
                    MetadataError::InvalidJson("signature must be an object".into())
                })?;
                required_str(signature, "keyid")?;
                required_str(signature, "sig")?;
            }
            evidence.signature_count = Some(signatures.len());
        }
        if let Some(attestations) = dist.get("attestations") {
            let attestations = attestations.as_object().ok_or_else(|| {
                MetadataError::InvalidJson("attestations must be an object".into())
            })?;
            evidence.attestation_url = Some(evidence_url(required_str(attestations, "url")?)?);
        }
        Ok(evidence)
    }
}

fn reported_timestamp(
    timestamps: Option<&serde_json::Map<String, serde_json::Value>>,
    key: &str,
) -> Result<Option<String>, MetadataError> {
    let Some(value) = timestamps.and_then(|timestamps| timestamps.get(key)) else {
        return Ok(None);
    };
    let invalid =
        || MetadataError::InvalidJson(format!("time.{key} must be an RFC 3339 timestamp"));
    let value = value.as_str().ok_or_else(invalid)?;
    let timestamp = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| invalid())?;
    timestamp.format(&Rfc3339).map(Some).map_err(|_| invalid())
}

fn evidence_url(value: &str) -> Result<String, MetadataError> {
    let url = Url::parse(value).map_err(|_| MetadataError::InvalidArtifact(value.into()))?;
    if !request_url_is_safe(value, &url) {
        return Err(MetadataError::InvalidArtifact(value.into()));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspect(entry: serde_json::Value) -> Result<NpmPackageEvidence, RegistryClientError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "name": "foo", "versions": {"1.2.3": entry}
        }))
        .unwrap();
        let mut transport = super::super::tests::fake(&body, "https://registry.npmjs.org/foo");
        transport.expected_accept = Some(NPM_FULL_METADATA_ACCEPT);
        NpmRegistry::new(transport, "https://registry.npmjs.org".parse().unwrap())
            .inspect("foo", &"1.2.3".parse().unwrap())
    }

    #[test]
    fn inspection_retains_missing_evidence() {
        let evidence = inspect(serde_json::json!({"name":"foo", "version":"1.2.3"})).unwrap();
        assert!(evidence.integrity.is_none());
        assert!(evidence.artifact_url.is_none());
        assert!(evidence.signature_count.is_none());
        assert!(evidence.attestation_url.is_none());
    }

    #[test]
    fn inspection_reports_only_selected_version_and_metadata_timestamps() {
        let origin = "https://registry.npmjs.org".parse().unwrap();
        let body = serde_json::json!({
            "name":"foo", "versions":{"1.2.3":{"name":"foo","version":"1.2.3"}},
            "time":{"1.2.3":"2020-01-02T03:04:05.000Z", "modified":"2026-10-01T12:30:00Z", "other-version":"malformed"}
        });
        let evidence = NpmPackageEvidence::from_metadata(
            &origin,
            "foo".parse().unwrap(),
            &"1.2.3".parse().unwrap(),
            &serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        assert_eq!(
            evidence.published_at.as_deref(),
            Some("2020-01-02T03:04:05Z")
        );
        assert_eq!(
            evidence.modified_at.as_deref(),
            Some("2026-10-01T12:30:00Z")
        );
        let mut missing = body;
        missing.as_object_mut().unwrap().remove("time");
        let evidence = NpmPackageEvidence::from_metadata(
            &origin,
            "foo".parse().unwrap(),
            &"1.2.3".parse().unwrap(),
            &serde_json::to_vec(&missing).unwrap(),
        )
        .unwrap();
        assert!(evidence.published_at.is_none());
        assert!(evidence.modified_at.is_none());
    }

    #[test]
    fn inspection_rejects_malformed_reported_timestamps() {
        for time in [
            serde_json::json!(null),
            serde_json::json!({"1.2.3":null}),
            serde_json::json!({"1.2.3":"2026-02-30T00:00:00Z"}),
            serde_json::json!({"modified":"2026-10-01"}),
            serde_json::json!({"modified":"2026-10-01T12:30:00Z\nforged fact"}),
        ] {
            let body = serde_json::json!({"name":"foo","versions":{"1.2.3":{"name":"foo","version":"1.2.3"}},"time":time});
            assert!(
                NpmPackageEvidence::from_metadata(
                    &"https://registry.npmjs.org".parse().unwrap(),
                    "foo".parse().unwrap(),
                    &"1.2.3".parse().unwrap(),
                    &serde_json::to_vec(&body).unwrap()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn inspection_reports_registry_claims_without_verification() {
        let evidence = inspect(serde_json::json!({"name":"foo", "version":"1.2.3", "dist": {
            "integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
            "tarball":"https://registry.npmjs.org/foo/-/foo-1.2.3.tgz",
            "signatures":[{"keyid":"example", "sig":"unverified"}],
            "attestations":{"url":"https://registry.npmjs.org/-/npm/v1/attestations/foo@1.2.3"}
        }})).unwrap();
        assert!(evidence.integrity.is_some());
        assert_eq!(evidence.signature_count, Some(1));
        assert!(evidence.attestation_url.is_some());
    }

    #[test]
    fn inspection_rejects_conflicting_identity_and_malformed_evidence() {
        for entry in [
            serde_json::json!({"name":"other", "version":"1.2.3"}),
            serde_json::json!({"name":"foo", "version":"9.9.9"}),
            serde_json::json!({"name":"foo", "version":"1.2.3", "dist":{"integrity":"sha512-bad"}}),
            serde_json::json!({"name":"foo", "version":"1.2.3", "dist":{"signatures":[{}]}}),
            serde_json::json!({"name":"foo", "version":"1.2.3", "dist":{"attestations":{"url":"http://example.com"}}}),
            serde_json::json!({"name":"foo", "version":"1.2.3", "dist":{"tarball":"https://user:secret@example.com/a"}}),
        ] {
            assert!(inspect(entry).is_err());
        }
    }
}
