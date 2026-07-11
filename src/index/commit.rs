//! Overlay commit: `Index::commit_batch`.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use super::{
    encoding, helpers, io_util, snapshot, Index, IndexSnapshot, OVERLAY_ENFORCE_THRESHOLD,
    OVERLAY_WARN_THRESHOLD,
};
use crate::index::overlay::{compute_delete_set, OverlayView, PendingEdits};
use crate::index::pending::TakeResult;
use crate::path::PathIndex;
use crate::IndexError;

/// RAII guard that re-queues drained pending edits when dropped, unless the
/// caller disarms it after a successful commit.
///
/// Lives only inside `Index::commit_batch`. If `commit_inner` returns `Err`,
/// the guard is dropped still armed and [`PendingEdits::requeue_uncommitted`]
/// restores the drained edits so they survive to the next commit attempt. If
/// `commit_inner` returns `Ok(())`, the caller sets `take = None` to disarm.
struct RequeueGuard<'a> {
    pending: &'a PendingEdits,
    take: Option<TakeResult>,
}

impl Drop for RequeueGuard<'_> {
    fn drop(&mut self) {
        if let Some(take) = self.take.take() {
            self.pending.requeue_uncommitted(take.drained);
        }
    }
}

/// Outcome of reading one changed file during [`Index::commit_batch`].
enum ChangedFileOutcome {
    /// Content read; add to the overlay.
    Indexed(Arc<[u8]>),
    /// Exclude the file (binary, oversized, escaped the repo, or unreadable).
    /// A per-file failure is excluded rather than aborting the batch, so a
    /// single bad file cannot wedge the incremental pipeline.
    Excluded,
    /// File vanished between `notify_change` and commit; treat as a deletion.
    Vanished,
}

