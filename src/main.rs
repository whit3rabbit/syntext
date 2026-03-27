//! Main entrypoint for ripline.
//!
//! Exposes a command-line interface to index and search code.

fn main() {
    let code = ripline_rs::cli::run();
    std::process::exit(code);
}
