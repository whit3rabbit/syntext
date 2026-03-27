// Configuration model
// Loaded from environment variables or a config file.

use std::path::PathBuf;

/// Maximum number of threads the engine can use.
pub const MAX_THREAD_COUNT: usize = 64;

/// Default index directory name.
pub const DEFAULT_INDEX_DIR: &str = ".ripline_index";

/// PARSE_QUERY timeout in milliseconds.
pub const PARSE_QUERY_TIMEOUT_MS: u64 = 5000;

/// Maximum file size to index (10 MB).
pub const MAX_FILE_SIZE_BYTES: u64 = 10_485_760;

/// Default batch size for process_batch operations.
pub const DEFAULT_BATCH_SIZE: usize = 256;

/// Application configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub index_path: PathBuf,
    pub threads: usize,
    pub max_results: usize,
    pub log_level: LogLevel,
}

/// Supported log levels.
#[derive(Debug, Clone, PartialEq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl Config {
    /// Load configuration from environment variables.
    /// Falls back to sensible defaults for unset variables.
    pub fn from_env() -> Result<Self, String> {
        let port = std::env::var("RIPLINE_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8080);

        let index_path = std::env::var("RIPLINE_INDEX_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_INDEX_DIR));

        let threads = std::env::var("RIPLINE_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);

        if threads > MAX_THREAD_COUNT {
            return Err(format!(
                "thread count {} exceeds maximum {}",
                threads, MAX_THREAD_COUNT
            ));
        }

        Ok(Config {
            port,
            index_path,
            threads,
            max_results: 100,
            log_level: LogLevel::Info,
        })
    }

    /// Check if the config is valid.
    pub fn validate(&self) -> Result<(), String> {
        if self.port == 0 {
            return Err("port must be nonzero".into());
        }
        if self.threads == 0 {
            return Err("threads must be nonzero".into());
        }
        Ok(())
    }
}