impl Index {
    /// Atomically commit all pending edits. After return, changes are visible
    /// to subsequent queries. In-flight searches see the old snapshot.
    ///
    /// # Failure semantics
    ///
    /// Per-file read failures (oversized file, path escaping the repo, I/O
    /// error) exclude that one file from the index (verbose warning) and never
    /// abort the batch, so a single bad file cannot wedge the pipeline; the
    /// remaining files still commit. Only batch-level failures (overlay-full,
    /// doc_id overflow) abort the commit, in which case the drained pending
    /// edits are re-queued so the next `commit_batch()` retries them.
    pub fn commit_batch(&self) -> Result<(), IndexError> {
        if !self.pending.has_uncommitted() {
            return Ok(());
        }

        // Serialize concurrent writers. _write_lock is held until end of
        // function (underscore prefix suppresses unused-variable lint without
        // triggering the immediate-drop behaviour of bare `_`).
        let _write_lock = helpers::acquire_writer_lock(&self.config.index_dir)?;

        let old_snap = self.snapshot.load_full();
        let take = self.pending.take_for_commit();
        // Re-queue the drained edits on any error path below. `requeue_guard`
        // owns `take` and re-queues unless `commit` completes successfully and
        // disarms it. This covers every error between `take_for_commit()` and
        // the snapshot store: read failures, FileTooLarge, PathOutsideRepo,
        // OverlayFull, OverlayView build errors, and DocIdOverflow.
        let mut requeue_guard = RequeueGuard {
            pending: &self.pending,
            take: Some(take),
        };

        match self.commit_inner(
            &old_snap,
            requeue_guard.take.as_mut().expect("take present"),
        ) {
            Ok(()) => {
                // Success: disarm so the guard drops the drained edits.
                requeue_guard.take = None;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Implementation of [`commit_batch`](Index::commit_batch) operating on an
    /// already-drained [`TakeResult`]. Re-queueing on error is the caller's
    /// responsibility via the `RequeueGuard`.
    fn commit_inner(
        &self,
        old_snap: &IndexSnapshot,
        take: &mut TakeResult,
    ) -> Result<(), IndexError> {
        // Total base doc count for the overlay ratio capacity check.
        let base_doc_count: u32 = old_snap.base_segments().iter().map(|s| s.doc_count).sum();
        let base_doc_id_limit = helpers::base_doc_id_limit(old_snap)?;

        // Read content from disk only for NEWLY changed paths.
        // Unchanged dirty files are reused from the old overlay via Arc::clone.
        let mut new_files: Vec<(std::path::PathBuf, Arc<[u8]>)> = Vec::new();
        let mut excluded_changed = std::collections::HashSet::new();
        // Files that vanished between notify_change() and commit_batch() are
        // treated as deletions rather than hard errors. The TOCTOU window is
        // narrow but real, especially in agent/watch workflows.
        let mut vanished_paths: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        for path in &take.newly_changed {
            match self.classify_changed_file(path) {
                ChangedFileOutcome::Indexed(content) => new_files.push((path.clone(), content)),
                ChangedFileOutcome::Excluded => {
                    excluded_changed.insert(path.clone());
                }
                ChangedFileOutcome::Vanished => {
                    vanished_paths.insert(path.clone());
                }
            }
        }

        let mut visible_changed = take.newly_changed.clone();
        for path in &excluded_changed {
            visible_changed.remove(path);
        }
        // Vanished files are not in new_files; remove them from visible_changed
        // so they don't appear as unresolvable additions.
        for path in &vanished_paths {
            visible_changed.remove(path);
        }

        let mut removed_paths = take.newly_deleted.clone();
        removed_paths.extend(excluded_changed.iter().cloned());
        // Vanished files act as deletions: evict any existing index entry.
        removed_paths.extend(vanished_paths.iter().cloned());

        let projected_overlay_docs = helpers::projected_overlay_doc_count(
            &old_snap.overlay,
            &visible_changed,
            &removed_paths,
        );

        // Enforce hard overlay size limit before rebuilding the overlay. Once
        // the overlay grows beyond 50% of base docs, the rebuild cost is
        // wasted work because callers need a full reindex anyway.
        //
        // An empty base (e.g. an empty / all-ignored / all-binary repo indexed,
        // then files added) skips this check: the overlay IS the index there, so
        // the ratio is undefined. Growth is bounded by available memory and the
        // u32 doc-id space, and surfaced by the verbose warn threshold below.
        if base_doc_count > 0 {
            let ratio = projected_overlay_docs as f64 / base_doc_count as f64;
            if ratio > OVERLAY_ENFORCE_THRESHOLD {
                return Err(IndexError::OverlayFull {
                    overlay_docs: projected_overlay_docs,
                    base_docs: base_doc_count as usize,
                });
            }
        }

        // Capture (path, content) for changed files before `new_files` is moved
        // into the overlay build, so the symbol index can be re-indexed below.
        // Arc::clone is a refcount bump, not a content copy.
        #[cfg(feature = "symbols")]
        let symbol_inputs: Vec<(std::path::PathBuf, Arc<[u8]>)> = if self.symbol_index.is_some() {
            new_files
                .iter()
                .map(|(p, c)| (p.clone(), Arc::clone(c)))
                .collect()
        } else {
            Vec::new()
        };

        let overlay = OverlayView::build_incremental(
            base_doc_id_limit,
            &old_snap.overlay,
            new_files,
            &visible_changed,
            &removed_paths,
        )?;

        debug_assert_eq!(overlay.docs.len(), projected_overlay_docs);

        // Compute delete_set: base doc_ids invalidated by changes.
        // Start from the previous snapshot's delete_set and add only the delta.
        // The base is immutable between full builds, so the delete_set grows
        // monotonically and incremental accumulation is always correct.
        let delete_set = compute_delete_set(
            &old_snap.base.path_doc_ids,
            &take.newly_changed,
            &take.newly_deleted,
            &old_snap.delete_set,
        );

        // Update the path index incrementally from the previous snapshot.
        //
        // This does NOT rewrite `paths.idx` on disk. `paths.idx` (see
        // `index::paths_idx`) caches only the on-disk *base* path index and is
        // written by `build.rs`/`compact.rs`, the only two places that rewrite
        // base segments. `commit_batch` never touches base segments (it only
        // updates the in-memory overlay), so the cached sidecar remains valid
        // for the next `open()` regardless of how many commits ran in between.
        // Writing it here would also defeat the point of the cache: this path
        // runs on every bounded update-on-search commit, so a disk write here
        // would reintroduce the fixed per-search cost the sidecar exists to
        // eliminate.
        let path_index =
            PathIndex::build_incremental(&old_snap.path_index, &removed_paths, &visible_changed);

        let mut overlay_doc_to_file_id = std::collections::HashMap::new();
        for doc in &overlay.docs {
            if let Some(fid) = path_index.file_id(&doc.path) {
                overlay_doc_to_file_id.insert(doc.doc_id, fid);
            }
        }

        let new_snap = Arc::new(snapshot::new_snapshot(
            Arc::clone(&old_snap.base),
            overlay,
            delete_set,
            path_index,
            overlay_doc_to_file_id,
            old_snap.scan_threshold,
        ));

        // Pre-populate all_doc_ids so the first post-commit query doesn't pay rebuild cost.
        new_snap.all_doc_ids();

        self.snapshot.store(new_snap);

        // Incrementally maintain the symbol index (path-keyed). The gram index is
        // already committed above; a symbol DB error here is logged, not fatal,
        // so a symbol hiccup cannot wedge search. delete-then-reindex covers
        // edits and renames; deleted/excluded/vanished paths are just evicted.
        // Symbol writes go straight to on-disk SQLite, so they persist across
        // processes (unlike the in-memory overlay). update_from_git reaches this
        // via commit_batch; compact/full-rebuild refresh symbols separately.
        #[cfg(feature = "symbols")]
        if let Some(sym_idx) = &self.symbol_index {
            let mut to_delete: Vec<&str> = Vec::new();
            for p in &visible_changed {
                if let Some(s) = p.to_str() {
                    to_delete.push(s);
                }
            }
            for p in &removed_paths {
                if let Some(s) = p.to_str() {
                    to_delete.push(s);
                }
            }
            if let Err(e) = sym_idx.delete_for_paths(&to_delete) {
                log::debug!("symbol index delete failed: {e}");
            } else {
                for (path, content) in &symbol_inputs {
                    let path_str = path.to_string_lossy();
                    // file_id is immaterial to lookups (search reads path/line/
                    // name; deletes are path-keyed), so 0 is a safe placeholder.
                    if let Err(e) = sym_idx.index_file(0, &path_str, content) {
                        log::debug!("symbol index failed for {}: {e}", path.display());
                    }
                }
            }
        }

        // Gated on Debug being enabled so the snapshot load + count sums only
        // run when the message can actually surface (preserves the old
        // verbose-only cost).
        if log::log_enabled!(log::Level::Debug) {
            let snap = self.snapshot.load();
            let base_count: u32 = snap.base_segments().iter().map(|s| s.doc_count).sum();
            let overlay_count = snap.overlay.docs.len() as u32;
            if base_count > 0 {
                let ratio = overlay_count as f64 / base_count as f64;
                if ratio > OVERLAY_WARN_THRESHOLD {
                    log::debug!(
                        "overlay is {:.0}% of base ({} overlay, {} base docs); \
                         consider running `st index` to rebuild",
                        ratio * 100.0,
                        overlay_count,
                        base_count
                    );
                }
            }
        }
        Ok(())
    }

    /// Read and classify one changed file for the commit. Never aborts the
    /// batch on a per-file condition: an oversized / escaped / unreadable file
    /// is excluded (verbose warning) so a single bad file cannot wedge the
    /// incremental pipeline. A file that vanished becomes a deletion.
    fn classify_changed_file(&self, path: &Path) -> ChangedFileOutcome {
        let abs = self.config.repo_root.join(path);

        // Open the changed file guaranteed-beneath the repo root: one
        // openat2(RESOLVE_BENEATH) on Linux (atomic containment), else the
        // portable canonicalize + stat + O_NOFOLLOW + fd-verify path. This
        // replaces the inline symlink-swap guard that ran between
        // notify_change() and commit_batch(). On failure, distinguish a file
        // that genuinely vanished (record a deletion) from a transient error or
        // a containment reject (keep the old doc) with a cheap existence probe.
        let file = match io_util::open_beneath_fresh(&self.canonical_root, path) {
            Some(f) => f,
            None if !abs.exists() => return self.vanished(&abs),
            None => return self.skip(&abs, "path changed or could not be securely opened"),
        };

        // Bounded read (max_file_size + 1 sentinel) catches a file that grew
        // past the limit since notify_change(). saturating_add guards against
        // max_file_size == u64::MAX (plain +1 would wrap to 0 and read nothing).
        let mut reader = file.take(self.config.max_file_size.saturating_add(1));
        let mut raw: Vec<u8> = Vec::new();
        if let Err(e) = reader.read_to_end(&mut raw) {
            return self.skip(&abs, &format!("read failed: {e}"));
        }
        if raw.len() as u64 > self.config.max_file_size {
            return self.skip(
                &abs,
                &format!("exceeds max_file_size ({} bytes)", raw.len()),
            );
        }

        let content = encoding::normalize_encoding(&raw);
        if crate::index::walk::is_binary(&content) {
            // Binary files are excluded silently (normal, not a failure).
            return ChangedFileOutcome::Excluded;
        }
        ChangedFileOutcome::Indexed(Arc::from(content.as_ref()))
    }

    /// Debug-log a vanished file and route it to deletion handling.
    fn vanished(&self, abs: &Path) -> ChangedFileOutcome {
        log::debug!(
            "file vanished before indexing, treating as deletion: {}",
            abs.display()
        );
        ChangedFileOutcome::Vanished
    }

    /// Debug-log a per-file failure and exclude the file rather than aborting
    /// the whole batch.
    fn skip(&self, abs: &Path, why: &str) -> ChangedFileOutcome {
        log::debug!(
            "skipping file, excluding from index: {}: {}",
            abs.display(),
            why
        );
        ChangedFileOutcome::Excluded
    }
}
