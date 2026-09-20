fn main() -> std::process::ExitCode {
    let _launcher = tapid_runner::initialize_or_dispatch_private_launcher();
    tapid::run()
}
