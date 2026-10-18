use crate::application::tools::{self, Action, Options};
use clap::{Args as ClapArgs, Subcommand};
use std::{ffi::OsString, path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Separate durable tool installation directory and its bin directory.
    #[arg(long, global = true)]
    pub tools_dir: Option<PathBuf>,
    #[command(subcommand)]
    pub command: ToolCommand,
}
#[derive(Debug, Subcommand)]
pub(crate) enum ToolCommand {
    /// Install a standalone npm CLI without a project. Lifecycle scripts never run.
    Install(InstallArgs),
    /// List installed tools, locked versions, and exported commands.
    List,
    /// Update an installed tool within its recorded requirement.
    Update(InstallArgs),
    /// Remove a tool and its Tapid-owned commands.
    Remove { package: String },
}
#[derive(Debug, ClapArgs)]
pub(crate) struct InstallArgs {
    pub package: String,
    #[arg(long)]
    pub store_dir: Option<PathBuf>,
    #[arg(long)]
    pub registry_fixture: Option<PathBuf>,
}
#[derive(Debug, ClapArgs)]
pub(crate) struct ExecArgs {
    #[arg(long)]
    pub tools_dir: PathBuf,
    #[arg(long)]
    pub command: String,
    #[arg(last = true)]
    pub arguments: Vec<OsString>,
}
pub(crate) fn execute(args: ExecArgs) -> ExitCode {
    match tools::execute(&args.tools_dir, &args.command, &args.arguments) {
        Ok(code) => crate::output::child_exit_code(code),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    let (action, install) = match &args.command {
        ToolCommand::Install(a) => (Action::Install(a.package.as_str()), Some(a)),
        ToolCommand::Update(a) => (Action::Update(a.package.as_str()), Some(a)),
        ToolCommand::Remove { package } => (Action::Remove(package.as_str()), None),
        ToolCommand::List => (Action::List, None),
    };
    let options = Options {
        tools_dir: args.tools_dir.as_deref(),
        store_dir: install.and_then(|a| a.store_dir.as_deref()),
        registry_fixture: install.and_then(|a| a.registry_fixture.as_deref()),
    };
    match tools::run(action, options, |event| {
        crate::output::report_progress(event, json)
    }) {
        Ok(report) => {
            if json {
                return crate::output::json::tools(&report);
            }
            crate::output::report_warnings(&report.outcome.warnings);
            use crate::output::json::text;
            match &args.command {
                ToolCommand::Install(a) => println!("Installed {}", text(&a.package).0),
                ToolCommand::Update(a) => println!("Updated {}", text(&a.package).0),
                ToolCommand::Remove { package } => println!("Removed {}", text(package).0),
                ToolCommand::List => {}
            }
            for info in &report.tools {
                println!(
                    "{}@{} requirement={} ({})",
                    text(&info.name).0,
                    text(&info.tool.version).0,
                    text(&info.tool.requirement).0,
                    info.tool
                        .commands
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                println!("Environment: {}", text(info.environment.display()).0);
                println!(
                    "Lockfile: {}",
                    text(info.environment.join("tapid.lock").display()).0
                );
            }
            println!("Command directory: {}", text(report.bin_dir.display()).0);
            if matches!(
                report.outcome.state,
                crate::application::outcome::ChangeState::CommittedCleanupPending
            ) {
                eprintln!(
                    "warning: cleanup is pending; an environment is in use or could not be removed; retry a tool operation after it exits"
                );
            }
            if matches!(
                args.command,
                ToolCommand::Install(_) | ToolCommand::Update(_)
            ) {
                println!(
                    "Add this directory to PATH if needed. Tools run with your user permissions."
                );
                println!(
                    "Package installation does not complete tool-specific runtime setup; invoke the tool explicitly if it requires additional setup."
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => crate::output::json::failure_or_human(&e, "tool", json),
    }
}
