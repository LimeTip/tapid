use clap::Args as ClapArgs;
use clap_complete::{Shell, generate};
use std::{io, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Shell to generate completion for.
    #[arg(value_enum)]
    shell: Shell,
}

/// Generates static completions using the same Clap command tree as parsing and help.
pub(crate) fn run(args: Args) -> ExitCode {
    let mut output = Vec::new();
    generate(args.shell, &mut super::Cli::command(), "tapid", &mut output);
    match io::stdout().lock().write_all(&output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: cannot write shell completions: {error}");
            ExitCode::from(1)
        }
    }
}

use clap::CommandFactory;
use std::io::Write;
