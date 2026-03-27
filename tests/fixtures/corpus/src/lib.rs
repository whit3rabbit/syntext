// Core search engine library
// FIXME: the index rebuild path is not thread-safe

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub mod utils;
pub mod api;
pub mod models;

use crate::models::config::Config;
use crate::models::user::User;

/// Errors that the search engine can produce.
#[derive(Debug)]
pub enum EngineError {
    IndexCorrupted(String),
    QueryParseFailed(String),
    IoError(std::io::Error),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::IndexCorrupted(msg) => write!(f, "index corrupted: {}", msg),
            EngineError::QueryParseFailed(msg) => write!(f, "parse_query failed: {}", msg),
            EngineError::IoError(e) => write!(f, "io error: {}", e),
        }
    }
}

impl From<std::io::Error> for EngineError {
    fn from(err: std::io::Error) -> Self {
        EngineError::IoError(err)
    }
}

/// Primary search engine holding the index and config.
pub struct SearchEngine {
    config: Config,
    index: Arc<RwLock<HashMap<String, Vec<PathBuf>>>>,
}

impl SearchEngine {
    pub fn new(config: &Config) -> Self {
        SearchEngine {
            config: config.clone(),
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Run the engine, binding to the configured port.
    pub fn run(&self, admin: User) -> Result<(), EngineError> {
        // TODO: implement actual server loop
        println!("Engine running as user: {}", admin.name);
        Ok(())
    }

    /// Rebuild the entire search index from disk.
    pub fn rebuild_index(&self) -> Result<usize, EngineError> {
        let mut idx = self.index.write().map_err(|_| {
            EngineError::IndexCorrupted("lock poisoned".into())
        })?;
        idx.clear();
        // Walk config.index_path and re-index
        Ok(idx.len())
    }

    /// Execute a search query against the index.
    /// Delegates to utils::parser::parse_query for query parsing.
    pub fn search(&self, raw_query: &str) -> Result<Vec<PathBuf>, EngineError> {
        let _parsed = crate::utils::parser::parse_query(raw_query)?;
        let idx = self.index.read().map_err(|_| {
            EngineError::IndexCorrupted("lock poisoned".into())
        })?;
        // Placeholder: return empty results
        Ok(Vec::new())
    }
}
