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

/// Helper module for base64 encoding and decoding.
pub mod base64;
/// Command-line interface logic and argument parsing.
#[cfg(all(not(target_arch = "wasm32"), feature = "cli"))]
pub mod cli;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod git_util;
/// Git hook integrations for automated indexing workflows.
#[cfg(all(not(target_arch = "wasm32"), feature = "cli"))]
pub mod hook;
/// Core index management, writing, and snapshot components.
pub mod index;
/// Directory and file path component indexing using Roaring bitmaps.
pub mod path;
pub(crate) mod path_util;
/// Posting list encoding, decoding, and adaptive set operations.
pub mod posting;
/// Query planning, parsing, and decomposition of search patterns.
pub mod query;
/// Search execution and candidate verification engine.
pub mod search;
/// Tree-sitter symbol extraction and SQLite cache storage (optional).
#[cfg(feature = "symbols")]
pub mod symbol;
/// Gram tokenization and weight frequency table definitions.
pub mod tokenizer;
/// WebAssembly bindings for fully in-memory index operations.
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
    /// Fully checksum each segment's `.post` file at `Index::open` time.
    /// Default: false (O(1) structural checks only).
    ///
    /// The full pass costs O(total postings bytes) of I/O per open and only
    /// detects at-rest corruption present at open time; it is not a
    /// query-time integrity control (postings are re-read from disk per
    /// query), and postings parsing is bounds-checked, so corruption can
    /// cause missing results or `CorruptIndex` errors but never memory
    /// unsafety or fabricated match content. The `.dict` side is always
    /// fully verified. Enable via this flag, `SYNTEXT_VERIFY_ON_OPEN=1`, or
    /// run `st verify` for an on-demand full check.
    pub verify_on_open: bool,
    /// Force re-measurement of the index-vs-scan crossover threshold during
    /// `Index::build`, even when a prior manifest already carries a
    /// calibrated value. Default: false (reuse the prior value; it is
    /// hardware-dependent, not content-dependent). Set after moving the
    /// index to different hardware or storage. CLI: `st index --recalibrate`.
    pub recalibrate: bool,
    /// Whether `st search` should auto-update the index via git change
    /// detection before searching. Default: true.
    ///
    /// When enabled, `st search` calls `Index::update_from_git` bounded by
    /// `auto_update_budget_ms` and `auto_update_max_files`. A stale index
    /// can only produce false negatives (missed matches), never false
    /// positives — the verifier re-reads real file bytes before reporting.
    pub auto_update: bool,
    /// Maximum number of changed files to process in an auto-update before
    /// proceeding with a stale index. Default: 200.
    pub auto_update_max_files: usize,
    /// Elapsed-time budget in milliseconds for the three git detection
    /// commands in an auto-update. Default: 150.
    pub auto_update_budget_ms: u64,
    /// Whether `st search` should spawn a detached `st update --quiet`
    /// catch-up after printing results when the bounded auto-update above
    /// hit `TooManyFiles` or `BudgetExceeded` (i.e. the index is known to be
    /// stale beyond the search-time budget). Default: true. Disable via
    /// `SYNTEXT_NO_ASYNC_UPDATE=1`.
    pub auto_update_async_catchup: bool,
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
            verify_on_open: false,
            recalibrate: false,
            auto_update: true,
            auto_update_max_files: 200,
            auto_update_budget_ms: 150,
            auto_update_async_catchup: true,
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
    /// If set, use this pattern for regex/literal verification instead
    /// of the routing pattern. Used by -w/-x to verify with boundary
    /// wrapping while routing on the unwrapped inner pattern for gram
    /// extraction.
    pub verify_pattern: Option<String>,
    /// For testing/oracle: bypass query routing and force a full scan.
    #[cfg(any(test, feature = "oracle"))]
    pub force_full_scan: bool,
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
#[non_exhaustive]
pub enum IndexError {
    /// I/O error (file not found, permission denied, etc.)
    Io(std::io::Error),
    /// No index exists at the given index directory. Build one first
    /// (`Index::build`, or `st index` from the CLI).
    IndexNotFound(PathBuf),
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
            IndexError::IndexNotFound(p) => {
                write!(
                    f,
                    "no index found at {}: run `st index` to build one",
                    p.display()
                )
            }
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
