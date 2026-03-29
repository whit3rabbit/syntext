//! Compaction planning: decide whether, where, and how much to compact.

use crate::index::snapshot::IndexSnapshot;
use crate::Config;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompactionReason {
    OverlayRatio,
    SegmentLimit,
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
            (0..seg.doc_count)
                .filter_map(|local_doc_id| {
                    let global_doc_id = base_id.checked_add(local_doc_id)?;
                    if snapshot.delete_set.contains(global_doc_id) {
                        return Some(0);
                    }
                    Some(seg.get_doc(local_doc_id)?.size_bytes)
                })
                .sum::<u64>()
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
    if !overlay_ratio_exceeded && !segment_limit_exceeded {
        return None;
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
