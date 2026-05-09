//! Main entrypoint for syntext.
//!
//! Exposes a command-line interface to index and search code.

#[cfg(not(target_arch = "wasm32"))]
#[cfg(not(windows))]
fn main() {
    let code = syntext::cli::run();
    std::process::exit(code);
}

#[cfg(all(not(target_arch = "wasm32"), windows))]
fn main() {
    // clap's generated parser for the rg-compatible CLI can exceed the default
    // Windows main-thread stack. Run the actual CLI on a modestly larger stack.
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(syntext::cli::run)
        .expect("spawn CLI thread");
    let code = match handle.join() {
        Ok(code) => code,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    std::process::exit(code);
}

#[cfg(target_arch = "wasm32")]
fn main() {}
