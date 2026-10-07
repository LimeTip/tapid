use std::{io, io::Write, process::ExitCode};

const LICENSE: &str = include_str!(concat!(env!("OUT_DIR"), "/LICENSE"));

/// Prints the embedded license and copyright attribution without project access.
pub(crate) fn run() -> ExitCode {
    let mut stdout = io::stdout().lock();
    let result = stdout.write_all(LICENSE.as_bytes());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: cannot print license: {error}");
            ExitCode::from(1)
        }
    }
}
