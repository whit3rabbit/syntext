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

use xxhash_rust::xxh64::xxh64;

use super::flush::{AnchorInputs, FlushAnchor};
use super::worktree_anchor::WorktreeAnchor;
use super::{deletes_idx, helpers, paths_idx, worktree_codec, Index};
use crate::index::manifest::{Manifest, SegmentRef};
use crate::index::segment::SegmentWriter;
use crate::index::snapshot::IndexSnapshot;
use crate::tokenizer::build_all;
use crate::{Config, IndexError};

/// Write the committed overlay as a durable delta segment + persistent
/// delete-set + working-tree anchor, then reopen the index.
///
/// `anchor` decides what happens to `base_commit`. `inputs` carries what the
/// commits since the last flush observed, used to build the next working-tree
/// anchor. `write_lock` is the writer lock acquired by the caller before
/// snapshotting; it is held for the duration of the write.
pub(super) fn flush_overlay_durable(
    config: Config,
    snapshot: Arc<IndexSnapshot>,
    anchor: FlushAnchor,
    inputs: AnchorInputs,
    write_lock: std::fs::File,
) -> Result<Index, IndexError> {
    helpers::create_dir_all_secure(&config.index_dir)?;

    let lock_file = helpers::open_dir_lock_file(&config.index_dir)?;
    helpers::try_lock_exclusive(&lock_file, &config.index_dir)?;
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
    let flushed_paths: std::collections::HashSet<std::path::PathBuf> = snapshot
        .overlay
        .docs
        .iter()
        .map(|d| d.path.clone())
        .collect();
    if overlay_doc_count > 0 {
        let mut docs: Vec<&crate::index::overlay::OverlayDoc> =
            snapshot.overlay.docs.iter().collect();
        // SegmentWriter requires strictly-increasing doc_ids; overlay ids are a
        // contiguous range above the base (assigned from base_doc_id_limit), so
        // sorting yields a gap-free ascending run.
        docs.sort_unstable_by_key(|d| d.doc_id);
        let first_doc_id = docs[0].doc_id;

        let mut writer = SegmentWriter::with_capacity(docs.len(), 120);
        for doc in &docs {
            let content_hash = xxh64(doc.content.as_ref(), 0);
            writer.add_document(
                doc.doc_id,
                &doc.path,
                content_hash,
                doc.content.len() as u64,
            );
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

    // Refresh paths.idx so a reopen with a matching version sees the correct
    // path set for `--files`/path filters. The snapshot's path index came from
    // `build_incremental`, which preserves STABLE (tombstoned, non-positional)
    // file_ids; but the paths.idx on-disk format assumes POSITIONAL file_ids
    // (file_id == index in the sorted path list, which is how the sidecar's
    // extension/component bitmaps are keyed and how `read_paths_idx` reassigns
    // ids). Persisting the stable-id index directly would round-trip to an
    // internally inconsistent PathIndex (bitmaps referencing ids that no longer
    // match path positions), corrupting `--files`/path-filter results after a
    // cross-process reopen. So rebuild a positional index over the live path
    // set (already sorted+deduped by build_incremental) before writing. This is
    // safe because on reopen `base_doc_to_file_id` is re-derived from the loaded
    // index by path lookup, so the writer's in-memory stable ids need not
    // survive to disk.
    // Record what the working tree looked like for everything this flush made
    // durable, so `retain_unflushed` can skip these paths until they change
    // again. Best-effort in both directions: no read epoch (no commit since the
    // last flush recorded one), too many entries, or a write failure all just
    // mean the paths get re-applied next time. See `worktree_anchor`.
    let worktree_anchor_file = write_anchor(&config, &inputs, &flushed_paths);

    let live_paths: Vec<std::path::PathBuf> = snapshot
        .path_index
        .paths
        .iter()
        .map(|p| p.to_path_buf())
        .collect();
    let positional_index = crate::path::PathIndex::build(&live_paths);
    let mut paths_idx_ok = false;
    if let Err(e) = paths_idx::write_paths_idx(&config.index_dir, &positional_index) {
        log::debug!("could not write paths.idx cache: {e}");
    } else {
        paths_idx_ok = true;
    }

    let total_files = previous_manifest
        .total_files_indexed
        .saturating_add(overlay_doc_count);
    let mut manifest = Manifest::new(seg_refs, total_files);
    manifest.base_commit = match anchor {
        FlushAnchor::AdvanceHead(head) => head,
        // Uncommitted drift belongs to no commit. Advancing base_commit here
        // would make `rebuild_if_stale` (which only fires on
        // `base_commit != HEAD`) skip the real commit when it lands.
        FlushAnchor::KeepHead => previous_manifest.base_commit.clone(),
    };
    manifest.scan_threshold_fraction = previous_manifest.scan_threshold_fraction;
    manifest.paths_idx_version = if paths_idx_ok {
        Some(paths_idx::FORMAT_VERSION)
    } else {
        None
    };
    manifest.overlay_deletes_file = deletes_file;
    manifest.worktree_anchor_file = worktree_anchor_file;
    // One generation per durable flush. The field predates this and was
    // documented as reserved; it now counts flushes, which is what a test can
    // assert a flush actually happened by.
    manifest.overlay_gen = previous_manifest.overlay_gen.saturating_add(1);
    manifest.save(&config.index_dir)?;
    // Removes orphan segments, stale deletes-*.idx and stale worktree-*.idx
    // (all but the ones named in the manifest above).
    manifest.gc_orphan_segments(&config.index_dir)?;

    // Same lock-downgrade dance as build_index/compact_index: flock has no
    // atomic EX -> SH downgrade, so a competing writer could grab EX briefly
    // between unlock and try_lock_shared; it fails at write.lock (still held)
    // and releases. _write_lock is dropped only after the shared lock is held.
    lock_file
        .unlock()
        .map_err(|e| IndexError::CorruptIndex(format!("failed to unlock dir lock: {e}")))?;
    helpers::try_lock_shared(&lock_file, &config.index_dir)?;
    drop(_write_lock);
    Index::open_with_lock(config, lock_file)
}

/// Build and persist the next working-tree anchor, returning the filename to
/// record in the manifest.
///
/// `None` on every failure path, which drops the anchor entirely and costs
/// re-applies rather than correctness. `read_epoch` is `None` when no commit
/// since the last flush recorded one, and without it nothing can be judged
/// settled (see the racy-mtime rule in `worktree_anchor`).
fn write_anchor(
    config: &Config,
    inputs: &AnchorInputs,
    flushed_paths: &std::collections::HashSet<std::path::PathBuf>,
) -> Option<String> {
    let read_epoch = inputs.read_epoch?;
    let next = WorktreeAnchor::build_next(
        &inputs.previous,
        flushed_paths,
        &inputs.non_doc_paths,
        &inputs.root,
        read_epoch,
    )?;
    if next.is_empty() {
        return None;
    }
    let name = worktree_codec::new_filename();
    match worktree_codec::write_worktree_anchor(&config.index_dir, &name, &next) {
        Ok(()) => {
            log::debug!("anchored {} working-tree path(s) in {name}", next.len());
            Some(name)
        }
        Err(e) => {
            log::debug!("could not write worktree anchor: {e}");
            None
        }
    }
}
