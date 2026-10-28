use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Project directory containing package.json and tapid.lock.
    #[arg(long, default_value = ".", value_name = "PATH")]
    pub(crate) project_dir: PathBuf,
}

pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    let report = crate::application::audit::inspect(&args.project_dir);
    if json {
        return crate::output::json::audit(&args.project_dir, report);
    }
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
    ExitCode::from(status)
}
