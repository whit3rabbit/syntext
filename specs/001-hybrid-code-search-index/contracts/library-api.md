# Library API Contract

**Date**: 2026-03-25
**Type**: Rust public API (library crate)

## Core Types

```rust
/// Configuration for index building and searching.
pub struct Config {
    /// Maximum file size to index (bytes). Default: 10MB.
    pub max_file_size: u64,
    /// Maximum segments before triggering background merge. Default: 10.
    pub max_segments: usize,
    /// Overlay flush threshold (file count). Default: 100.
    pub overlay_flush_files: usize,
    /// Overlay flush threshold (bytes). Default: 10MB.
    pub overlay_flush_bytes: u64,
    /// Path to index directory.
    pub index_dir: PathBuf,
    /// Repository root path.
    pub repo_root: PathBuf,
}

/// A search match.
pub struct SearchMatch {
    pub path: PathBuf,
    pub line_number: u32,
    pub line_content: String,
    pub byte_offset: u64,
}

/// Search options.
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

/// Index statistics.
pub struct IndexStats {
    pub total_documents: usize,
    pub total_segments: usize,
    pub total_grams: usize,
    pub index_size_bytes: u64,
    pub base_commit: Option<String>,
    pub overlay_generations: usize,
    pub pending_edits: usize,
}
```

## Primary API

```rust
/// The main index handle. Thread-safe for concurrent reads.
pub struct Index { /* ... */ }

impl Index {
    /// Open or create an index at the configured directory.
    /// Returns error if index is corrupt (caller should rebuild).
    pub fn open(config: Config) -> Result<Self, IndexError>;

    /// Build the full index from scratch for the repository.
    /// Respects .gitignore and max_file_size config.
    pub fn build(&self) -> Result<IndexStats, IndexError>;

    /// Search for a pattern (literal or regex).
    /// Automatically routes to the optimal execution path:
    /// - Literal patterns use memchr::memmem for verification.
    /// - Regex patterns with extractable grams use indexed search.
    /// - Regex patterns without grams fall back to full scan.
    /// Returns matches sorted by file path, then line number.
    pub fn search(
        &self,
        pattern: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SearchMatch>, IndexError>;

    /// Buffer a file change. NOT visible to queries until commit_batch().
    /// Use notify_change_immediate() for single-file convenience.
    pub fn notify_change(&self, path: &Path) -> Result<(), IndexError>;

    /// Buffer a file deletion. NOT visible to queries until commit_batch().
    pub fn notify_delete(&self, path: &Path) -> Result<(), IndexError>;

    /// Atomically commit all pending edits as a new overlay generation.
    /// After return, all buffered changes are visible to subsequent queries.
    /// This is the "read-your-writes" boundary.
    pub fn commit_batch(&self) -> Result<(), IndexError>;

    /// Convenience: notify_change + commit_batch for a single file.
    pub fn notify_change_immediate(&self, path: &Path) -> Result<(), IndexError>;

    /// Compact overlay segments if threshold exceeded (>16 generations
    /// or overlay size > 10% of base). Runs in background, non-blocking.
    /// Returns true if compaction was triggered.
    pub fn maybe_compact(&self) -> Result<bool, IndexError>;

    /// Force compact all overlays into one. Blocking.
    pub fn compact(&self) -> Result<(), IndexError>;

    /// Get index statistics.
    pub fn stats(&self) -> Result<IndexStats, IndexError>;

    /// Rebuild the index if HEAD has changed since last build.
    /// Returns None if no rebuild needed.
    pub fn rebuild_if_stale(&self) -> Result<Option<IndexStats>, IndexError>;
}
```

## Error Types

```rust
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
    FileTooLarge { path: PathBuf, size: u64 },
}
```

## Guarantees

1. **Thread safety**: `Index` is `Send + Sync`. Internally uses `ArcSwap<IndexSnapshot>` for lock-free concurrent reads. `search()` clones the Arc at the start, getting a stable snapshot. `notify_change()` / `notify_delete()` acquire a `Mutex` on the pending buffer (does not block readers). `commit_batch()` creates a new `IndexSnapshot` and atomically swaps the `ArcSwap` pointer.
2. **Snapshot isolation**: in-flight `search()` calls always complete against the snapshot they started with, even if `commit_batch()` swaps the snapshot mid-query.
3. **Correctness**: `search()` never returns false negatives (assuming the index is up to date via `commit_batch()`). False positives are eliminated by the verification step.
4. **Freshness**: After `commit_batch()` returns, subsequent `search()` calls reflect all buffered changes. Pending edits (between `notify_change()` and `commit_batch()`) are NOT visible to queries. This is intentional: it prevents partial visibility of multi-file atomic edits.
5. **Atomicity**: `build()` and `commit_batch()` update the manifest atomically (write-then-rename). A crash during either leaves the previous manifest valid. On-disk overlay generations enable recovery of committed-but-not-yet-consolidated changes.
6. **Gitignore respect**: `notify_change()` checks `.gitignore` rules and silently skips ignored files. Callers do not need to pre-filter.
7. **Case handling**: the index stores grams from lowercased content. Case-sensitive queries return correct results (the index returns a superset, the verifier filters). Case-insensitive queries are naturally supported.
8. **No unsafe**: No public API exposes `unsafe`. Internal `unsafe` (if any) is limited to mmap operations with documented justification.
