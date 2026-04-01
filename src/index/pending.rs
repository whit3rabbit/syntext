//! Pending edits buffer for incremental index updates.
//!
//! `PendingEdits` accumulates file changes and deletions between commits.
//! Nothing here is visible to queries until `commit_batch()` drains this buffer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use roaring::RoaringBitmap;

use super::overlay::EditKind;

/// Thread-safe buffer for pending edits. Invisible to queries until committed.
pub struct PendingEdits {
    inner: Mutex<PendingState>,
}

/// Edits buffered since the last `commit_batch()`.
struct PendingState {
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
                uncommitted: Vec::new(),
            }),
        }
    }

    /// Buffer a file change. NOT visible to queries until `commit_batch()`.
    /// Only records the path; file content is read at commit time.
    ///
    /// # Poison recovery
    /// Recovery via `into_inner` is safe here: the only operation under the lock
    /// is `Vec::push`, which cannot panic except on OOM (which aborts, not
    /// unwinds, under the default allocator). The same reasoning applies to
    /// `notify_delete`, `reset`, `has_uncommitted`, and `uncommitted_count`.
    ///
    /// `take_for_commit` is theoretically riskier: if unwinding occurred
    /// mid-`drain` (e.g. a custom allocator that unwinds on OOM during
    /// `HashSet::insert`), the Vec would retain only the un-drained suffix,
    /// silently dropping earlier edits from the commit. In practice this cannot
    /// happen with the default global allocator.
    ///
    /// Contrast with `SymbolIndex` (symbol/mod.rs), which deliberately does NOT
    /// recover from poison because its `rusqlite::Connection` may hold open
    /// transactions or inconsistent prepared-statement cache state.
    pub fn notify_change(&self, path: &Path) {
        let mut state = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        state.uncommitted.push(super::overlay::FileEdit {
            path: path.to_path_buf(),
            kind: EditKind::Changed,
        });
    }

    /// Buffer a file deletion. NOT visible to queries until `commit_batch()`.
    pub fn notify_delete(&self, path: &Path) {
        let mut state = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        state.uncommitted.push(super::overlay::FileEdit {
            path: path.to_path_buf(),
            kind: EditKind::Deleted,
        });
    }

    /// Drain uncommitted edits and return a summary for the commit.
    ///
    /// `newly_changed`/`newly_deleted` are paths touched since the last
    /// `commit_batch()`. A file changed then deleted in the same batch counts
    /// as deleted only.
    pub fn take_for_commit(&self) -> TakeResult {
        let mut state = self.inner.lock().unwrap_or_else(|p| p.into_inner());

        // Deduplicate uncommitted into changed/deleted sets.
        // A file changed then deleted counts as deleted only.
        let mut newly_changed: HashSet<PathBuf> = HashSet::new();
        let mut newly_deleted: HashSet<PathBuf> = HashSet::new();
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

        TakeResult {
            newly_changed,
            newly_deleted,
        }
    }

    /// Clear all accumulated state. Call after a full index rebuild.
    pub fn reset(&self) {
        let mut state = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        state.uncommitted.clear();
    }

    /// Whether there are uncommitted edits pending.
    pub fn has_uncommitted(&self) -> bool {
        let state = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        !state.uncommitted.is_empty()
    }

    /// Number of uncommitted edits.
    pub fn uncommitted_count(&self) -> usize {
        let state = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        state.uncommitted.len()
    }
}

/// Result of draining uncommitted edits from `PendingEdits`.
pub struct TakeResult {
    /// Paths changed since the last `commit_batch()`.
    pub newly_changed: HashSet<PathBuf>,
    /// Paths deleted since the last `commit_batch()`.
    pub newly_deleted: HashSet<PathBuf>,
}

/// Compute the delete_set: base doc_ids that are invalidated by overlay
/// changes (modified or deleted files).
///
/// # Precondition: `modified_paths` and `deleted_paths` must be disjoint
///
/// `take_for_commit` guarantees mutual exclusivity: a path changed then
/// deleted in the same batch is placed in `deleted_paths` only. If a caller
/// passes overlapping sets, `RoaringBitmap::insert` is idempotent so the result
/// is still correct, but it indicates a bug in the caller's logic.
///
/// Starts from `prev` (the previous snapshot's delete_set) and adds entries
/// for the current delta only. The base is immutable between full builds, so
/// the delete_set is monotonically growing and this is always correct.
pub fn compute_delete_set(
    base_path_doc_ids: &HashMap<PathBuf, Vec<u32>>,
    modified_paths: &HashSet<PathBuf>,
    deleted_paths: &HashSet<PathBuf>,
    prev: &RoaringBitmap,
) -> RoaringBitmap {
    let mut delete_set = prev.clone();

    for path in modified_paths.iter().chain(deleted_paths.iter()) {
        if let Some(doc_ids) = base_path_doc_ids.get(path) {
            for &doc_id in doc_ids {
                delete_set.insert(doc_id);
            }
        }
    }

    delete_set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_clears_uncommitted() {
        let pe = PendingEdits::new();
        pe.notify_change(Path::new("a.rs"));
        pe.notify_change(Path::new("b.rs"));
        pe.reset();
        assert_eq!(pe.uncommitted_count(), 0, "reset() must clear uncommitted");
    }

    #[test]
    fn take_for_commit_after_reset_returns_empty() {
        let pe = PendingEdits::new();
        pe.notify_change(Path::new("a.rs"));
        pe.reset();
        let result = pe.take_for_commit();
        assert!(result.newly_changed.is_empty());
        assert!(result.newly_deleted.is_empty());
    }

    #[test]
    fn compute_delete_set_is_incremental() {
        let mut base: HashMap<PathBuf, Vec<u32>> = HashMap::new();
        base.insert(PathBuf::from("a.rs"), vec![1]);
        base.insert(PathBuf::from("b.rs"), vec![2]);
        base.insert(PathBuf::from("c.rs"), vec![3]);

        // First commit: only a.rs changed.
        let prev = RoaringBitmap::new();
        let changed: HashSet<PathBuf> = [PathBuf::from("a.rs")].into();
        let deleted: HashSet<PathBuf> = HashSet::new();
        let ds1 = compute_delete_set(&base, &changed, &deleted, &prev);
        assert!(ds1.contains(1));
        assert!(!ds1.contains(2));

        // Second commit: b.rs deleted. Previous delete_set carried forward.
        let changed2: HashSet<PathBuf> = HashSet::new();
        let deleted2: HashSet<PathBuf> = [PathBuf::from("b.rs")].into();
        let ds2 = compute_delete_set(&base, &changed2, &deleted2, &ds1);
        assert!(ds2.contains(1), "a.rs entry must persist");
        assert!(ds2.contains(2), "b.rs entry must be added");
        assert!(!ds2.contains(3));
    }
}
