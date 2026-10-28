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
    match report {
        Ok(report) => {
            println!("Audit: not checked; security provider unavailable");
            println!(
                "Recorded subjects: {} ({})",
                report.recorded_subjects, report.format
            );
            println!("Coverage: incomplete; advisory and reported-malware checks not performed");
            println!("Artifact analysis: not checked. Live requests: 0");
        }
        Err(code) => eprintln!("Audit failed: {code}"),
    }
    ExitCode::from(3)
}
