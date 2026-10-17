use clap::{Args as ClapArgs, Subcommand};
use std::process::ExitCode;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: SandboxCommand,
}
#[derive(Debug, Subcommand)]
enum SandboxCommand {
    /// Install and start the protected Windows network guard. Requires an administrator terminal once.
    Install,
    /// Inspect the Windows guard service and retained denial filters without changing settings.
    Status,
    /// Remove an idle Windows guard. Requires an administrator terminal and no retained filters.
    Uninstall,
}
pub(crate) fn run(args: Args) -> ExitCode {
    #[cfg(windows)]
    {
        use tapid_runner::{WindowsNetworkGuardAction as Action, windows_network_guard};
        let action = match args.command {
            SandboxCommand::Install => Action::Install,
            SandboxCommand::Status => Action::Status,
            SandboxCommand::Uninstall => Action::Uninstall,
        };
        match windows_network_guard(action) {
            Ok(status) => {
                println!(
                    "Windows network guard: {}",
                    if status.running {
                        "running"
                    } else if status.installed {
                        "stopped"
                    } else {
                        "not installed"
                    }
                );
                println!("Protected executable: {}", status.executable.display());
                if status.running {
                    println!(
                        "Active executions: {}; retained denial filters: {}",
                        status.active_executions, status.retained_filters
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = args.command;
        eprintln!(
            "error: administrator-installed network guard management is available on Windows only"
        );
        ExitCode::from(1)
    }
}
