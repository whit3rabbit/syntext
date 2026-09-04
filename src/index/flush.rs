//! Durable flush of the in-memory overlay, for uncommitted working-tree drift.
//!
//! # The gap this closes
//!
//! `commit_batch` persists nothing. It builds a new in-memory `OverlayView` and
//! swaps it into this process's `ArcSwap<IndexSnapshot>`, and that is all. For a
//! *moved HEAD* that was already fine: `delta::apply_committed_delta_update`
//! follows the commit with a durable delta segment. For *uncommitted drift* it
//! was not. `st update` applied the drift to its own overlay and exited, so
//! unless the overlay crossed its 50%-of-base cap (which forces a full rebuild),
//! nothing reached disk. The next `st search` was a fresh process with an empty
//! overlay: it re-detected the same files, searched stale, and spawned another
//! catch-up child that did the same thing again.
//!
//! [`Index::flush_overlay`] writes that overlay out. It reuses the same
//! machinery as the committed-HEAD path, with one difference, spelled by
//! [`FlushAnchor`]: uncommitted drift must NOT advance `base_commit`, because
//! nothing was committed. Advancing it would make `rebuild_if_stale` (which
//! only fires on `base_commit != HEAD`) skip the real commit when it arrives.
//!
//! # The other half: not re-applying what was flushed
//!
//! Flushing alone is not enough. `git diff HEAD` keeps reporting an
//! uncommitted file forever, so every later search would re-read and re-apply
//! content the index already holds. [`Index::retain_unflushed`] drops those
//! paths using the working-tree anchor written beside the flush. See
//! [`super::worktree_anchor`].

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use super::worktree_anchor::WorktreeAnchor;
use super::{delta_apply, helpers, Index};
use crate::index::manifest::Manifest;
use crate::IndexError;

/// What a flush should do to `Manifest::base_commit`.
#[derive(Debug, Clone)]
pub(super) enum FlushAnchor {
    /// The flush carries content from a newer commit: record it. Used by
    /// `delta::apply_committed_delta_update`.
    AdvanceHead(Option<String>),
    /// The flush carries uncommitted working-tree drift, which belongs to no
    /// commit. Carry the previous `base_commit` forward untouched.
    KeepHead,
}

/// What `commit_batch` observed, kept until the next flush consumes it.
///
/// Accumulates across every commit since the last flush, because several
/// commits can land before one flush writes them all out.
#[derive(Debug, Default)]
pub(super) struct FlushBookkeeping {
    /// When the *earliest* un-flushed commit started reading file content.
    ///
    /// The earliest, not the latest, on purpose. The anchor trusts a path only
    /// when its mtime is safely older than the read, and a bound that holds for
    /// the earliest read holds for every later one too. Taking the latest would
    /// wrongly trust a path read early whose mtime falls between the two.
    pub(super) read_epoch: Option<SystemTime>,
    /// Paths the commit did not turn into documents: deleted, vanished between
    /// `notify_change` and the read, or excluded as binary/oversized. They still
    /// need anchoring, or a deleted tracked file costs a `notify_delete` on
    /// every search until it is committed.
    pub(super) non_doc_paths: HashSet<PathBuf>,
}

impl Index {
    /// Record what a commit observed, for the next flush's anchor.
    pub(super) fn note_commit_for_flush(
        &self,
        read_epoch: SystemTime,
        non_doc_paths: impl IntoIterator<Item = PathBuf>,
    ) {
        let Ok(mut book) = self.flush_book.lock() else {
            // A poisoned lock means a previous commit panicked mid-record. The
            // anchor is an optimization; losing it costs re-applies, not
            // correctness, so there is nothing to escalate here.
            return;
        };
        if book.read_epoch.is_none() {
            book.read_epoch = Some(read_epoch);
        }
        book.non_doc_paths.extend(non_doc_paths);
    }

    /// Drop paths from `paths` that are provably already flushed and unchanged
    /// on disk, returning how many were dropped.
    ///
    /// Called by `update_from_git` before the empty check and the `max_files`
    /// gate, and by `st status`, so both count only work that actually remains.
    /// Loads the anchor lazily on first use: a `--no-update` search never
    /// touches the sidecar at all.
    pub(crate) fn retain_unflushed(&self, paths: &mut HashSet<PathBuf>) -> usize {
        if paths.is_empty() {
            return 0;
        }
        let anchor = self.loaded_anchor();
        if anchor.is_empty() {
            return 0;
        }
        let before = paths.len();
        paths.retain(|rel| !anchor.is_unchanged(&self.canonical_root, rel));
        before - paths.len()
    }

    /// The working-tree anchor for this index, loading it on first use.
    ///
    /// Fails OPEN: any read error yields an empty anchor, which only means
    /// paths get re-applied. Contrast `deletes_idx`, whose loss would produce
    /// duplicate results and therefore fails closed in `open()`.
    fn loaded_anchor(&self) -> Arc<WorktreeAnchor> {
        let mut slot = match self.worktree_anchor.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(anchor) = slot.as_ref() {
            return Arc::clone(anchor);
        }
        let loaded = Arc::new(self.load_worktree_anchor());
        *slot = Some(Arc::clone(&loaded));
        loaded
    }

    fn load_worktree_anchor(&self) -> WorktreeAnchor {
        let Ok(manifest) = Manifest::load(&self.config.index_dir) else {
            return WorktreeAnchor::default();
        };
        let Some(name) = manifest.worktree_anchor_file.as_deref() else {
            return WorktreeAnchor::default();
        };
        match super::worktree_codec::read_worktree_anchor(&self.config.index_dir, name) {
            Ok(anchor) => anchor,
            Err(e) => {
                log::debug!("worktree anchor {name} unreadable ({e}); re-applying every path");
                WorktreeAnchor::default()
            }
        }
    }

