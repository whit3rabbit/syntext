//! Main entrypoint for syntext.
//!
//! Exposes a command-line interface to index and search code.

fn main() {
    let code = syntext::cli::run();
    std::process::exit(code);
}
