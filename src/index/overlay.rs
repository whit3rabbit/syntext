//! Overlay: in-memory view of dirty files for incremental updates.
//!
//! The overlay provides read-your-writes freshness with atomic batch commits
//! and snapshot isolation. Pending edits are invisible until `commit_batch()`.
//!
//! Design: single merged query view (research.md section 7). Each
//! `commit_batch()` incrementally rebuilds the overlay, reusing docs from
//! the previous generation for unchanged files and reading only the delta
//! from disk.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use roaring::RoaringBitmap;

use crate::tokenizer::build_all;

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
    pub path: String,
    /// Nature of the change.
    pub kind: EditKind,
}

/// A dirty file tracked by the overlay with its current content and grams.
#[derive(Debug, Clone)]
pub struct OverlayDoc {
    /// Overlay-space doc_id (disjoint from base segment range).
    pub doc_id: u32,
    /// Repository-relative path.
    pub path: String,
    /// Current file content (kept for verification during search).
    /// Arc-shared to avoid cloning between snapshot generations.
    pub content: Arc<[u8]>,
    /// Cached gram hashes for this document. Avoids re-tokenization
    /// when the doc is carried forward to the next overlay generation.
    pub grams: Vec<u64>,
}

/// Single merged in-memory gram index for all dirty files.
///
/// Rebuilt from scratch on each `commit_batch()`. Query execution always
/// does two lookups: base segments + this single overlay.
pub struct OverlayView {
    /// Map from gram hash to sorted overlay doc_ids that contain it.
    pub gram_index: HashMap<u64, Vec<u32>>,
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

