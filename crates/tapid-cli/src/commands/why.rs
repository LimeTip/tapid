use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};
use tapid_lockfile::{LockfilePackageKey, LockfilePackageSource};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Package name to explain, including scope when applicable.
    #[arg(value_name = "PACKAGE")]
    pub(crate) package: String,
    /// Project directory containing package.json and tapid.lock.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub(crate) project_dir: PathBuf,
    /// Select a workspace member by name.
    #[arg(long, value_name = "NAME")]
    pub(crate) workspace: Option<String>,
}

pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    match crate::application::why::explain(
        &args.project_dir,
        args.workspace.as_deref(),
        &args.package,
    ) {
        Ok(report) => {
            if json {
                return crate::output::json::why_success(&report, &args.project_dir);
            }
            if report.paths.is_empty() {
                println!(
                    "No path to '{}' was found in the resolved dependency graph.",
                    report.package
                );
                if report.truncated {
                    println!("The search reached a safety bound; this result may be incomplete.");
                }
            } else {
                println!("Dependency paths to {}:", report.package);
                for (index, path) in report.paths.iter().enumerate() {
                    let mut line = String::from("project root");
                    for step in &path.steps {
                        let kind = step.edge_kind.as_deref().unwrap_or("dependency");
                        let via = step.via.as_deref().unwrap_or("?");
                        line.push_str(&format!(" --{kind} ({via})--> {}", display_key(&step.key)));
                    }
                    println!("[{}] {line}", index + 1);
                }
                for warning in &report.warnings {
                    println!("warning: {warning}");
                }
                if report.truncated {
                    println!("Additional paths were omitted or traversal reached a safety bound.");
                }
            }
            if report.paths.is_empty() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            if json {
                return crate::output::json::why_failure(&args.project_dir, error.kind.code());
            }
            eprintln!("error: {error}");
            eprintln!("diagnostic: {}", error.kind.code());
            ExitCode::from(1)
        }
    }
}

fn display_key(encoded: &str) -> String {
    let Ok(key) = encoded.parse::<LockfilePackageKey>() else {
        return encoded.to_owned();
    };
    let peer = if key.peer_context.is_empty() {
        String::from("-")
    } else {
        key.peer_context.clone()
    };
    let platform = if key.platform_context.is_empty() {
        String::from("-")
    } else {
        key.platform_context.clone()
    };
    match key.source {
        LockfilePackageSource::Registry(origin) => {
            format!(
                "{}@{} [{}; peer={peer}; platform={platform}]",
                key.name, key.version, origin
            )
        }
        LockfilePackageSource::Workspace(source) => {
            format!(
                "{}@{} [workspace:{}; peer={peer}; platform={platform}]",
                key.name,
                key.version,
                source.path()
            )
        }
    }
}
