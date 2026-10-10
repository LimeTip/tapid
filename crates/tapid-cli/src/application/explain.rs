use super::outcome::{ErrorKind, OperationalError};
use std::{collections::BTreeMap, path::Path};
use tapid_core::PackageVersion;
use tapid_registry_client::{NpmPackageEvidence, NpmRegistry, RegistryClientError};

#[derive(Clone, Copy, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ByteIntegrity {
    NotChecked,
    VerifiedMatch,
    Mismatch,
    MissingExpectedIntegrity,
}

pub(crate) struct ExplainReport {
    pub(crate) evidence: NpmPackageEvidence,
    pub(crate) source: String,
    pub(crate) byte_integrity: ByteIntegrity,
    pub(crate) actual_integrity: Option<String>,
}

pub(crate) fn explain(
    spec: &str,
    project: &Path,
    metadata_file: Option<&Path>,
    artifact_file: Option<&Path>,
) -> Result<ExplainReport, OperationalError> {
    let (package, version) = crate::package_spec::parse(spec);
    let version: PackageVersion = version.parse().map_err(|_| {
        OperationalError::new(
            ErrorKind::InvalidRequest,
            "explain requires <package>@<exact-version>",
        )
    })?;
    if package.starts_with("jsr:") {
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "explain currently supports npm packages only",
        ));
    }
    let config = crate::registry::RegistryConfig::load(project)
        .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?;
    let (origin, name) = config
        .identity_for_spec(package)
        .map_err(|error| OperationalError::new(ErrorKind::InvalidRequest, error))?;
    let (evidence, source) = if let Some(path) = metadata_file {
        let body = crate::commands::run::read_bounded_config_file(path, 32 * 1024 * 1024).map_err(
            |_| {
                OperationalError::new(
                    ErrorKind::RegistryMetadata,
                    "cannot read metadata snapshot as a regular file within 32 MiB",
                )
            },
        )?;
        let evidence = NpmPackageEvidence::from_metadata(&origin, name, &version, &body)
            .map_err(|error| OperationalError::from_source(ErrorKind::RegistryMetadata, error))?;
        (
            evidence,
            format!("local metadata snapshot {}", path.display()),
        )
    } else {
        let mut cache = BTreeMap::new();
        let transport = crate::online::metadata_transport_for_package(
            &mut cache,
            &config,
            &origin,
            &name,
            &config.configured_origins(),
        )?;
        let evidence = NpmRegistry::new(transport, origin.clone())
            .inspect(name.as_str(), &version)
            .map_err(|error| {
                let kind = match &error {
                    RegistryClientError::Transport(_) => ErrorKind::RegistryTransport,
                    RegistryClientError::Metadata(_) => ErrorKind::RegistryMetadata,
                };
                OperationalError::from_source(kind, error)
            })?;
        (
            evidence,
            format!("{}/{}", origin, name.as_str().replace('/', "%2F")),
        )
    };
    let mut report = ExplainReport {
        evidence,
        source,
        byte_integrity: ByteIntegrity::NotChecked,
        actual_integrity: None,
    };
    if let Some(path) = artifact_file {
        let bytes = crate::commands::run::read_bounded_config_file(path, 512 * 1024 * 1024)
            .map_err(|_| {
                OperationalError::new(
                    ErrorKind::InvalidData,
                    "cannot read artifact as a regular file within 512 MiB",
                )
            })?;
        let actual = crate::online::integrity(&bytes);
        report.byte_integrity = match &report.evidence.integrity {
            Some(expected) if *expected == actual => ByteIntegrity::VerifiedMatch,
            Some(_) => ByteIntegrity::Mismatch,
            None => ByteIntegrity::MissingExpectedIntegrity,
        };
        report.actual_integrity = Some(actual.to_string());
    }
    Ok(report)
}