    /// Build an overlay from a set of dirty files.
    ///
    /// `base_doc_count` is the total doc count across all base segments.
    /// Overlay doc_ids start at `base_doc_count` to stay disjoint.
    /// `dirty_files` maps repo-relative path to file content.
    pub fn build(base_doc_count: u32, dirty_files: Vec<(String, Arc<[u8]>)>) -> Self {
        let mut gram_index: HashMap<u64, Vec<u32>> = HashMap::new();
        let mut docs = Vec::with_capacity(dirty_files.len());
        let mut next_id = base_doc_count;

        for (path, content) in dirty_files {
            let doc_id = next_id;
            next_id = next_id
                .checked_add(1)
                .expect("doc_id overflow: base_doc_count + overlay size exceeds u32::MAX");

            let grams = build_all(&content);
            for &gram_hash in &grams {
                gram_index.entry(gram_hash).or_default().push(doc_id);
            }

            docs.push(OverlayDoc {
                doc_id,
                path,
                content,
                grams,
            });
        }

        // Sort posting lists for consistent intersection behavior.
        for ids in gram_index.values_mut() {
            ids.sort_unstable();
            ids.dedup();
        }

        let doc_id_map = docs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.doc_id, i))
            .collect();

        OverlayView {
            gram_index,
            docs,
            doc_id_map,
            next_doc_id: next_id,
            base_doc_count,
        }
    }

    /// Build an overlay incrementally, reusing docs from the previous overlay
    /// for files that haven't changed since the last commit.
    ///
    /// Only `new_files` are read from disk and tokenized fresh. Unchanged docs
    /// from `old_overlay` are carried forward with `Arc::clone` on content
    /// (refcount bump, no data copy). All docs are re-indexed with new doc_ids
    /// since the ID space starts at `base_doc_count`.
    pub fn build_incremental(
        base_doc_count: u32,
        old_overlay: &OverlayView,
        new_files: Vec<(String, Arc<[u8]>)>,
        newly_changed: &HashSet<String>,
        removed_paths: &HashSet<String>,
    ) -> Self {
        let mut gram_index: HashMap<u64, Vec<u32>> = HashMap::new();
        let mut docs = Vec::new();
        let mut next_id = base_doc_count;

        // Reuse old overlay docs that are unchanged.
        for old_doc in &old_overlay.docs {
            if removed_paths.contains(&old_doc.path) {
                continue;
            }
            if newly_changed.contains(&old_doc.path) {
                continue; // replaced by new version below
            }
            let doc_id = next_id;
            next_id = next_id
                .checked_add(1)
                .expect("doc_id overflow: base_doc_count + overlay size exceeds u32::MAX");

            // Reuse cached grams instead of re-tokenizing.
            for &gram_hash in &old_doc.grams {
                gram_index.entry(gram_hash).or_default().push(doc_id);
            }
            docs.push(OverlayDoc {
                doc_id,
                path: old_doc.path.clone(),
                content: Arc::clone(&old_doc.content),
                grams: old_doc.grams.clone(),
            });
        }

        // Add newly changed/added files (freshly read from disk).
        for (path, content) in new_files {
            let doc_id = next_id;
            next_id = next_id
                .checked_add(1)
                .expect("doc_id overflow: base_doc_count + overlay size exceeds u32::MAX");

            let grams = build_all(&content);
            for &gram_hash in &grams {
                gram_index.entry(gram_hash).or_default().push(doc_id);
            }
            docs.push(OverlayDoc {
                doc_id,
                path,
                content,
                grams,
            });
        }

        for ids in gram_index.values_mut() {
            ids.sort_unstable();
            ids.dedup();
        }

        let doc_id_map = docs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.doc_id, i))
            .collect();

        OverlayView {
            gram_index,
            docs,
            doc_id_map,
            next_doc_id: next_id,
            base_doc_count,
        }
    }

    /// Delta path: base_doc_count is unchanged, so overlay doc_ids for unchanged
    /// files are stable. Clone the old gram_index, remove stale entries for
    /// changed/deleted files using their cached grams, append new doc_ids for
    /// new/changed files. New doc_ids are always > all existing ids so posting
    /// lists remain sorted after push.
    fn build_incremental_delta(
        base_doc_count: u32,
        old_overlay: &OverlayView,
        new_files: Vec<(String, Arc<[u8]>)>,
        newly_changed: &HashSet<String>,
        removed_paths: &HashSet<String>,
    ) -> Self {
        // Clone old gram_index; remove stale doc_ids for changed/deleted files.
        let mut gram_index = old_overlay.gram_index.clone();

        for old_doc in &old_overlay.docs {
            if removed_paths.contains(&old_doc.path) || newly_changed.contains(&old_doc.path) {
                for &gram_hash in &old_doc.grams {
                    if let Some(list) = gram_index.get_mut(&gram_hash) {
                        list.retain(|&id| id != old_doc.doc_id);
                    }
                }
            }
        }
        // Drop posting lists that became empty after removal.
        gram_index.retain(|_, list| !list.is_empty());

        // Carry forward unchanged docs with their existing (stable) doc_ids.
        let mut docs: Vec<OverlayDoc> = old_overlay
            .docs
            .iter()
            .filter(|d| !removed_paths.contains(&d.path) && !newly_changed.contains(&d.path))
            .cloned()
            .collect();

        // New/changed files get fresh doc_ids starting from next_doc_id.
        // Since next_doc_id > all existing doc_ids, push keeps posting lists sorted.
        let mut next_id = old_overlay.next_doc_id;
        for (path, content) in new_files {
            let doc_id = next_id;
            next_id = next_id
                .checked_add(1)
                .expect("doc_id overflow: base_doc_count + overlay size exceeds u32::MAX");

            let grams = build_all(&content);
            for &gram_hash in &grams {
                gram_index.entry(gram_hash).or_default().push(doc_id);
            }
            docs.push(OverlayDoc {
                doc_id,
                path,
                content,
                grams,
            });
        }

        let doc_id_map = docs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.doc_id, i))
            .collect();

        OverlayView {
            gram_index,
            docs,
            doc_id_map,
            next_doc_id: next_id,
            base_doc_count,
        }
    }

    /// Look up an overlay doc by its global doc_id. O(1) via HashMap.
    pub fn get_doc(&self, global_id: u32) -> Option<&OverlayDoc> {
        self.doc_id_map.get(&global_id).map(|&idx| &self.docs[idx])
    }

    /// Look up an overlay doc by path.
    pub fn get_doc_by_path(&self, path: &str) -> Option<&OverlayDoc> {
        self.docs.iter().find(|d| d.path == path)
    }
}

/// Thread-safe buffer for pending edits. Invisible to queries until committed.
pub struct PendingEdits {
    inner: Mutex<PendingState>,
}

