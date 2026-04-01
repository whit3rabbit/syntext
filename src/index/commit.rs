//! Overlay commit: `Index::commit_batch`.

use std::io::Read;
use std::sync::Arc;

use super::{
    encoding, helpers, io_util, snapshot, Index, OVERLAY_ENFORCE_THRESHOLD, OVERLAY_WARN_THRESHOLD,
};
use crate::index::overlay::{compute_delete_set, OverlayView};
use crate::path::PathIndex;
use crate::IndexError;

impl Index {
    /// Atomically commit all pending edits. After return, changes are visible
    /// to subsequent queries. In-flight searches see the old snapshot.
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

        // Total base doc count for overlay doc_id assignment.
        let base_doc_count: u32 = old_snap.base_segments().iter().map(|s| s.doc_count).sum();
        let base_doc_id_limit = helpers::base_doc_id_limit(&old_snap)?;

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
            let abs = self.config.repo_root.join(path);
            // Canonicalize before opening to detect symlink swaps that occurred
            // between notify_change() and commit_batch(). If the resolved path
            // escapes canonical_root, reject it as PathOutsideRepo.
            let resolved = match std::fs::canonicalize(&abs) {
                Ok(p) => p,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // File deleted between notify_change() and now; treat as deletion.
                    if self.config.verbose {
                        eprintln!(
                            "syntext: warning: file vanished before indexing, treating as deletion: {}",
                            abs.display()
                        );
                    }
                    vanished_paths.insert(path.clone());
                    continue;
                }
                Err(e) => return Err(IndexError::Io(e)),
            };
            if !resolved.starts_with(&self.canonical_root) {
                return Err(IndexError::PathOutsideRepo(abs));
            }
            // Stat before open to record expected inode. After open, fstat the fd
            // and compare dev+ino to catch directory-component symlink swaps that
            // occur in the window between canonicalize() and open() (O_NOFOLLOW
            // only blocks the final component, not intermediate ones).
            #[cfg(any(unix, windows))]
            let pre_open_meta = match std::fs::metadata(&resolved) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if self.config.verbose {
                        eprintln!(
                            "syntext: warning: file vanished before indexing, treating as deletion: {}",
                            abs.display()
                        );
                    }
                    vanished_paths.insert(path.clone());
                    continue;
                }
                Err(e) => return Err(IndexError::Io(e)),
            };
            // Enforce the same max_file_size limit used during full builds.
            // Use bounded read to eliminate TOCTOU race: file can grow between
            // metadata check and read. Read up to max_file_size + 1 bytes to detect overflow.
            let file = match io_util::open_readonly_nofollow(&resolved) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if self.config.verbose {
                        eprintln!(
                            "syntext: warning: file vanished before indexing, treating as deletion: {}",
                            abs.display()
                        );
                    }
                    vanished_paths.insert(path.clone());
                    continue;
                }
                Err(e) => return Err(IndexError::Io(e)),
            };
            #[cfg(any(unix, windows))]
            if !io_util::verify_fd_matches_stat(&file, &pre_open_meta) {
                return Err(IndexError::PathOutsideRepo(abs.clone()));
            }
            // Use saturating_add to guard against max_file_size == u64::MAX:
            // plain `+ 1` would wrap to 0 and read nothing.
            let mut reader = file.take(self.config.max_file_size.saturating_add(1));
            let mut raw: Vec<u8> = Vec::new();
            reader.read_to_end(&mut raw)?;
            if raw.len() as u64 > self.config.max_file_size {
                return Err(IndexError::FileTooLarge {
                    path: abs,
                    size: raw.len() as u64,
                });
            }
            let content = encoding::normalize_encoding(&raw, self.config.verbose);
            if crate::index::walk::is_binary(&content) {
                excluded_changed.insert(path.clone());
                continue;
            }
            new_files.push((path.clone(), Arc::from(content.as_ref())));
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
        if base_doc_count > 0 {
            let ratio = projected_overlay_docs as f64 / base_doc_count as f64;
            if ratio > OVERLAY_ENFORCE_THRESHOLD {
                return Err(IndexError::OverlayFull {
                    overlay_docs: projected_overlay_docs,
                    base_docs: base_doc_count as usize,
                });
            }
        }

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
        let path_index =
            PathIndex::build_incremental(&old_snap.path_index, &removed_paths, &visible_changed);

        let base_doc_to_file_id = Arc::clone(&old_snap.base_doc_to_file_id);
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
            base_doc_to_file_id,
            overlay_doc_to_file_id,
            old_snap.scan_threshold,
        ));

        // Pre-populate all_doc_ids so the first post-commit query doesn't pay rebuild cost.
        new_snap.all_doc_ids();

        self.snapshot.store(new_snap);
        if self.config.verbose {
            let snap = self.snapshot.load();
            let base_count: u32 = snap.base_segments().iter().map(|s| s.doc_count).sum();
            let overlay_count = snap.overlay.docs.len() as u32;
            if base_count > 0 {
                let ratio = overlay_count as f64 / base_count as f64;
                if ratio > OVERLAY_WARN_THRESHOLD {
                    eprintln!(
                        "syntext: warning: overlay is {:.0}% of base ({} overlay, {} base docs); \
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
}
