use clap::{CommandFactory, ValueEnum};
use std::{io, io::Write, process::ExitCode};

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Shell whose static completion script should be generated.
    #[arg(value_enum)]
    shell: Shell,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

pub(super) fn run(args: Args) -> ExitCode {
    let shell = match args.shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
        Shell::Powershell => clap_complete::Shell::PowerShell,
    };
    let mut script = Vec::new();
    let mut command = visible_command(&super::Cli::command());
    clap_complete::generate(shell, &mut command, "tapid", &mut script);
    match emit(&script, &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: cannot print completions: {error}");
            ExitCode::from(1)
        }
    }
}

// The static generators do not consistently honor hidden commands. Project only
// completion metadata from the maintained tree; this tree is never used to parse.
fn visible_command(source: &clap::Command) -> clap::Command {
    let mut command = clap::Command::new(source.get_name().to_owned())
        .args(source.get_arguments().cloned())
        .groups(source.get_groups().cloned())
        .visible_aliases(source.get_visible_aliases().map(str::to_owned))
        .subcommands(
            source
                .get_subcommands()
                .filter(|child| !child.is_hide_set())
                .map(visible_command),
        );
    if let Some(about) = source.get_about() {
        command = command.about(about.clone());
    }
    command
}

fn emit(script: &[u8], writer: &mut impl Write) -> io::Result<()> {
    let script = std::str::from_utf8(script)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writer.write_all(script.as_bytes())?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Output {
        bytes: Vec<u8>,
        fail_after: Option<usize>,
        flush_error: bool,
        flushed: bool,
    }

    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_after == Some(self.bytes.len()) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let count = bytes.len().min(2).min(
                self.fail_after
                    .map_or(usize::MAX, |limit| limit - self.bytes.len()),
            );
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed = true;
            if self.flush_error {
                Err(io::ErrorKind::Other.into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn completions_emission_handles_short_writes_and_flushes() {
        let mut output = Output::default();
        emit(b"script", &mut output).unwrap();
        assert_eq!(output.bytes, b"script");
        assert!(output.flushed);
    }

    #[test]
    fn completions_emission_rejects_invalid_utf8_before_writing() {
        let mut output = Output::default();
        assert_eq!(
            emit(b"valid\xff", &mut output).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(output.bytes.is_empty());
        assert!(!output.flushed);
    }

    #[test]
    fn completions_emission_reports_broken_pipe_and_partial_failure() {
        for limit in [0, 3] {
            let mut output = Output {
                fail_after: Some(limit),
                ..Output::default()
            };
            assert_eq!(
                emit(b"script", &mut output).unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
            assert_eq!(output.bytes, b"script"[..limit]);
            assert!(!output.flushed);
        }
    }

    #[test]
    fn completions_emission_reports_flush_failure() {
        let mut output = Output {
            flush_error: true,
            ..Output::default()
        };
        assert_eq!(
            emit(b"script", &mut output).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        assert_eq!(output.bytes, b"script");
        assert!(output.flushed);
    }
}
