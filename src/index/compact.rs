//! Snapshot-backed compaction: rewrite fresh base segments from the current
//! in-memory view without rereading unchanged base files from disk.

use std::fs;
use std::sync::Arc;

use fs2::FileExt;
use xxhash_rust::xxh64::xxh64;

use crate::index::manifest::{Manifest, SegmentRef};
use crate::index::segment::SegmentWriter;
use crate::index::snapshot::IndexSnapshot;
use crate::{Config, IndexError};

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

#[derive(Clone, Copy)]
struct CompactedDocTarget {
    segment_idx: usize,
    global_doc_id: u32,
}

struct CompactionState {
    writers: Vec<SegmentWriter>,
    writer_base_doc_ids: Vec<u32>,
    target_map: Vec<Option<CompactedDocTarget>>,
    current_segment_idx: usize,
    current_segment_bytes: u64,
    next_doc_id: u32,
    batch_size_bytes: u64,
}

impl CompactionState {
    fn new(start_doc_id: u32, batch_size_bytes: u64) -> Self {
        Self {
            writers: Vec::new(),
            writer_base_doc_ids: Vec::new(),
            target_map: Vec::new(),
            current_segment_idx: 0,
            current_segment_bytes: 0,
            next_doc_id: start_doc_id,
            batch_size_bytes: batch_size_bytes.max(1),
        }
    }

