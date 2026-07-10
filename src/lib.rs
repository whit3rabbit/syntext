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
//! let config = Config::new(".syntext".into(), ".".into());
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

// ── Public API ───────────────────────────────────────────────
/// Core index management, writing, and snapshot components.
pub mod index;
/// Tree-sitter symbol extraction and SQLite cache storage (optional).
#[cfg(feature = "symbols")]
pub mod symbol;
/// WebAssembly bindings for fully in-memory index operations.
#[cfg(feature = "wasm")]
pub mod wasm;

// ── Internal modules (not public API) ────────────────────────
pub(crate) mod base64;
/// Command-line interface (used by the `st` binary).
#[cfg(all(not(target_arch = "wasm32"), feature = "cli"))]
pub mod cli;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod git_util;
/// Git hook integrations (used by the `st` binary).
#[cfg(all(not(target_arch = "wasm32"), feature = "cli"))]
pub mod hook;
pub(crate) mod path;
pub(crate) mod path_util;
pub(crate) mod posting;
pub(crate) mod query;
pub(crate) mod search;
pub(crate) mod tokenizer;

// ── Test-only access hatch ───────────────────────────────────
/// Internal items exposed for test and benchmark access only.
///
/// **No stability guarantees.** These re-exports exist so integration tests
/// and benchmarks can exercise internals. Library consumers MUST NOT use
/// this module: the items here may change or vanish in any release.
// Native only: re-exports native-gated items (manifest, mmap segment, walk)
// for the native test/bench harness. wasm has no such harness.
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod __internal {
    // base64
    pub use crate::base64::encode;
    // path
    pub use crate::path::filter;
    // posting
    pub use crate::posting::{
        roaring_util, varint_decode, varint_encode, PostingList, ROARING_THRESHOLD,
    };
    // query
    pub use crate::query::regex_decompose;
    pub use crate::query::{is_literal, literal_grams, route_query, GramQuery, QueryRoute};
    // tokenizer
    pub use crate::tokenizer::{
        build_all, build_covering, build_covering_inner, gram_hash, CoveringSet, MAX_GRAM_LEN,
        MIN_GRAM_LEN,
    };
    // index submodules
    pub use crate::index::manifest::Manifest;
    pub use crate::index::overlay::{
        compute_delete_set, EditKind, FileEdit, OverlayDoc, OverlayView,
    };
    pub use crate::index::pending::{PendingEdits, TakeResult};
    pub use crate::index::segment::{
        DictVerify, DocEntry, MmapSegment, PostVerify, SegmentMeta, SegmentWriter, FOOTER_SIZE,
        FORMAT_VERSION, MAGIC,
    };
    pub use crate::index::snapshot::{new_snapshot, BaseSegments, IndexSnapshot};
    pub use crate::index::walk::is_binary;
}

use std::path::PathBuf;

/// Configuration for index building and searching.
///
/// Use [`Config::new`] to construct. New fields may be added in minor
/// versions; prefer `Config::new(index_dir, repo_root)` with field
/// assignment over struct literal syntax for forward compatibility.
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
    /// Optional rayon thread pool for parallel search execution. When `None`
    /// (the default), the global rayon pool is used. Embedders that manage
    /// their own thread pool can provide one here to avoid contending with
    /// the global pool.
    #[cfg(feature = "rayon")]
    pub thread_pool: Option<std::sync::Arc<rayon::ThreadPool>>,
}

impl Config {
    /// Create a new `Config` with required paths set and sensible defaults for
    /// all other fields.
    pub fn new(index_dir: PathBuf, repo_root: PathBuf) -> Self {
        Self {
            index_dir,
            repo_root,
            max_file_size: 10 * 1024 * 1024,
            max_segments: 10,
            verbose: false,
            strict_permissions: true,
            verify_on_open: false,
            recalibrate: false,
            auto_update: true,
            auto_update_max_files: 200,
            auto_update_budget_ms: 150,
            auto_update_async_catchup: true,
            #[cfg(feature = "rayon")]
            thread_pool: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new(PathBuf::from(".syntext"), PathBuf::from("."))
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
#[derive(Debug, Clone)]
#[non_exhaustive]
#[derive(Default)]
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
    /// Skip populating `SearchMatch::line_content`, leaving it empty. Set for
    /// output modes that only need which files matched (`-l`/`-L`), avoiding a
    /// per-matched-line byte copy in the verifier. Do NOT set for `-c`, which
    /// re-scans `line_content` to count per-line occurrences. Default `false`
    /// (populate), so existing callers are unaffected.
    pub skip_line_content: bool,
    /// Force deterministic result ordering across `rayon` worker thread
    /// scheduling differences (default: `false`). When `true`, the per-file
    /// early-exit based on `max_results` is disabled so that the candidate set
    /// is always fully resolved before truncation, producing identical output
    /// regardless of thread interleaving. Costs latency on large result sets.
    /// Replaces the `SYNTEXT_DETERMINISTIC` environment variable.
    pub deterministic: bool,
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

#[cfg(test)]
mod api_tests {
    use std::sync::Arc;

    #[test]
    fn index_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        // Static assertions: if these fail to compile, a refactor has made
        // Index or IndexSnapshot !Send or !Sync, a regression for embedders.
        assert_send_sync::<crate::index::Index>();
        assert_send_sync::<Arc<crate::index::snapshot::IndexSnapshot>>();
    }
}
