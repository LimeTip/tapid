use std::env;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        None | Some("--help") | Some("-h") => print_help(),
        Some("--version") | Some("-V") => println!("tapid {VERSION}"),
        Some(command) => {
            eprintln!("error: unknown command '{command}'");
            eprintln!("run 'tapid --help' for usage");
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "Tapid, a deterministic JavaScript and TypeScript package manager\n\nUsage:\n  tapid [OPTIONS]\n\nOptions:\n  -h, --help       Print help information\n  -V, --version    Print version information"
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn package_version_is_set() {
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
