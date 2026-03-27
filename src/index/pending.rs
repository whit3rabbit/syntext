//! Pending edits buffer for incremental index updates.
//!
//! `PendingEdits` accumulates file changes and deletions between commits.
//! Nothing here is visible to queries until `commit_batch()` drains this buffer.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use roaring::RoaringBitmap;

use super::overlay::EditKind;

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
    uncommitted: Vec<super::overlay::FileEdit>,
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
        state.uncommitted.push(super::overlay::FileEdit {
            path: path.to_owned(),
            kind: EditKind::Changed,
        });
    }

    /// Buffer a file deletion. NOT visible to queries until `commit_batch()`.
    pub fn notify_delete(&self, path: &str) {
        let mut state = self.inner.lock().unwrap();
        state.dirty_files.insert(path.to_owned(), EditKind::Deleted);
        state.uncommitted.push(super::overlay::FileEdit {
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
