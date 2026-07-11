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
/// Error types for index operations.
pub mod error;
/// Tree-sitter symbol extraction and SQLite cache storage (optional).
#[cfg(feature = "symbols")]
pub mod symbol;
/// WebAssembly bindings for fully in-memory index operations.
#[cfg(feature = "wasm")]
pub mod wasm;

pub use error::IndexError;

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
use std::sync::Arc;

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
    /// CLI-only: sets the `st` binary's log level (verbose → `Debug`, else
    /// `Warn`). Default: false. The library itself no longer reads this field;
    /// its diagnostics go through the `log` facade, so a library embedder
    /// controls verbosity by installing (or not installing) a `log` logger, not
    /// via this flag.
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

/// All matches within a single file, plus the exact bytes the verifier matched
/// against.
///
/// `content` is the encoding-normalized byte view captured at verification time
/// (UTF-8 BOM stripped, UTF-16 transcoded). Line numbers and byte offsets in
/// `matches` index into THESE bytes, not the file on disk, which may have
/// changed since. Rendering from `content` instead of re-reading the path is
/// what keeps results consistent under concurrent file churn.
///
/// Produced by [`Index::search_grouped`](crate::index::Index::search_grouped).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FileMatches {
    /// Repository-relative path.
    pub path: PathBuf,
    /// Matches within this file, sorted by line number ascending. Never empty.
    pub matches: Vec<SearchMatch>,
    /// Encoding-normalized file content at verification time. Cheap to clone
    /// (`Arc`); shared with the index's internal buffers, so holding results
    /// does not copy file bytes.
    pub content: Arc<[u8]>,
}

impl FileMatches {
    /// Iterate `(line_number, line_bytes)` over the captured content. Line
    /// numbers are 1-based; bytes exclude the trailing newline (and the `\r`
    /// of a CRLF pair), matching [`SearchMatch::line_content`].
    pub fn lines(&self) -> Vec<(u32, &[u8])> {
        // Collect (line_no, start, len) inside the closure, then slice
        // `self.content` after: the closure's `&[u8]` borrows a per-call
        // lifetime that cannot escape, but the spans can.
        let mut spans: Vec<(u32, usize, usize)> = Vec::new();
        crate::search::lines::for_each_line(&self.content, |n, start, line| {
            spans.push((n, start, line.len()));
        });
        spans
            .into_iter()
            .map(|(n, start, len)| (n, &self.content[start..start + len]))
            .collect()
    }

    /// The lines around `line_number`: up to `before` preceding and `after`
    /// following, plus the line itself. The tuple's bool is true for the match
    /// line. This is the ±N context an editor plugin renders next to each hit.
    ///
    /// Linear-scans `content` once per call, which is fine for the tens of
    /// matches a UI shows at a time. To render thousands with context, iterate
    /// [`lines`](Self::lines) yourself once instead of calling this per match.
    pub fn context(
        &self,
        line_number: u32,
        before: usize,
        after: usize,
    ) -> Vec<(u32, &[u8], bool)> {
        let target = line_number as usize;
        let lo = target.saturating_sub(before);
        let hi = target.saturating_add(after);
        let mut spans: Vec<(u32, usize, usize, bool)> = Vec::new();
        crate::search::lines::for_each_line(&self.content, |n, start, line| {
            let nn = n as usize;
            if nn >= lo && nn <= hi {
                spans.push((n, start, line.len(), nn == target));
            }
        });
        spans
            .into_iter()
            .map(|(n, start, len, is_match)| (n, &self.content[start..start + len], is_match))
            .collect()
    }
}

/// Search options.
#[derive(Debug, Clone)]
#[non_exhaustive]
#[derive(Default)]
pub struct SearchOptions {
    /// Glob pattern to restrict search to matching paths.
    pub path_filter: Option<String>,
    /// File type filter (e.g., "rs", "py"). Single-type convenience; combined
    /// with `file_types` (both are honored). Prefer `file_types` for 2+ types
    /// so the Roaring extension index narrows the candidate set.
    pub file_type: Option<String>,
    /// Exclude files of this type from results. Combined with `exclude_types`.
    pub exclude_type: Option<String>,
    /// Include only files with one of these extensions. Empty by default.
    /// Unlike a single `file_type`, multiple entries are UNIONed against the
    /// path index (no fallback to full post-filtering), so `-t rs -t py` still
    /// narrows candidates. Combined with `file_type`.
    pub file_types: Vec<String>,
    /// Exclude files with any of these extensions. Combined with `exclude_type`.
    pub exclude_types: Vec<String>,
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
