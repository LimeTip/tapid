use super::{diagnostic, emit, envelope, recorded_text};
use serde_json::json;
use std::process::ExitCode;

pub(crate) fn explain(
    report: Result<
        &crate::application::explain::ExplainReport,
        &crate::application::outcome::OperationalError,
    >,
) -> ExitCode {
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            let mut result = envelope("explain", "failure", None);
            diagnostic::record(&mut result, error, "operation");
            return emit(result, 1);
        }
    };
    let mismatch = report.byte_integrity == crate::application::explain::ByteIntegrity::Mismatch;
    let mut result = envelope(
        "explain",
        if mismatch { "failure" } else { "success" },
        None,
    );
    let mut truncated = Vec::new();
    let evidence = &report.evidence;
    let source = recorded_text(&report.source, "/data/source", &mut truncated);
    let artifact = evidence
        .artifact_url
        .as_ref()
        .map(|url| recorded_text(url, "/data/artifact", &mut truncated));
    let attestation = evidence
        .attestation_url
        .as_ref()
        .map(|url| recorded_text(url, "/data/provenance/reference", &mut truncated));
    let name = recorded_text(&evidence.identity.name, "/data/package", &mut truncated);
    let version = recorded_text(&evidence.identity.version, "/data/version", &mut truncated);
    let registry = recorded_text(
        &evidence.identity.registry,
        "/data/registry",
        &mut truncated,
    );
    result["data"] = json!({
        "package": name,
        "version": version,
        "registry": registry,
        "source": source,
        "metadata_fetched_at": report.metadata_fetched_at,
        "metadata_read_at": report.metadata_read_at,
        "source_timestamp": evidence.modified_at,
        "source_timestamp_status": if evidence.modified_at.is_some() { "reported" } else { "missing" },
        "published_at": evidence.published_at,
        "freshness": "unknown",
        "artifact": artifact,
        "registry_integrity": {
            "status": if evidence.integrity.is_some() { "reported" } else { "missing" },
            "value": evidence.integrity.as_ref().map(ToString::to_string),
        },
        "byte_integrity": {"status": report.byte_integrity, "actual": report.actual_integrity, "scope": report.actual_integrity.as_ref().map(|_| "supplied_local_artifact"), "checked_at": report.bytes_checked_at},
        "registry_signatures": {"status": if evidence.signature_count.is_some() { "unverified" } else { "missing" }, "count": evidence.signature_count},
        "provenance": {"status": if evidence.attestation_url.is_some() { "unverified" } else { "missing" }, "reference": attestation, "fetched": false},
        "publisher_identity": {"status": "not_verified"},
        "vulnerabilities": {"status": "unavailable", "reason": "no_provider_queried"},
        "malware_analysis": {"status": "not_performed"},
        "human_review": {"status": "unavailable"},
        "limitations": ["Digest matches do not establish package safety, publisher identity, or intended content.", "Registry signatures and attestation references are not verified.", "Observation times and registry-reported timestamps do not establish evidence freshness."]
    });
    truncated.sort();
    result["truncated_fields"] = json!(truncated);
    super::compatibility::attach(&mut result, Some(&report.compatibility));
    if mismatch {
        let context = crate::application::outcome::DiagnosticContext::comparison(
            "artifact_integrity",
            Some("sha512"),
            evidence
                .integrity
                .as_ref()
                .expect("mismatch requires expected integrity"),
            report
                .actual_integrity
                .as_ref()
                .expect("mismatch requires observed integrity"),
        );
        let error = crate::application::outcome::OperationalError::new(
            crate::application::outcome::ErrorKind::Integrity,
            "integrity mismatch",
        )
        .with_diagnostic(context)
        .for_package((&evidence.identity).into());
        diagnostic::record(&mut result, &error, "operation");
    }
    emit(result, u8::from(mismatch))
}
