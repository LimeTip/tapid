use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Project directory containing package.json and tapid.lock.
    #[arg(long, default_value = ".", value_name = "PATH")]
    pub(crate) project_dir: PathBuf,
    /// Write the versioned JSON result to a new file outside the project.
    #[arg(long, value_name = "PATH", conflicts_with = "json")]
    pub(crate) result_file: Option<PathBuf>,
    /// Minimum advisory severity that fails CI; malware and unknown severity always fail.
    #[arg(long, value_enum, default_value = "any")]
    pub(crate) threshold: Threshold,
    /// Explicit provider configuration; never discovered in a project.
    #[arg(long, value_name = "PATH", requires_all = ["inventory_consent", "provider_trust", "replay_state", "credential_mode"])]
    pub(crate) provider_config: Option<PathBuf>,
    /// Separately authorized exact inventory disclosure document.
    #[arg(long, value_name = "PATH", requires = "provider_config")]
    pub(crate) inventory_consent: Option<PathBuf>,
    /// Independently provisioned issuer trust and freshness policy.
    #[arg(long, value_name = "PATH", requires = "provider_config")]
    pub(crate) provider_trust: Option<PathBuf>,
    /// Durable private security state outside the audited project.
    #[arg(long, value_name = "PATH", requires = "provider_config")]
    pub(crate) replay_state: Option<PathBuf>,
    /// Explicit credential source; headless never falls back to account credentials.
    #[arg(long, value_enum, requires = "provider_config")]
    pub(crate) credential_mode: Option<CredentialMode>,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum CredentialMode {
    Anonymous,
    Account,
    Headless,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum Threshold {
    Any,
    Low,
    Moderate,
    High,
    Critical,
}

impl From<Threshold> for tapid_policy::AuditThreshold {
    fn from(value: Threshold) -> Self {
        match value {
            Threshold::Any => Self::Any,
            Threshold::Low => Self::Low,
            Threshold::Moderate => Self::Moderate,
            Threshold::High => Self::High,
            Threshold::Critical => Self::Critical,
        }
    }
}

pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    let mut result_file = match args.result_file.as_deref() {
        Some(path) => match crate::filesystem::result_file::create(path, &args.project_dir) {
            Ok(file) => Some(file),
            Err(error) => {
                eprintln!("Audit failed: cannot create result file: {error}");
                return ExitCode::from(3);
            }
        },
        None => None,
    };
    let report = if let Some(config) = &args.provider_config {
        crate::application::audit::inspect_configured(
            &args.project_dir,
            config,
            args.inventory_consent
                .as_deref()
                .expect("clap requires consent"),
            args.provider_trust.as_deref().expect("clap requires trust"),
            args.replay_state
                .as_deref()
                .expect("clap requires replay state"),
            match args.credential_mode.expect("clap requires credential mode") {
                CredentialMode::Anonymous => tapid_security_provider::CredentialMode::Anonymous,
                CredentialMode::Account => tapid_security_provider::CredentialMode::Account,
                CredentialMode::Headless => tapid_security_provider::CredentialMode::Headless,
            },
        )
    } else {
        crate::application::audit::inspect(&args.project_dir)
    }
    .map(|report| report.with_threshold(args.threshold.into()));
    if json {
        return crate::output::json::audit(&args.project_dir, report);
    }
    let file_status = if let Some(file) = &mut result_file {
        let (value, status) = crate::output::json::audit_value(&args.project_dir, &report);
        if let Err(error) = crate::filesystem::result_file::write(file, &value) {
            eprintln!("Audit failed: cannot write result file: {error}");
            return ExitCode::from(3);
        }
        Some(status)
    } else {
        None
    };
    let mut status = 3;
    match report {
        Ok(report) => {
            status = report.outcome.exit_code();
            if report.evidence.is_empty() {
                println!("Audit: not checked; security provider unavailable");
            } else {
                println!(
                    "Audit: {}",
                    match report.outcome {
                        tapid_policy::AuditOutcome::BelowThreshold
                            if report.evidence.iter().all(|e| e.result.findings.is_empty()) =>
                            "no known findings within checked coverage",
                        tapid_policy::AuditOutcome::BelowThreshold => "findings below threshold",
                        tapid_policy::AuditOutcome::Findings => "findings",
                        tapid_policy::AuditOutcome::Incomplete => "incomplete",
                    }
                );
                let mut shown = 0;
                for evidence in &report.evidence {
                    for finding in &evidence.result.findings {
                        if shown == 100 {
                            break;
                        }
                        shown += 1;
                        let (line, shortened) = crate::output::json::text(format!(
                            "{} revision {}: {:?} {:?}; {}@{} as {} from {}; severity {}; fixes {}; issuer {}; checked {}; expires {}; status {:?}",
                            finding.id,
                            finding.revision,
                            finding.classification,
                            finding.state,
                            evidence.result.request.subject.package.name,
                            evidence.result.request.subject.package.version,
                            evidence.result.request.subject.requested_name,
                            evidence.result.request.subject.package.registry,
                            finding.severity.as_deref().unwrap_or("unknown"),
                            finding
                                .fixes
                                .iter()
                                .map(|f| f.version.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                            evidence.result.issuer,
                            evidence.result.checked_at,
                            evidence.result.expires_at,
                            evidence.result.status
                        ));
                        println!("{line}");
                        if let Some(path) = evidence.paths.first() {
                            let (path, path_shortened) = crate::output::json::text(
                                path.steps
                                    .iter()
                                    .map(|step| step.via.as_deref().unwrap_or(&step.key))
                                    .collect::<Vec<_>>()
                                    .join(" -> "),
                            );
                            println!("Example dependency path: {path}");
                            if path_shortened {
                                status = 3;
                            }
                        }
                        if evidence.paths_truncated {
                            println!("Dependency path search truncated");
                            status = 3;
                        }
                        if shortened {
                            status = 3;
                        }
                    }
                }
                if report
                    .evidence
                    .iter()
                    .map(|e| e.result.findings.len())
                    .sum::<usize>()
                    > 100
                {
                    println!("Findings display truncated at 100 reports");
                    status = 3;
                }
            }
            println!(
                "Recorded subjects: {} ({})",
                report.recorded_subjects, report.format
            );
            if let Some(count) = report.reachable_subjects {
                println!("Reachable subjects: {count}");
                if let Some(count) = report.requestable_occurrences {
                    println!(
                        "Representable security occurrences: {count}{}",
                        if report.evidence.is_empty() {
                            " (not checked)"
                        } else {
                            ""
                        }
                    );
                }
                if let Some(count) = report.excluded_occurrences.filter(|count| *count > 0) {
                    println!(
                        "Graph limitation: {count} occurrences lack exact security applicability"
                    );
                }
                if report.roots_recorded == Some(false) {
                    println!("Graph limitation: no recorded roots");
                }
                if let Some(count) = report.unresolved_peers.filter(|count| *count > 0) {
                    println!("Graph limitation: {count} unresolved peer identities");
                }
            }
            if report.evidence.is_empty() {
                println!(
                    "Coverage: incomplete; advisory and reported-malware checks not performed"
                );
            } else {
                println!("Coverage complete: {}", status != 3);
            }
            for failure in report.failures.iter().take(100) {
                println!("Security check failed: {}", failure.code);
            }
            println!("Unchecked occurrences: {}", report.not_checked);
            if report.evidence.is_empty() && report.failures.is_empty() {
                println!("Artifact analysis: not checked. Live requests: 0");
            } else {
                println!("Artifact analysis: not checked");
            }
        }
        Err(code) => eprintln!("Audit failed: {code}"),
    }
    ExitCode::from(if file_status == Some(3) { 3 } else { status })
}