    /// Forget the loaded anchor and the pending bookkeeping.
    ///
    /// Called after any operation that replaces the on-disk index (flush,
    /// delta, compaction, full rebuild): the anchor file the manifest names has
    /// changed, and the bookkeeping describes commits that are now durable.
    pub(super) fn reset_flush_state(&self) {
        if let Ok(mut slot) = self.worktree_anchor.lock() {
            *slot = None;
        }
        if let Ok(mut book) = self.flush_book.lock() {
            *book = FlushBookkeeping::default();
        }
    }

    /// Persist the in-memory overlay so a later process sees it.
    ///
    /// Commits anything pending, writes the overlay as a delta segment plus a
    /// delete-set and working-tree anchor sidecar, then compacts if the segment
    /// count crossed its threshold. `base_commit` is left alone: this is
    /// uncommitted drift.
    ///
    /// Returns `Ok(false)` when there was nothing to flush.
    ///
    /// Needs the exclusive directory lock, like every other durable write. A
    /// competing reader or writer surfaces as [`IndexError::LockConflict`],
    /// which is retryable and leaves the overlay intact.
    pub fn flush_overlay(&self) -> Result<bool, IndexError> {
        if self.pending.has_uncommitted() {
            match self.commit_batch() {
                Ok(()) => {}
                // The change set is over the overlay's 50%-of-base cap, so a
                // delta segment is the wrong answer for it and a full rebuild
                // is the right one. An unbounded `update_from_git` has already
                // done that rebuild inline, which is itself durable, and its
                // `install_rebuilt_index` re-queued the edits that are failing
                // here. Reporting this as a flush failure would tell the user
                // to `st index` immediately after an `st index` just ran.
                //
                // `commit_batch`'s RequeueGuard leaves the edits queued, so
                // nothing is dropped. What is genuinely lost is the anchor:
                // a full rebuild writes none, so those paths keep being
                // re-detected until they are committed. See `worktree_anchor`.
                Err(IndexError::OverlayFull { .. }) => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        if self.snapshot().overlay.docs.is_empty() && !self.has_flush_bookkeeping() {
            return Ok(false);
        }
        self.run_durable_flush(FlushAnchor::KeepHead)?;
        // Bound segment growth and physically drop superseded base docs. Only
        // fires when the segment count crosses `max_segments`.
        self.maybe_compact()?;
        Ok(true)
    }

    /// Whether a commit has left anchor bookkeeping that no flush has consumed.
    ///
    /// An empty overlay with pending bookkeeping is a real case: a commit whose
    /// only content was deletions produces no documents but still needs its
    /// `Absent` entries written, or those deletions are re-detected forever.
    fn has_flush_bookkeeping(&self) -> bool {
        self.flush_book
            .lock()
            .map(|book| !book.non_doc_paths.is_empty())
            .unwrap_or(false)
    }

    /// Write the committed overlay out durably and install the result.
    ///
    /// Owns the lock choreography shared by both flush callers: take the writer
    /// lock, capture the just-committed snapshot and the anchor inputs, release
    /// the shared directory lock so the flush can take exclusive and reopen,
    /// then install. On error, re-acquire shared before returning.
    pub(super) fn run_durable_flush(&self, anchor: FlushAnchor) -> Result<(), IndexError> {
        let write_lock = helpers::acquire_writer_lock(&self.config.index_dir)?;
        let snapshot = self.snapshot();
        let inputs = self.take_anchor_inputs();

        self._dir_lock.unlock()?;
        let rebuilt = match delta_apply::flush_overlay_durable(
            self.config.clone(),
            snapshot,
            anchor,
            inputs,
            write_lock,
        ) {
            Ok(rebuilt) => rebuilt,
            Err(err) => {
                if let Err(e) = self._dir_lock.try_lock_shared() {
                    log::debug!(
                        "failed to re-acquire shared directory lock after flush error: {e}"
                    );
                }
                return Err(err);
            }
        };
        helpers::try_lock_shared(&self._dir_lock, &self.config.index_dir)?;

        self.install_rebuilt_index(&rebuilt)?;
        Ok(())
    }

    /// Drain the bookkeeping a flush needs to build the next anchor.
    ///
    /// Drained, not borrowed: a flush that then fails loses the bookkeeping and
    /// so writes no anchor next time either, which costs re-applies and nothing
    /// else. Holding it across a failed flush would be worse, since the
    /// `read_epoch` it carries would no longer describe any content on disk.
    fn take_anchor_inputs(&self) -> AnchorInputs {
        let mut book = match self.flush_book.lock() {
            Ok(book) => book,
            Err(poisoned) => poisoned.into_inner(),
        };
        AnchorInputs {
            read_epoch: book.read_epoch.take(),
            non_doc_paths: std::mem::take(&mut book.non_doc_paths),
            previous: self.loaded_anchor(),
            root: self.canonical_root.clone(),
        }
    }
}

/// Everything a flush needs to write the next working-tree anchor.
pub(super) struct AnchorInputs {
    /// `None` means no commit recorded a read epoch since the last flush, so
    /// nothing can be trusted as settled and no anchor is written.
    pub(super) read_epoch: Option<SystemTime>,
    pub(super) non_doc_paths: HashSet<PathBuf>,
    pub(super) previous: Arc<WorktreeAnchor>,
    pub(super) root: PathBuf,
}
