#![deny(unsafe_code, unsafe_op_in_unsafe_fn)]

mod application;
mod commands;
mod filesystem;
mod online;
mod output;
mod package_spec;
mod registry;
mod run;
mod transport;

pub fn run() -> std::process::ExitCode {
    application::run()
}
