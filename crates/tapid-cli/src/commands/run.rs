use clap::Args as ClapArgs;
use std::{fs, path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Root package script name.
    pub(crate) script: String,
    /// Project directory containing package.json.
    #[arg(long, default_value = ".")]
    pub(crate) project_dir: PathBuf,
    /// Arguments forwarded after `--` to the script.
    #[arg(last = true)]
    pub(crate) arguments: Vec<String>,
}

pub(crate) fn run(args: Args) -> ExitCode {
    let project_dir = match fs::canonicalize(&args.project_dir) {
        Ok(path) if path.is_dir() => path,
        Ok(path) => {
            eprintln!(
                "error: project directory is not a directory: {}",
                path.display()
            );
            return ExitCode::from(1);
        }
        Err(error) => {
            eprintln!(
                "error: cannot access project directory '{}': {error}",
                args.project_dir.display()
            );
            return ExitCode::from(1);
        }
    };
    let manifest = match super::manifest::read_manifest(&project_dir.join("package.json")) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let Some(script) = manifest.scripts().get(&args.script).cloned() else {
        eprintln!("error: root package script is missing: {}", args.script);
        return ExitCode::from(1);
    };
    match crate::run::execute(
        crate::run::RunRequest::new(project_dir, Some(script)).with_arguments(args.arguments),
    ) {
        Ok(result) => result
            .exit_code()
            .map_or(ExitCode::from(1), crate::output::child_exit_code),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}
