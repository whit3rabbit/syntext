//! Snapshot-backed compaction: rewrite fresh base segments from the current
//! in-memory view without rereading unchanged base files from disk.

use std::fs;
use std::sync::Arc;

#[cfg(feature = "fs2")]
use fs2::FileExt;
use xxhash_rust::xxh64::xxh64;

use crate::index::manifest::{Manifest, SegmentRef};
use crate::index::segment::SegmentWriter;
use crate::index::snapshot::IndexSnapshot;
use crate::{Config, IndexError};

#[allow(unused_imports)] // CompactionReason used by #[cfg(test)] via `use super::*`
pub(super) use super::compact_plan::{forced_plan, plan, CompactionPlan, CompactionReason};

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

fn validate_snapshot_matches_manifest(
    snapshot: &IndexSnapshot,
    manifest_bases: &[u32],
) -> Result<(), IndexError> {
    let snapshot_segments = snapshot.base.segments.len();
    let manifest_segments = manifest_bases.len();
    if snapshot_segments != manifest_segments {
        return Err(IndexError::CorruptIndex(format!(
            "snapshot segment count {} diverges from manifest segment count {}",
            snapshot_segments, manifest_segments
        )));
    }

    let snapshot_base_ids = snapshot.base.base_ids.len();
    if snapshot_base_ids != snapshot_segments {
        return Err(IndexError::CorruptIndex(format!(
            "snapshot base_ids length {} diverges from snapshot segment count {}",
            snapshot_base_ids, snapshot_segments
        )));
    }

    for (idx, &manifest_base) in manifest_bases.iter().enumerate() {
        let snapshot_base = snapshot.base.base_ids[idx];
        if snapshot_base != manifest_base {
            return Err(IndexError::CorruptIndex(format!(
                "snapshot base_id[{idx}]={} diverges from manifest base[{idx}]={}",
                snapshot_base, manifest_base
            )));
        }
    }

    Ok(())
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
    let _write_lock = super::helpers::acquire_writer_lock(&config.index_dir)?;
    let previous_manifest = Manifest::load(&config.index_dir)?;
    if plan.suffix_start > previous_manifest.segments.len() {
        return Err(IndexError::CorruptIndex(format!(
            "compaction suffix {} exceeds manifest segment count {}",
            plan.suffix_start,
            previous_manifest.segments.len()
        )));
    }
    let manifest_bases = manifest_segment_bases(&previous_manifest.segments);
    // Consistency guard: snapshot base_ids must agree with manifest bases
    // before we rewrite any segment. A divergence means the snapshot predates
    // some other rebuild and compacting it would assign incorrect global doc_ids.
    validate_snapshot_matches_manifest(snapshot.as_ref(), &manifest_bases)?;

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
    manifest.base_commit = super::helpers::current_repo_head(&config.repo_root)?;
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
#[path = "compact_tests.rs"]
mod tests;
