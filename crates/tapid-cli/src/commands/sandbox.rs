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
                print!("{}", render_status(&status));
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

#[cfg(windows)]
fn render_status(status: &tapid_runner::WindowsNetworkGuardStatus) -> String {
    let state = if status.running {
        "running"
    } else if status.installed {
        "stopped"
    } else {
        "not installed"
    };
    let mut output = format!(
        "Windows network guard: {state}\nProtected executable: {}\n",
        status.executable.display()
    );
    if status.running {
        output.push_str(&format!(
            "Active executions: {}; retained denial filters: {}\n",
            status.active_executions, status.retained_filters
        ));
    } else if status.installed {
        output.push_str(&format!(
            "Retained denial filters: {}\n",
            status.retained_filters
        ));
    }
    output
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn stopped_guard_status_discloses_retained_denial_without_active_count() {
        let status = tapid_runner::WindowsNetworkGuardStatus {
            installed: true,
            running: false,
            active_executions: 0,
            retained_filters: 4,
            executable: "guard.exe".into(),
        };
        let output = render_status(&status);
        assert!(output.contains("Windows network guard: stopped"));
        assert!(output.contains("Retained denial filters: 4"), "{output}");
        assert!(!output.contains("Active executions"), "{output}");
    }
}
