//! Hybrid code search index library.
//!
//! Provides the core index, search, and configuration APIs.

pub mod cli;
pub mod index;
pub mod path;
pub mod posting;
pub mod query;
pub mod search;
pub mod tokenizer;
// NOTE: `symbol` module (Tree-sitter + SQLite symbol index) is deferred to Phase 7.
// When implemented, add: pub mod symbol;
// See specs/001-hybrid-code-search-index/plan.md for the implementation plan.
#[cfg(feature = "symbols")]
pub mod symbol;

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_file_size: 10 * 1024 * 1024,
            max_segments: 10,
            index_dir: PathBuf::from(".ripline"),
            repo_root: PathBuf::from("."),
            verbose: false,
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
    /// Full text of the matching line (without trailing newline).
    pub line_content: String,
    /// Byte offset of the start of the first match on the line.
    pub byte_offset: u64,
}

/// Search options.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Glob pattern to restrict search to matching paths.
    pub path_filter: Option<String>,
    /// File type filter (e.g., "rs", "py").
    pub file_type: Option<String>,
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
    /// Number of on-disk RPLX segment files.
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
            IndexError::PathOutsideRepo(p) => write!(f, "path outside repo: {}", p.display()),
            IndexError::FileTooLarge { path, size } => {
                write!(f, "file too large: {} ({size} bytes)", path.display())
            }
            IndexError::LockConflict(p) => {
                write!(f, "index locked by another process: {}", p.display())
            }
            IndexError::OverlayFull { overlay_docs, base_docs } => write!(
                f,
                "overlay too large ({overlay_docs} overlay docs, {base_docs} base docs): \
                 run `ripline index` to rebuild"
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
