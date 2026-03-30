//! Main entrypoint for syntext.
//!
//! Exposes a command-line interface to index and search code.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let code = syntext::cli::run();
    std::process::exit(code);
}

#[cfg(target_arch = "wasm32")]
fn main() {}
