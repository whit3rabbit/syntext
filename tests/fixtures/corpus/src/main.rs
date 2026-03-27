// Main entry point for the ripline search engine
// TODO: add graceful shutdown handling

use std::env;
use std::process;

mod lib;
mod utils;
mod api;
mod models;

use crate::lib::SearchEngine;
use crate::models::config::Config;
use crate::models::user::User;

/// Application version
const APP_VERSION: &str = "0.1.0";

/// Default port for the API server
const DEFAULT_PORT: u16 = 8080;

fn main() {
    let config = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Configuration error: {}", e);
        process::exit(1);
    });

    let engine = SearchEngine::new(&config);

    // TODO: replace with proper logging framework
    println!("Starting ripline v{} on port {}", APP_VERSION, config.port);

    let admin = User {
        id: 1,
        name: String::from("admin"),
        email: String::from("admin@example.com"),
        role: models::user::Role::Admin,
    };

    if let Err(e) = engine.run(admin) {
        eprintln!("Engine failed: {}", e);
        process::exit(1);
    }
}

/// Parse command-line arguments into a config override map.
/// Supports --port, --index-path, and --threads.
fn parse_args() -> Vec<(String, String)> {
    let args: Vec<String> = env::args().collect();
    let mut overrides = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" if i + 1 < args.len() => {
                overrides.push(("port".into(), args[i + 1].clone()));
                i += 2;
            }
            "--threads" if i + 1 < args.len() => {
                overrides.push(("threads".into(), args[i + 1].clone()));
                i += 2;
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                i += 1;
            }
        }
    }

    overrides
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_args_empty() {
        let result = parse_args();
        assert!(result.is_empty());
    }
}
