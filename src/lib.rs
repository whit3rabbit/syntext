//! Hybrid code search index for agent workflows.
//!
//! syntext indexes repository files using sparse n-grams with a pre-trained
//! frequency weight table, then narrows queries to a small candidate set
//! before verification. Three index components:
//!
//! - **Content index**: sparse n-gram posting lists (delta-varint or Roaring bitmap)
//! - **Path index**: Roaring bitmap component sets for path/type scoping
//! - **Symbol index** (optional, `--features symbols`): Tree-sitter + SQLite
//!
//! # Usage
//!
//! ```no_run
//! use syntext::{Config, SearchOptions};
//! use syntext::index::Index;
//!
//! let config = Config {
//!     index_dir: ".syntext".into(),
//!     repo_root: ".".into(),
//!     ..Config::default()
//! };
//!
//! // Build or open the index.
//! let index = Index::build(config).unwrap();
//!
//! // Search with default options.
//! let results = index.search("parse_query", &SearchOptions::default()).unwrap();
//! for m in &results {
//!     println!("{}:{}: {}", m.path.display(), m.line_number,
//!         String::from_utf8_lossy(&m.line_content));
//! }
//! ```

pub mod base64;
#[cfg(not(target_arch = "wasm32"))]
pub mod cli;
pub mod index;
pub mod path;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod git_util;
pub(crate) mod path_util;
pub mod posting;
pub mod query;
pub mod search;
#[cfg(feature = "symbols")]
pub mod symbol;
pub mod tokenizer;
#[cfg(feature = "wasm")]
pub mod wasm;

use std::path::PathBuf;

/// Configuration for index building and searching.
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum file size to index (bytes). Default: 10MB.
    pub max_file_size: u64,
    /// Maximum segments before triggering background merge. Default: 10.
    pub max_segments: usize,
    /// Path to index directory.
    pub index_dir: PathBuf,
    /// Repository root path.
    pub repo_root: PathBuf,
    /// Emit progress messages to stderr. Default: false (silent for library consumers).
    pub verbose: bool,
    /// Reject index directories with group/other permission bits (unix only).
    /// Permissive modes allow SIGBUS DoS via concurrent ftruncate on mmap'd
    /// segment files. Default: true.
    pub strict_permissions: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_file_size: 10 * 1024 * 1024,
            max_segments: 10,
            index_dir: PathBuf::from(".syntext"),
            repo_root: PathBuf::from("."),
            verbose: false,
            strict_permissions: true,
        }
    }
}

/// A single line-level match returned by a search query.
#[derive(Debug, Clone)]
pub struct SearchMatch {
    /// Repository-relative path of the matching file.
    pub path: PathBuf,
    /// 1-based line number of the match within the file.
    pub line_number: u32,
    /// Full bytes of the matching line (without trailing newline).
    pub line_content: Vec<u8>,
    /// Absolute byte offset of the start of the first match within the file.
    pub byte_offset: u64,
    /// Byte offset of the first match within `line_content`.
    pub submatch_start: usize,
    /// Exclusive end byte offset of the first match within `line_content`.
    pub submatch_end: usize,
}

/// Search options.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Glob pattern to restrict search to matching paths.
    pub path_filter: Option<String>,
    /// File type filter (e.g., "rs", "py").
    pub file_type: Option<String>,
    /// Exclude files of this type from results.
    pub exclude_type: Option<String>,
    /// Maximum number of results.
    pub max_results: Option<usize>,
    /// Enable case-insensitive matching.
    pub case_insensitive: bool,
}

/// Counters and metadata reported by `Index::stats()`.
#[derive(Debug, Clone)]
pub struct IndexStats {
    /// Number of indexed files across all base segments.
    pub total_documents: usize,
    /// Number of on-disk SNTX segment files.
    pub total_segments: usize,
    /// Total distinct n-grams across all segments.
    pub total_grams: usize,
    /// Combined on-disk size of all segment files plus the manifest (bytes).
    pub index_size_bytes: u64,
    /// Git commit SHA the base segments were built against, if known.
    pub base_commit: Option<String>,
    /// Number of overlay generations since the last full rebuild.
    pub overlay_generations: usize,
    /// Number of dirty file edits buffered in the current overlay.
    pub pending_edits: usize,
}

/// Errors returned by index operations.
#[derive(Debug)]
#[must_use]
pub enum IndexError {
    /// I/O error (file not found, permission denied, etc.)
    Io(std::io::Error),
    /// Invalid regex pattern.
    InvalidPattern(String),
    /// Index is corrupt and needs rebuilding.
    CorruptIndex(String),
    /// Path is outside the repository root.
    PathOutsideRepo(PathBuf),
    /// File exceeds maximum indexable size.
    FileTooLarge {
        /// Path to the file.
        path: PathBuf,
        /// Size of the file in bytes.
        size: u64,
    },
    /// Another process holds a conflicting lock on the index directory.
    LockConflict(PathBuf),
    /// Overlay has grown too large relative to the base index.
    /// Call `Index::build()` to perform a full reindex.
    OverlayFull {
        /// Current number of overlay documents.
        overlay_docs: usize,
        /// Number of base documents at the time of the check.
        base_docs: usize,
    },
    /// Document ID space exceeded `u32::MAX`.
    DocIdOverflow {
        /// Number of base documents already allocated.
        base_doc_count: u32,
        /// Number of overlay documents requested on top of the base.
        overlay_docs: usize,
    },
}

impl From<std::io::Error> for IndexError {
    fn from(err: std::io::Error) -> Self {
        IndexError::Io(err)
    }
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Io(e) => write!(f, "I/O error: {e}"),
            IndexError::InvalidPattern(p) => write!(f, "invalid pattern: {p}"),
            IndexError::CorruptIndex(msg) => write!(f, "corrupt index: {msg}"),
            IndexError::PathOutsideRepo(p) => {
                // Use only the last path component to avoid leaking absolute
                // filesystem layout in library/server contexts where this error
                // may be forwarded to an untrusted caller.
                let name = p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                write!(f, "path outside repo: {name}")
            }
            IndexError::FileTooLarge { path, size } => {
                // Same rationale: show filename only, not the full absolute path.
                let name =
                    path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                write!(f, "file too large: {name} ({size} bytes)")
            }
            IndexError::LockConflict(p) => {
                write!(f, "index locked by another process: {}", p.display())
            }
            IndexError::OverlayFull { overlay_docs, base_docs } => write!(
                f,
                "overlay too large ({overlay_docs} overlay docs, {base_docs} base docs): \
                 run `st index` to rebuild"
            ),
            IndexError::DocIdOverflow {
                base_doc_count,
                overlay_docs,
            } => write!(
                f,
                "doc_id overflow: base {base_doc_count} docs plus {overlay_docs} overlay docs exceeds u32::MAX"
            ),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexError::Io(e) => Some(e),
            _ => None,
        }
    }
}
