//! Flush an in-memory overlay to a durable delta segment.
//!
//! Called by `delta::Index::apply_committed_delta_update` after `commit_batch`
//! has built the overlay (new/modified docs) and the delete-set (superseded and
//! deleted base doc_ids) in memory. This writes the overlay docs as one new
//! base segment, persists the delete-set to a checksummed sidecar
//! (`deletes_idx`), refreshes `paths.idx`, and saves the manifest last, then
//! reopens the index so the delta docs become part of the durable base and the
//! overlay resets to empty. The result is visible to a later `st search`
//! process — the whole point of the exercise.
//!
//! Split from `delta.rs` to keep both files under the 400-line quality gate.

use std::sync::Arc;

#[cfg(feature = "fs2")]
use fs2::FileExt;
use xxhash_rust::xxh64::xxh64;

use super::{deletes_idx, helpers, paths_idx, Index};
use crate::index::manifest::{Manifest, SegmentRef};
use crate::index::segment::SegmentWriter;
use crate::index::snapshot::IndexSnapshot;
use crate::tokenizer::build_all;
use crate::{Config, IndexError};

/// Write the committed overlay as a durable delta segment + persistent
/// delete-set, then reopen the index.
///
/// `head` is the git HEAD the delta advances the index to (recorded as the new
/// `base_commit`). `write_lock` is the writer lock acquired by the caller
/// before snapshotting; it is held for the duration of the write.
pub(super) fn flush_overlay_as_delta(
    config: Config,
    snapshot: Arc<IndexSnapshot>,
    head: Option<String>,
    write_lock: std::fs::File,
) -> Result<Index, IndexError> {
    helpers::create_dir_all_secure(&config.index_dir)?;

    let lock_file = helpers::open_dir_lock_file(&config.index_dir)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;
    let _write_lock = write_lock;

    let previous_manifest = Manifest::load(&config.index_dir)?;

    // Consistency guard (same intent as compact's validate_snapshot_matches_manifest):
    // the snapshot must describe the same base as the manifest we are extending,
    // or a concurrent rebuild slipped in and the overlay doc_ids/delete_set no
    // longer line up. Bail so the caller falls back to a full rebuild.
    let base_total = previous_manifest.total_docs();
    let snapshot_base_total: u32 = snapshot.base.segments.iter().map(|s| s.doc_count).sum();
    if snapshot.base.segments.len() != previous_manifest.segments.len()
        || snapshot_base_total != base_total
    {
        return Err(IndexError::CorruptIndex(
            "index changed under a delta apply; falling back to rebuild".to_string(),
        ));
    }

    let mut seg_refs: Vec<SegmentRef> = previous_manifest.segments.clone();

    // Write the overlay docs as one delta segment. A single commit's delta is
    // bounded by `DELTA_MAX_FILES`, so one segment is enough; over-cap change
    // sets take the full-rebuild path instead of arriving here.
    let overlay_doc_count = snapshot.overlay.docs.len() as u32;
    if overlay_doc_count > 0 {
        let mut docs: Vec<&crate::index::overlay::OverlayDoc> = snapshot.overlay.docs.iter().collect();
        // SegmentWriter requires strictly-increasing doc_ids; overlay ids are a
        // contiguous range above the base (assigned from base_doc_id_limit), so
        // sorting yields a gap-free ascending run.
        docs.sort_unstable_by_key(|d| d.doc_id);
        let first_doc_id = docs[0].doc_id;

        let mut writer = SegmentWriter::with_capacity(docs.len(), 120);
        for doc in &docs {
            let content_hash = xxh64(doc.content.as_ref(), 0);
            writer.add_document(doc.doc_id, &doc.path, content_hash, doc.content.len() as u64);
            // Re-derive distinct grams from the in-memory content (no disk
            // re-read, no TOCTOU); matches build.rs's dedup.
            let distinct: std::collections::HashSet<u64> =
                build_all(doc.content.as_ref()).into_iter().collect();
            for gram in distinct {
                writer.add_gram_posting(gram, doc.doc_id);
            }
        }
        let mut seg_ref: SegmentRef = writer.write_to_dir(&config.index_dir)?.into();
        seg_ref.base_doc_id = Some(first_doc_id);
        seg_refs.push(seg_ref);
    }

    // Persist the accumulated delete-set (base doc_ids superseded/removed by
    // this and prior deltas). Generation-named so a crash before the manifest
    // save leaves the previous file intact for the previous manifest.
    let deletes_file = if snapshot.delete_set.is_empty() {
        None
    } else {
        let name = deletes_idx::new_filename();
        deletes_idx::write_deletes_idx(&config.index_dir, &name, &snapshot.delete_set)?;
        Some(name)
    };

    // Refresh paths.idx from the snapshot's already-incremental path index
    // (commit_batch removed deleted paths and added new ones), so a reopen with
    // a matching version sees the correct path set for `--files`/path filters.
    if let Err(e) = paths_idx::write_paths_idx(&config.index_dir, &snapshot.path_index) {
        if config.verbose {
            eprintln!("syntext: warning: could not write paths.idx cache: {e}");
        }
    }

    let total_files = previous_manifest
        .total_files_indexed
        .saturating_add(overlay_doc_count);
    let mut manifest = Manifest::new(seg_refs, total_files);
    manifest.base_commit = head;
    manifest.scan_threshold_fraction = previous_manifest.scan_threshold_fraction;
    manifest.paths_idx_version = Some(paths_idx::FORMAT_VERSION);
    manifest.overlay_deletes_file = deletes_file;
    manifest.save(&config.index_dir)?;
    // Removes orphan segments and stale deletes-*.idx (all but the one named in
    // overlay_deletes_file above).
    manifest.gc_orphan_segments(&config.index_dir)?;

    // Same lock-downgrade dance as build_index/compact_index: flock has no
    // atomic EX -> SH downgrade, so a competing writer could grab EX briefly
    // between unlock and try_lock_shared; it fails at write.lock (still held)
    // and releases. _write_lock is dropped only after the shared lock is held.
    lock_file
        .unlock()
        .map_err(|e| IndexError::CorruptIndex(format!("failed to unlock dir lock: {e}")))?;
    lock_file
        .try_lock_shared()
        .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;
    drop(_write_lock);
    Index::open_with_lock(config, lock_file)
}
