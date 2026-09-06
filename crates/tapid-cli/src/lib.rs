mod application;
mod commands;
mod context;
mod filesystem;
mod online;
mod output;
mod package_spec;
mod run;

pub fn run() -> std::process::ExitCode {
    application::run()
}
