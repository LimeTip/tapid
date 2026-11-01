//! Disposable research executable; never a production containment backend.
fn main() {
    let _launcher = tapid_runner::initialize_or_dispatch_private_launcher();
    if let Err(error) = probe::run(std::env::args().skip(1).collect()) {
        eprintln!("probe setup failure: {error}");
        std::process::exit(2);
    }
}

#[path = "macos_managed_tree_probe/probe.rs"]
mod probe;
