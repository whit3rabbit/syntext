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
    /// `take_for_commit` is theoretically riskier: it uses `std::mem::take`
    /// (a pointer swap, cannot fail) and then rebuilds changed/deleted sets via
    /// `HashSet::insert`. If unwinding occurred mid-rebuild (e.g. a custom
    /// allocator that unwinds on OOM during `HashSet::insert`), the drained
    /// edits in the local `drained` Vec would be dropped, losing them. In
    /// practice this cannot happen with the default global allocator.
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

    /// Drain uncommitted edits and return a summary for the commit, capturing
    /// the raw edits in [`TakeResult::drained`] for re-queueing on failure.
    ///
    /// `newly_changed`/`newly_deleted` are paths touched since the last
    /// `commit_batch()`. A file changed then deleted in the same batch counts
    /// as deleted only.
    ///
    /// `drained` holds the exact raw edits in original insertion order. If
    /// `commit_batch` fails after this call, the caller MUST pass `drained`
    /// back to [`PendingEdits::requeue_uncommitted`] so the edits are not
    /// silently lost (staying stale until a full rebuild). On success, the
    /// caller drops `drained`.
    pub fn take_for_commit(&self) -> TakeResult {
        let mut state = self.inner.lock().unwrap_or_else(|p| p.into_inner());

        // Snapshot the raw edits before clearing the buffer. Re-queueing these
        // on failure preserves the caller's edit stream verbatim, which is
        // simpler and safer than re-deriving changed/deleted sets from a
        // partially-applied commit.
        let drained = std::mem::take(&mut state.uncommitted);

        // Deduplicate uncommitted into changed/deleted sets.
        // A file changed then deleted counts as deleted only.
        let mut newly_changed: HashSet<PathBuf> = HashSet::new();
        let mut newly_deleted: HashSet<PathBuf> = HashSet::new();
        for edit in &drained {
            match edit.kind {
                EditKind::Changed => {
                    newly_deleted.remove(&edit.path);
                    newly_changed.insert(edit.path.clone());
                }
                EditKind::Deleted => {
                    newly_changed.remove(&edit.path);
                    newly_deleted.insert(edit.path.clone());
                }
            }
        }

        TakeResult {
            newly_changed,
            newly_deleted,
            drained,
        }
    }

    /// Re-queue edits drained by [`take_for_commit`] when the commit fails.
    ///
    /// Prepends `edits` to the front of the uncommitted buffer so that any
    /// edits buffered concurrently with the failed commit are applied after
    /// them (preserving the original arrival order relative to each other).
    /// Idempotent if `edits` is empty.
    ///
    /// Deduplicates to at most one edit per path (last occurrence wins, matching
    /// [`take_for_commit`]'s last-wins fold). Without this, repeated commit
    /// failures (e.g. a persistent `OverlayFull` in a watcher that keeps calling
    /// `notify_change`) re-prepend the same drained edits every cycle while new
    /// notifies append, growing the buffer with duplicate `PathBuf`s unboundedly
    /// until a full rebuild. The commit result is already correct either way
    /// (`take_for_commit` folds into sets); this bounds memory.
    pub fn requeue_uncommitted(&self, edits: Vec<super::overlay::FileEdit>) {
        if edits.is_empty() {
            return;
        }
        let mut state = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        // Splice the requeued edits before any edits that arrived during the
        // failed commit: `edits` were observed first and must stay first.
        let mut combined = edits;
        combined.append(&mut state.uncommitted);
        // Keep only the last edit per path. Walk in reverse so the last
        // occurrence is retained, then restore forward order. Order among
        // distinct paths does not affect correctness (take_for_commit builds
        // sets), only which kind wins per path, which last-occurrence preserves.
        let mut seen: HashSet<PathBuf> = HashSet::with_capacity(combined.len());
        let mut deduped: Vec<super::overlay::FileEdit> = Vec::with_capacity(combined.len());
        for edit in combined.into_iter().rev() {
            if seen.insert(edit.path.clone()) {
                deduped.push(edit);
            }
        }
        deduped.reverse();
        state.uncommitted = deduped;
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
    /// Raw drained edits (in arrival order). Pass back to
    /// [`PendingEdits::requeue_uncommitted`] if the commit fails so the edits
    /// are not silently lost. Drop on success.
    pub drained: Vec<super::overlay::FileEdit>,
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
    fn requeue_dedups_and_stays_bounded() {
        let pe = PendingEdits::new();

        // Simulate repeated commit failures: drain, then requeue the same edits
        // while a new notify arrives each cycle.
        pe.notify_change(Path::new("a.rs"));
        for _ in 0..100 {
            let taken = pe.take_for_commit();
            pe.notify_change(Path::new("b.rs")); // concurrent arrival
            pe.requeue_uncommitted(taken.drained);
        }
        // Without dedup this would be ~100+ entries; bounded to one per path.
        assert!(
            pe.uncommitted_count() <= 2,
            "requeue must dedup per path, got {}",
            pe.uncommitted_count()
        );
    }

    #[test]
    fn requeue_last_kind_wins_per_path() {
        use super::super::overlay::{EditKind, FileEdit};
        let pe = PendingEdits::new();
        // Requeue changed-then-deleted for the same path; deleted must win.
        pe.requeue_uncommitted(vec![
            FileEdit {
                path: PathBuf::from("x.rs"),
                kind: EditKind::Changed,
            },
            FileEdit {
                path: PathBuf::from("x.rs"),
                kind: EditKind::Deleted,
            },
        ]);
        assert_eq!(pe.uncommitted_count(), 1);
        let taken = pe.take_for_commit();
        assert!(taken.newly_deleted.contains(&PathBuf::from("x.rs")));
        assert!(taken.newly_changed.is_empty());
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
