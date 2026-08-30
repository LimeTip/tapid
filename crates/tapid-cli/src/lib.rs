mod application;
mod commands;
mod context;
mod filesystem;
mod online;
mod output;
mod package_spec;
#[allow(dead_code)]
mod run;
mod transport;

pub fn run() -> std::process::ExitCode {
    application::run()
}