/// Accumulated state of all committed dirty files plus uncommitted pending edits.
struct PendingState {
    /// All edits since the last full build (committed + uncommitted).
    /// Maps repo-relative path to current state.
    dirty_files: HashMap<String, EditKind>,
    /// Edits buffered since the last `commit_batch()`.
    uncommitted: Vec<FileEdit>,
}

impl Default for PendingEdits {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingEdits {
    /// Create a new, empty pending edits collector.
    pub fn new() -> Self {
        PendingEdits {
            inner: Mutex::new(PendingState {
                dirty_files: HashMap::new(),
                uncommitted: Vec::new(),
            }),
        }
    }

    /// Buffer a file change. NOT visible to queries until `commit_batch()`.
    /// Only records the path; file content is read at commit time.
    pub fn notify_change(&self, path: &str) {
        let mut state = self.inner.lock().unwrap();
        state.dirty_files.insert(path.to_owned(), EditKind::Changed);
        state.uncommitted.push(FileEdit {
            path: path.to_owned(),
            kind: EditKind::Changed,
        });
    }

    /// Buffer a file deletion. NOT visible to queries until `commit_batch()`.
    pub fn notify_delete(&self, path: &str) {
        let mut state = self.inner.lock().unwrap();
        state.dirty_files.insert(path.to_owned(), EditKind::Deleted);
        state.uncommitted.push(FileEdit {
            path: path.to_owned(),
            kind: EditKind::Deleted,
        });
    }

    /// Drain uncommitted edits and return a summary for the commit.
    ///
    /// `newly_changed`/`newly_deleted` are paths touched since the last
    /// `commit_batch()`. `all_changed`/`all_deleted` are the full
    /// accumulated dirty state (for delete_set computation).
    pub fn take_for_commit(&self) -> TakeResult {
        let mut state = self.inner.lock().unwrap();

        // Deduplicate uncommitted into changed/deleted sets.
        // A file changed then deleted counts as deleted only.
        let mut newly_changed: HashSet<String> = HashSet::new();
        let mut newly_deleted: HashSet<String> = HashSet::new();
        for edit in state.uncommitted.drain(..) {
            match edit.kind {
                EditKind::Changed => {
                    newly_deleted.remove(&edit.path);
                    newly_changed.insert(edit.path);
                }
                EditKind::Deleted => {
                    newly_changed.remove(&edit.path);
                    newly_deleted.insert(edit.path);
                }
            }
        }

        let mut all_changed = Vec::new();
        let mut all_deleted = Vec::new();
        for (path, entry) in &state.dirty_files {
            match entry {
                EditKind::Changed => all_changed.push(path.clone()),
                EditKind::Deleted => all_deleted.push(path.clone()),
            }
        }

        TakeResult {
            newly_changed,
            newly_deleted,
            all_changed,
            all_deleted,
        }
    }

    /// Whether there are uncommitted edits pending.
    pub fn has_uncommitted(&self) -> bool {
        let state = self.inner.lock().unwrap();
        !state.uncommitted.is_empty()
    }

    /// Number of uncommitted edits.
    pub fn uncommitted_count(&self) -> usize {
        let state = self.inner.lock().unwrap();
        state.uncommitted.len()
    }
}

/// Result of draining uncommitted edits from `PendingEdits`.
pub struct TakeResult {
    /// Paths changed since the last `commit_batch()`.
    pub newly_changed: HashSet<String>,
    /// Paths deleted since the last `commit_batch()`.
    pub newly_deleted: HashSet<String>,
    /// All accumulated dirty paths that are present (for delete_set).
    pub all_changed: Vec<String>,
    /// All accumulated dirty paths that are deleted (for delete_set).
    pub all_deleted: Vec<String>,
}

/// Compute the delete_set: base doc_ids that are invalidated by overlay
/// changes (modified or deleted files).
///
/// Uses a prebuilt path -> doc_ids map from the immutable base snapshot.
pub fn compute_delete_set(
    base_path_doc_ids: &HashMap<String, Vec<u32>>,
    modified_paths: &[String],
    deleted_paths: &[String],
) -> RoaringBitmap {
    let mut delete_set = RoaringBitmap::new();

    for path in modified_paths.iter().chain(deleted_paths.iter()) {
        if let Some(doc_ids) = base_path_doc_ids.get(path) {
            for &doc_id in doc_ids {
                delete_set.insert(doc_id);
            }
        }
    }

    delete_set
}
