use crate::application::explain::ByteIntegrity;
use crate::output::json::text;
use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Exact npm package version, including scoped names.
    pub(crate) package: String,
    /// Directory whose tapid.toml supplies registry routing. No manifest is required.
    #[arg(long, default_value = ".")]
    pub(crate) project_dir: PathBuf,
    /// Read a local full npm metadata snapshot instead of contacting the registry.
    #[arg(long)]
    pub(crate) registry_metadata: Option<PathBuf>,
    /// Check a local artifact's bytes against the registry-declared SHA-512 integrity.
    #[arg(long)]
    pub(crate) artifact_file: Option<PathBuf>,
}

pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    let report = match crate::application::explain::explain(
        &args.package,
        &args.project_dir,
        args.registry_metadata.as_deref(),
        args.artifact_file.as_deref(),
    ) {
        Ok(report) => report,
        Err(error) => {
            if json {
                return crate::output::json::explain(Err(&error));
            }
            eprintln!("error: {error}");
            eprintln!("diagnostic: {}", error.kind.code());
            return ExitCode::from(1);
        }
    };
    if json {
        return crate::output::json::explain(Ok(&report));
    }
    let evidence = &report.evidence;
    println!(
        "{}@{}",
        evidence.identity.name,
        text(&evidence.identity.version).0
    );
    println!("Registry: {}", evidence.identity.registry);
    println!("Evidence source: {}", text(&report.source).0);
    println!("Evidence source timestamp: unavailable; freshness is unknown");
    match &evidence.artifact_url {
        Some(url) => println!("Registry-reported artifact: {}", text(url).0),
        None => println!("Registry-reported artifact: missing"),
    }
    match &evidence.integrity {
        Some(integrity) => println!("Registry-reported integrity: {integrity}"),
        None => println!("Registry-reported integrity: missing"),
    }
    println!(
        "Byte integrity: {}",
        match report.byte_integrity {
            ByteIntegrity::VerifiedMatch => "verified match for the supplied local artifact",
            ByteIntegrity::Mismatch => "MISMATCH for the supplied local artifact",
            ByteIntegrity::MissingExpectedIntegrity =>
                "unavailable; registry-declared integrity is missing",
            ByteIntegrity::NotChecked => "not checked; use --artifact-file to check local bytes",
        }
    );
    if let Some(actual) = &report.actual_integrity {
        println!("Observed local artifact integrity: {actual}");
    }
    match evidence.signature_count {
        Some(count) => println!("Registry signatures: {count} reported, unverified"),
        None => println!("Registry signatures: missing; not verified"),
    }
    match &evidence.attestation_url {
        Some(url) => println!(
            "Provenance: registry-reported attestation reference {}, unverified; not fetched",
            text(url).0
        ),
        None => println!("Provenance: missing attestation reference; not verified"),
    }
    println!("Publisher identity: not verified");
    println!("Vulnerabilities: unavailable; no vulnerability provider queried, status unknown");
    println!("Malware analysis: not performed");
    println!("Human review: unavailable");
    println!(
        "A digest match does not establish package safety, publisher identity, or intended content."
    );
    if report.byte_integrity == ByteIntegrity::Mismatch {
        eprintln!("diagnostic: INTEGRITY_MISMATCH");
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
