//! Compaction planning: decide whether, where, and how much to compact.

use crate::index::snapshot::IndexSnapshot;
use crate::Config;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompactionReason {
    OverlayRatio,
    SegmentLimit,
    /// Stable file_id high-water mark far exceeds live path count (delete+recreate
    /// churn in a long-lived process that never crossed the two ratio triggers).
    FileIdBloat,
    ExplicitRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CompactionPlan {
    pub reason: CompactionReason,
    pub suffix_start: usize,
    pub batch_size_bytes: u64,
    pub target_segments: usize,
}

fn live_snapshot_bytes_from(snapshot: &IndexSnapshot, suffix_start: usize) -> u64 {
    let base_bytes: u64 = snapshot
        .base
        .segments
        .iter()
        .enumerate()
        .skip(suffix_start)
        .map(|(seg_idx, seg)| {
            let base_id = snapshot.base.base_ids.get(seg_idx).copied().unwrap_or(0);
            if let Some(total_bytes) = seg.doc_bytes {
                let mut deleted_bytes = 0;
                let start_id = base_id;
                let end_id = base_id + seg.doc_count;
                if !snapshot.delete_set.is_empty() {
                    for global_doc_id in snapshot.delete_set.iter() {
                        if global_doc_id < start_id {
                            continue;
                        }
                        if global_doc_id >= end_id {
                            break;
                        }
                        if let Some(local_doc_id) = global_doc_id.checked_sub(base_id) {
                            if let Some(doc) = seg.get_doc(local_doc_id) {
                                deleted_bytes += doc.size_bytes;
                            }
                        }
                    }
                }
                total_bytes.saturating_sub(deleted_bytes)
            } else {
                (0..seg.doc_count)
                    .filter_map(|local_doc_id| {
                        let global_doc_id = base_id.checked_add(local_doc_id)?;
                        if snapshot.delete_set.contains(global_doc_id) {
                            return Some(0);
                        }
                        Some(seg.get_doc(local_doc_id)?.size_bytes)
                    })
                    .sum::<u64>()
            }
        })
        .sum();
    let overlay_bytes: u64 = snapshot
        .overlay
        .docs
        .iter()
        .map(|doc| doc.content.len() as u64)
        .sum();
    base_bytes.saturating_add(overlay_bytes)
}

fn first_deleted_segment(snapshot: &IndexSnapshot) -> Option<usize> {
    snapshot.delete_set.iter().next().map(|global_doc_id| {
        snapshot
            .segment_base_ids()
            .partition_point(|&base_id| base_id <= global_doc_id)
            .saturating_sub(1)
    })
}

fn batch_size_for_plan(snapshot: &IndexSnapshot, plan: &CompactionPlan) -> u64 {
    let total_bytes = live_snapshot_bytes_from(snapshot, plan.suffix_start);
    if total_bytes == 0 {
        return super::build::BATCH_SIZE_BYTES;
    }

    let target_segments = u64::try_from(plan.target_segments.max(1)).unwrap_or(u64::MAX);
    let target = total_bytes.saturating_add(target_segments - 1) / target_segments;
    target.max(super::build::BATCH_SIZE_BYTES).max(1)
}

fn make_plan(
    snapshot: &IndexSnapshot,
    reason: CompactionReason,
    suffix_start: usize,
    target_segments: usize,
) -> CompactionPlan {
    let mut plan = CompactionPlan {
        reason,
        suffix_start,
        batch_size_bytes: super::build::BATCH_SIZE_BYTES,
        target_segments: target_segments.max(1),
    };
    plan.batch_size_bytes = batch_size_for_plan(snapshot, &plan);
    plan
}

pub(super) fn plan(snapshot: &IndexSnapshot, config: &Config) -> Option<CompactionPlan> {
    let base_docs: usize = snapshot
        .base_segments()
        .iter()
        .map(|seg| seg.doc_count as usize)
        .sum();
    let overlay_docs = snapshot.overlay.docs.len();
    let overlay_ratio_exceeded = if base_docs == 0 {
        overlay_docs > 0
    } else {
        overlay_docs as f64 / base_docs as f64 > 0.10
    };
    let segment_limit_exceeded = snapshot.base.segments.len() > config.max_segments.max(1);
    // Backstop the one growth window the two ratio triggers miss: repeated
    // delete+recreate of the same path in a long-lived process (base_commit ==
    // HEAD, so nothing flushes to a delta segment) keeps overlay_docs and the
    // segment count flat while burning a fresh stable file_id + a tombstone in
    // PathIndex.file_id_to_path every cycle. Fire when the file_id high-water
    // mark runs 4x ahead of the live path count. The 4x margin and the 1024
    // floor keep this from firing before the overlay/segment triggers on normal
    // add-heavy workloads (there, next_file_id ~ live + overlay_docs, and the
    // overlay ratio crosses 0.10 long before next reaches 4x live) and from
    // thrashing tiny repos.
    let file_id_bloat = {
        let next = snapshot.path_index.next_file_id() as usize;
        let live = snapshot.path_index.live_path_count();
        next > 1024 && next > live.saturating_mul(4)
    };
    if !overlay_ratio_exceeded && !segment_limit_exceeded && !file_id_bloat {
        return None;
    }

    // file_id_bloat with neither ratio trigger firing: run a full renumber
    // (suffix_start = 0, target = 1). Compaction rebuilds PathIndex from the
    // live paths it walks, so only a full-suffix compaction is guaranteed to
    // drop every tombstone and reset next_file_id.
    if !overlay_ratio_exceeded && !segment_limit_exceeded {
        return Some(make_plan(snapshot, CompactionReason::FileIdBloat, 0, 1));
    }

    let segment_count = snapshot.base.segments.len();
    let delete_start = first_deleted_segment(snapshot).unwrap_or(segment_count);
    let limit_start = if segment_limit_exceeded {
        config
            .max_segments
            .max(1)
            .saturating_sub(1)
            .min(segment_count)
    } else {
        segment_count
    };
    let suffix_start = delete_start.min(limit_start);
    let target_segments = if segment_limit_exceeded {
        config
            .max_segments
            .max(1)
            .saturating_sub(suffix_start)
            .max(1)
    } else {
        1
    };
    Some(make_plan(
        snapshot,
        if overlay_ratio_exceeded {
            CompactionReason::OverlayRatio
        } else {
            CompactionReason::SegmentLimit
        },
        suffix_start,
        target_segments,
    ))
}

pub(super) fn forced_plan(snapshot: &IndexSnapshot, config: &Config) -> Option<CompactionPlan> {
    if let Some(plan) = plan(snapshot, config) {
        return Some(plan);
    }

    if snapshot.overlay.docs.is_empty() && snapshot.delete_set.is_empty() {
        return None;
    }

    Some(make_plan(
        snapshot,
        CompactionReason::ExplicitRequest,
        first_deleted_segment(snapshot).unwrap_or(snapshot.base.segments.len()),
        1,
    ))
}
