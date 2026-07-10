//! Overlay: in-memory view of dirty files for incremental updates.
//!
//! The overlay provides read-your-writes freshness with atomic batch commits
//! and snapshot isolation. Pending edits are invisible until `commit_batch()`.
//!
//! Design: single merged query view (research.md section 7). Each
//! `commit_batch()` incrementally rebuilds the overlay, reusing docs from
//! the previous generation for unchanged files and reading only the delta
//! from disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod build;

/// Kind of file change buffered by `notify_change` / `notify_delete`.
#[derive(Debug, Clone)]
pub enum EditKind {
    /// File was added or modified (content changed).
    Changed,
    /// File was removed from the repository.
    Deleted,
}

/// A buffered file edit not yet committed to the index snapshot.
#[derive(Debug, Clone)]
pub struct FileEdit {
    /// Repository-relative path of the changed file.
    pub path: PathBuf,
    /// Nature of the change.
    pub kind: EditKind,
}

/// A dirty file tracked by the overlay with its current content and grams.
///
/// # Memory: content is pinned for the overlay's lifetime
///
/// `content` holds the full file bytes and is carried forward across snapshot
/// generations via `Arc::clone` (refcount bump, no copy). This keeps verify-time
/// reads O(1) and avoids re-reading changed files on every commit. The cost is
/// that every dirty file's content stays resident for as long as it remains in
/// the overlay. With the 50%-of-base overlay cap (`OVERLAY_ENFORCE_THRESHOLD`)
/// and default 10 MB `max_file_size`, a long-lived process (watcher, library
/// consumer) can legitimately hold gigabytes of overlay content.
///
/// v2 mitigation (not yet implemented): store `content_hash` + `grams` only and
/// re-read content from disk at verify time via the same hardened path
/// (`resolve_doc`) used for base docs, or spill docs above a byte threshold.
/// The carry-forward cost note in `build_incremental_delta` applies equally to
/// any spill design.
#[derive(Debug, Clone)]
pub struct OverlayDoc {
    /// Overlay-space doc_id (disjoint from base segment range).
    pub doc_id: u32,
    /// Repository-relative path.
    pub path: PathBuf,
    /// Current file content (kept for verification during search).
    /// Arc-shared to avoid cloning between snapshot generations. See the type
    /// doc for the memory-pinning trade-off.
    pub content: Arc<[u8]>,
    /// Cached gram hashes for this document. Avoids re-tokenization
    /// when the doc is carried forward to the next overlay generation.
    pub grams: Vec<u64>,
}

/// Single merged in-memory gram index for all dirty files.
///
/// A fresh `OverlayView` is produced on each `commit_batch()`, but unchanged
/// file content is `Arc`-reused across generations (`OverlayDoc::content`);
/// posting lists are likewise `Arc`-shared so the delta commit path
/// (`build_incremental_delta`) clones the map as refcount bumps and only
/// deep-copies the lists it actually mutates. Query execution always does two
/// lookups: base segments + this single overlay.
#[derive(Clone)]
pub struct OverlayView {
    /// Map from gram hash to sorted overlay doc_ids that contain it. Posting
    /// lists are `Arc`-shared across generations so an unchanged list carried
    /// through a delta commit costs a refcount bump, not a `Vec` copy.
    pub gram_index: HashMap<u64, Arc<Vec<u32>>>,
    /// All dirty files with current content.
    pub docs: Vec<OverlayDoc>,
    /// doc_id -> index into `docs` for O(1) lookup.
    doc_id_map: HashMap<u32, usize>,
    /// Next overlay-space doc_id (starts after base range).
    pub next_doc_id: u32,
    /// The base_doc_count at which this overlay was built.
    /// Used to detect whether a segment flush occurred between commits.
    pub base_doc_count: u32,
}

impl OverlayView {
    /// Create an empty overlay view.
    pub fn empty() -> Self {
        OverlayView {
            gram_index: HashMap::new(),
            docs: Vec::new(),
            doc_id_map: HashMap::new(),
            next_doc_id: 0,
            base_doc_count: 0,
        }
    }



    /// Look up an overlay doc by its global doc_id. O(1) via HashMap.
    pub fn get_doc(&self, global_id: u32) -> Option<&OverlayDoc> {
        self.doc_id_map.get(&global_id).map(|&idx| &self.docs[idx])
    }

    /// Look up an overlay doc by path.
    pub fn get_doc_by_path(&self, path: &Path) -> Option<&OverlayDoc> {
        self.docs.iter().find(|d| d.path == path)
    }
}

// Re-export pending types so callers using `crate::index::overlay::*` continue to compile.
pub use crate::index::pending::{compute_delete_set, PendingEdits, TakeResult};

#[cfg(test)]
#[path = "../overlay_tests.rs"]
mod tests;
