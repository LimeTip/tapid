use clap::{Args as ClapArgs, Subcommand};
use std::{fs, path::PathBuf, process::ExitCode};
use tapid_lockfile::Lockfile;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    Verify,
}

pub(crate) fn run(args: Args) -> ExitCode {
    match args.command {
        Command::Verify => {
            let path = PathBuf::from("tapid.lock");
            match fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|input| Lockfile::from_json(&input).map_err(|error| error.to_string()))
            {
                Ok(_) => {
                    println!("Valid lockfile: {}", path.display());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("error: cannot verify {}: {error}", path.display());
                    ExitCode::from(1)
                }
            }
        }
    }
}