    fn add_document(
        &mut self,
        old_global_id: u32,
        path: &std::path::Path,
        content_hash: u64,
        size_bytes: u64,
    ) -> Result<(), IndexError> {
        let size_cost = size_bytes.max(1);
        let need_new_segment = !self.writers.is_empty()
            && self.current_segment_bytes > 0
            && self.current_segment_bytes.saturating_add(size_cost) > self.batch_size_bytes;
        if self.writers.is_empty() || need_new_segment {
            self.writers.push(SegmentWriter::new());
            self.writer_base_doc_ids.push(self.next_doc_id);
            self.current_segment_idx = self.writers.len() - 1;
            self.current_segment_bytes = 0;
        }

        let new_global_id = self.next_doc_id;
        let target = CompactedDocTarget {
            segment_idx: self.current_segment_idx,
            global_doc_id: new_global_id,
        };
        self.writers[self.current_segment_idx].add_document(
            new_global_id,
            path,
            content_hash,
            size_bytes,
        );
        if self.target_map.len() <= old_global_id as usize {
            self.target_map.resize(old_global_id as usize + 1, None);
        }
        self.target_map[old_global_id as usize] = Some(target);
        self.current_segment_bytes = self.current_segment_bytes.saturating_add(size_cost);
        self.next_doc_id = self
            .next_doc_id
            .checked_add(1)
            .ok_or(IndexError::DocIdOverflow {
                base_doc_count: self.next_doc_id,
                overlay_docs: 0,
            })?;
        Ok(())
    }
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

fn manifest_segment_bases(segments: &[SegmentRef]) -> Vec<u32> {
    let mut next_base = 0u32;
    let mut bases = Vec::with_capacity(segments.len());
    for segment in segments {
        let base = segment.base_doc_id.unwrap_or(next_base);
        bases.push(base);
        next_base = base.saturating_add(segment.doc_count);
    }
    bases
}

fn manifest_total_docs(segments: &[SegmentRef]) -> u32 {
    segments.iter().map(|segment| segment.doc_count).sum()
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

pub(super) fn compact_index(
    config: Config,
    snapshot: Arc<IndexSnapshot>,
    plan: CompactionPlan,
) -> Result<super::Index, IndexError> {
    fs::create_dir_all(&config.index_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config.index_dir, fs::Permissions::from_mode(0o700))?;
    }

    let lock_path = config.index_dir.join("lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;
    let _write_lock = super::acquire_writer_lock(&config.index_dir)?;
    let previous_manifest = Manifest::load(&config.index_dir)?;
    if plan.suffix_start > previous_manifest.segments.len() {
        return Err(IndexError::CorruptIndex(format!(
            "compaction suffix {} exceeds manifest segment count {}",
            plan.suffix_start,
            previous_manifest.segments.len()
        )));
    }
    let manifest_bases = manifest_segment_bases(&previous_manifest.segments);

    // Consistency guard: snapshot base_ids must agree with manifest bases for
    // the segments we are compacting. A divergence (e.g., manifest written by a
    // concurrent build that the snapshot predates) would cause incorrect
    // global_doc_id assignments in the rewritten segments.
    //
    // This should never fire in normal operation: base segments only change when
    // compact_index or build_index hold the exclusive lock (which we hold here).
    // It is a defence-in-depth assert, not a runtime error path.
    #[cfg(debug_assertions)]
    {
        let check_end = snapshot.base.base_ids.len().min(manifest_bases.len());
        for (idx, manifest_base) in manifest_bases
            .iter()
            .enumerate()
            .take(check_end)
            .skip(plan.suffix_start)
        {
            debug_assert_eq!(
                snapshot.base.base_ids[idx], *manifest_base,
                "snapshot base_id[{idx}]={} diverges from manifest base[{idx}]={} -- \
                 compaction would assign wrong global doc_ids",
                snapshot.base.base_ids[idx], manifest_base,
            );
        }
    }

    let prefix_doc_id_limit = if plan.suffix_start == 0 {
        0
    } else {
        let last_idx = plan.suffix_start - 1;
        manifest_bases[last_idx].saturating_add(previous_manifest.segments[last_idx].doc_count)
    };

    let mut state = CompactionState::new(prefix_doc_id_limit, plan.batch_size_bytes);

    for (seg_idx, seg) in snapshot
        .base
        .segments
        .iter()
        .enumerate()
        .skip(plan.suffix_start)
    {
        let base_id = snapshot
            .base
            .base_ids
            .get(seg_idx)
            .copied()
            .ok_or_else(|| {
                IndexError::CorruptIndex(format!("missing base doc offset for segment {seg_idx}"))
            })?;
        for local_doc_id in 0..seg.doc_count {
            let old_global_id =
                base_id
                    .checked_add(local_doc_id)
                    .ok_or(IndexError::DocIdOverflow {
                        base_doc_count: base_id,
                        overlay_docs: 0,
                    })?;
            if snapshot.delete_set.contains(old_global_id) {
                continue;
            }
            let doc = seg.get_doc(local_doc_id).ok_or_else(|| {
                IndexError::CorruptIndex(format!(
                    "missing doc {local_doc_id} while compacting segment {seg_idx}"
                ))
            })?;
            state.add_document(old_global_id, &doc.path, doc.content_hash, doc.size_bytes)?;
        }
    }

    for doc in &snapshot.overlay.docs {
        state.add_document(
            doc.doc_id,
            &doc.path,
            xxh64(doc.content.as_ref(), 0),
            doc.content.len() as u64,
        )?;
    }

    for seg in snapshot.base.segments.iter().skip(plan.suffix_start) {
        for gram_hash in seg.gram_hashes()? {
            let postings = seg.lookup_gram(gram_hash).ok_or_else(|| {
                IndexError::CorruptIndex(format!("missing postings for gram {gram_hash:#x}"))
            })?;
            let global_ids = postings.to_vec().map_err(|msg| {
                IndexError::CorruptIndex(format!("segment postings for gram {gram_hash:#x}: {msg}"))
            })?;
            for old_global_id in global_ids {
                let Some(Some(target)) = state.target_map.get(old_global_id as usize) else {
                    continue;
                };
                state.writers[target.segment_idx].add_gram_posting(gram_hash, target.global_doc_id);
            }
        }
    }

    for (&gram_hash, doc_ids) in &snapshot.overlay.gram_index {
        for &old_global_id in doc_ids {
            let Some(Some(target)) = state.target_map.get(old_global_id as usize) else {
                continue;
            };
            state.writers[target.segment_idx].add_gram_posting(gram_hash, target.global_doc_id);
        }
    }

    let mut seg_refs: Vec<SegmentRef> = previous_manifest.segments[..plan.suffix_start].to_vec();
    for (seg_ref, &base_doc_id) in seg_refs.iter_mut().zip(manifest_bases.iter()) {
        seg_ref.base_doc_id = Some(base_doc_id);
    }
    for (writer_idx, writer) in state.writers.iter_mut().enumerate() {
        if writer.doc_count() == 0 {
            continue;
        }
        let mut seg_ref: SegmentRef = writer.write_to_dir(&config.index_dir)?.into();
        seg_ref.base_doc_id = state.writer_base_doc_ids.get(writer_idx).copied();
        seg_refs.push(seg_ref);
    }

    let total_docs = manifest_total_docs(&seg_refs);
    let mut manifest = Manifest::new(seg_refs, total_docs);
    manifest.base_commit = super::current_repo_head(&config.repo_root)?;
    manifest.scan_threshold_fraction = Some(snapshot.scan_threshold);
    manifest.save(&config.index_dir)?;
    manifest.gc_orphan_segments(&config.index_dir)?;

    if config.verbose {
        eprintln!(
            "syntext: compacted {} documents into {} segment(s)",
            manifest.total_files_indexed,
            manifest.segments.len()
        );
    }

    lock_file
        .unlock()
        .map_err(|e| IndexError::CorruptIndex(format!("failed to unlock dir lock: {e}")))?;
    lock_file
        .try_lock_shared()
        .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;
    drop(_write_lock);
    super::Index::open_with_lock(config, lock_file)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use roaring::RoaringBitmap;
    use tempfile::TempDir;

    use super::*;
    use crate::index::overlay::OverlayView;
    use crate::index::segment::MmapSegment;
    use crate::index::snapshot::{new_snapshot, BaseSegments};
    use crate::path::PathIndex;

    fn build_snapshot(
        segments: &[Vec<(u32, &'static str, u64)>],
        overlay: OverlayView,
        delete_set: RoaringBitmap,
    ) -> (TempDir, IndexSnapshot) {
        let dir = TempDir::new().unwrap();
        let mut mmap_segments = Vec::new();
        let mut base_ids = Vec::new();
        let mut base_doc_paths: Vec<Option<PathBuf>> = Vec::new();
        let mut path_doc_ids: HashMap<PathBuf, Vec<u32>> = HashMap::new();
        let mut all_paths = Vec::new();
        let mut total_docs = 0u32;

        for (seg_idx, docs) in segments.iter().enumerate() {
            let mut writer = SegmentWriter::new();
            let base_id = docs.first().map(|doc| doc.0).unwrap_or(total_docs);
            base_ids.push(base_id);
            for &(doc_id, path, size_bytes) in docs {
                writer.add_document(doc_id, Path::new(path), doc_id as u64, size_bytes);
                if base_doc_paths.len() <= doc_id as usize {
                    base_doc_paths.resize(doc_id as usize + 1, None);
                }
                base_doc_paths[doc_id as usize] = Some(PathBuf::from(path));
                path_doc_ids
                    .entry(PathBuf::from(path))
                    .or_default()
                    .push(doc_id);
                all_paths.push(PathBuf::from(path));
                total_docs = total_docs.max(doc_id.saturating_add(1));
            }
            let meta = writer
                .write_to_dir(dir.path())
                .unwrap_or_else(|_| panic!("failed to write segment {seg_idx}"));
            mmap_segments.push(
                MmapSegment::open_split(
                    &dir.path().join(&meta.dict_filename),
                    &dir.path().join(&meta.post_filename),
                )
                .unwrap(),
            );
        }

        all_paths.sort_unstable();
        all_paths.dedup();
        let path_index = PathIndex::build(&all_paths);
        let mut doc_to_file_id = vec![u32::MAX; total_docs as usize];
        for (global_doc_id, path) in base_doc_paths.iter().enumerate() {
            if let Some(path) = path {
                if let Some(file_id) = path_index.file_id(path) {
                    doc_to_file_id[global_doc_id] = file_id;
                }
            }
        }

        let snapshot = new_snapshot(
            Arc::new(BaseSegments {
                segments: mmap_segments,
                base_ids,
                base_doc_paths,
                path_doc_ids,
            }),
            overlay,
            delete_set,
            path_index,
            doc_to_file_id,
            0.10,
        );
        (dir, snapshot)
    }

    #[test]
    fn plan_uses_segment_limit_and_snapshot_sizes() {
        let (_dir, snapshot) = build_snapshot(
            &[
                vec![(0, "a.rs", 300_000_000)],
                vec![(1, "b.rs", 400_000_000)],
                vec![(2, "c.rs", 500_000_000)],
            ],
            OverlayView::empty(),
            RoaringBitmap::new(),
        );
        let config = Config {
            max_segments: 2,
            ..Config::default()
        };

        let plan = plan(&snapshot, &config).unwrap();
        assert_eq!(plan.reason, CompactionReason::SegmentLimit);
        assert_eq!(plan.suffix_start, 1);
        assert_eq!(plan.target_segments, 1);
        assert_eq!(plan.batch_size_bytes, 900_000_000);
    }

    #[test]
    fn plan_ignores_deleted_base_docs_when_sizing() {
        let mut delete_set = RoaringBitmap::new();
        delete_set.insert(0);
        let (_dir, snapshot) = build_snapshot(
            &[
                vec![(0, "a.rs", 300_000_000)],
                vec![(1, "b.rs", 500_000_000)],
            ],
            OverlayView::empty(),
            delete_set,
        );
        let config = Config {
            max_segments: 1,
            ..Config::default()
        };

        let plan = plan(&snapshot, &config).unwrap();
        assert_eq!(plan.reason, CompactionReason::SegmentLimit);
        assert_eq!(plan.suffix_start, 0);
        assert_eq!(plan.batch_size_bytes, 500_000_000);
    }

    #[test]
    fn plan_prioritizes_overlay_ratio_trigger() {
        let overlay = OverlayView::build(
            10,
            vec![
                (
                    PathBuf::from("dirty_1.rs"),
                    Arc::from(&b"fn dirty_1() {}\n"[..]),
                ),
                (
                    PathBuf::from("dirty_2.rs"),
                    Arc::from(&b"fn dirty_2() {}\n"[..]),
                ),
            ],
        )
        .unwrap();
        let (_dir, snapshot) = build_snapshot(
            &[
                vec![(0, "base_0.rs", 10)],
                vec![(1, "base_1.rs", 10)],
                vec![(2, "base_2.rs", 10)],
                vec![(3, "base_3.rs", 10)],
                vec![(4, "base_4.rs", 10)],
                vec![(5, "base_5.rs", 10)],
                vec![(6, "base_6.rs", 10)],
                vec![(7, "base_7.rs", 10)],
                vec![(8, "base_8.rs", 10)],
                vec![(9, "base_9.rs", 10)],
            ],
            overlay,
            RoaringBitmap::new(),
        );
        let config = Config {
            max_segments: 20,
            ..Config::default()
        };

        let plan = plan(&snapshot, &config).unwrap();
        assert_eq!(plan.reason, CompactionReason::OverlayRatio);
        assert_eq!(plan.suffix_start, 10);
        assert_eq!(plan.target_segments, 1);
        assert_eq!(plan.batch_size_bytes, super::super::build::BATCH_SIZE_BYTES);
    }

    #[test]
    fn forced_plan_rewrites_from_earliest_deleted_segment() {
        let mut delete_set = RoaringBitmap::new();
        delete_set.insert(1);
        let (_dir, snapshot) = build_snapshot(
            &[
                vec![(0, "a.rs", 10)],
                vec![(1, "b.rs", 10)],
                vec![(2, "c.rs", 10)],
            ],
            OverlayView::empty(),
            delete_set,
        );
        let config = Config {
            max_segments: 10,
            ..Config::default()
        };

        let plan = forced_plan(&snapshot, &config).unwrap();
        assert_eq!(plan.reason, CompactionReason::ExplicitRequest);
        assert_eq!(plan.suffix_start, 1);
        assert_eq!(plan.target_segments, 1);
    }
}
