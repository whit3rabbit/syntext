//! Index builder and reader.
//!
//! `Index::build()` walks the repository, extracts sparse n-grams in parallel,
//! writes immutable RPLX segments, and saves a manifest.
//!
//! `Index::open()` loads the manifest, mmaps existing segments, and makes the
//! index ready for search.

pub mod manifest;
pub mod overlay;
pub mod segment;
mod stats;
pub mod walk;

pub use walk::is_binary;

use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;
use rayon::prelude::*;
use roaring::RoaringBitmap;
use xxhash_rust::xxh64::xxh64;

use crate::index::manifest::{Manifest, SegmentRef};
use crate::index::overlay::{compute_delete_set, OverlayView, PendingEdits};
use crate::index::segment::{MmapSegment, SegmentWriter};
use crate::index::walk::{enumerate_files, split_batches};
use crate::path::PathIndex;
use crate::tokenizer::build_all;
use crate::{Config, IndexError, IndexStats, SearchMatch, SearchOptions};

/// Target batch size (content bytes) before flushing a segment.
const BATCH_SIZE_BYTES: u64 = 256 * 1024 * 1024;

/// Shared base segments (Arc-shared across snapshot swaps).
pub struct BaseSegments {
    pub segments: Vec<MmapSegment>,
    pub base_ids: Vec<u32>,
}

pub struct IndexSnapshot {
    /// Shared base segments (immutable between full rebuilds).
    pub base: Arc<BaseSegments>,
    /// In-memory gram index for dirty (not yet flushed) files.
    pub overlay: OverlayView,
    /// Base doc_ids invalidated by overlay changes (modified/deleted files).
    pub delete_set: RoaringBitmap,
    /// Roaring-bitmap component index for path-scoped queries.
    pub path_index: PathIndex,
    /// Maps global doc_id -> PathIndex file_id for O(1) path filter lookup.
    /// Value is u32::MAX for docs with no PathIndex entry.
    pub doc_to_file_id: Vec<u32>,
    /// Cached bitmap of all valid doc IDs. Lazy-initialized on first access.
    all_doc_ids_cache: OnceLock<RoaringBitmap>,
}

impl IndexSnapshot {
    pub fn base_segments(&self) -> &[MmapSegment] { &self.base.segments }
    pub fn segment_base_ids(&self) -> &[u32] { &self.base.base_ids }

    /// All valid global doc IDs (base minus deleted, plus overlay). Cached.
    pub fn all_doc_ids(&self) -> &RoaringBitmap {
        self.all_doc_ids_cache.get_or_init(|| {
            let mut bm = RoaringBitmap::new();
            for (seg_idx, seg) in self.base.segments.iter().enumerate() {
                let base = self.base.base_ids.get(seg_idx).copied().unwrap_or(0);
                for local in 0..seg.doc_count {
                    let global = base + local;
                    if !self.delete_set.contains(global) {
                        bm.insert(global);
                    }
                }
            }
            for doc in &self.overlay.docs {
                bm.insert(doc.doc_id);
            }
            bm
        })
    }
}

/// Top-level index handle. Thread-safe via `ArcSwap<IndexSnapshot>`.
pub struct Index {
    pub config: Config,
    snapshot: ArcSwap<IndexSnapshot>,
    pending: PendingEdits,
}

impl Index {
    /// Build the index from scratch, writing segments and a manifest.
    /// Respects `.gitignore`, skips binary files and files exceeding
    /// `config.max_file_size`.
    pub fn build(config: Config) -> Result<Self, IndexError> {
        fs::create_dir_all(&config.index_dir)?;

        // Enumerate all candidate files, sorted by relative path.
        let file_list = enumerate_files(&config)?;
        let total_candidate = file_list.len();
        eprintln!(
            "ripline: indexing {} candidate files",
            total_candidate
        );

        // Split into ~256MB batches and process each.
        let batches = split_batches(&file_list, BATCH_SIZE_BYTES);
        let mut seg_refs: Vec<SegmentRef> = Vec::new();
        let mut indexed_paths: Vec<String> = Vec::new();
        let mut next_doc_id: u32 = 0;

        for batch in &batches {
            // Parallel: read file content and extract grams.
            // results[i] is None if file i was binary or could not be read.
            let results: Vec<Option<(u64, Vec<u64>)>> = batch
                .par_iter()
                .map(|(abs_path, _, _)| {
                    let content = fs::read(abs_path).ok()?;
                    if is_binary(&content) {
                        return None;
                    }
                    let hash = xxh64(&content, 0);
                    Some((hash, build_all(&content)))
                })
                .collect();

            let mut writer = SegmentWriter::new();
            for ((abs_path, rel_path, size), result) in batch.iter().zip(results.iter()) {
                if let Some((content_hash, grams)) = result {
                    let doc_id = next_doc_id;
                    next_doc_id += 1;
                    writer.add_document(doc_id, rel_path, *content_hash, *size);
                    for &gram_hash in grams {
                        writer.add_gram_posting(gram_hash, doc_id);
                    }
                    indexed_paths.push(rel_path.clone());
                } else {
                    // File was binary or unreadable; log at trace level if verbose.
                    let _ = abs_path;
                }
            }

            if writer.doc_count() == 0 {
                continue; // Empty batch (all files were binary/unreadable).
            }

            let meta = writer.write_to_dir(&config.index_dir)?;
            let seg_path = config.index_dir.join(&meta.filename);

            // Sanity check: the posting/dictionary overhead should not exceed 50% of
            // the raw content size. Larger ratios indicate an unexpectedly dense gram
            // distribution and may signal a tokenizer or threshold misconfiguration.
            let content_size: u64 = batch
                .iter()
                .zip(results.iter())
                .filter_map(|((_, _, size), r)| r.as_ref().map(|_| size))
                .sum();
            let seg_size = fs::metadata(&seg_path).map(|m| m.len()).unwrap_or(0);
            if seg_size > content_size / 2 && content_size > 0 {
                eprintln!(
                    "ripline: warning: segment is {seg_size} bytes for {content_size} bytes content"
                );
            }

            seg_refs.push(meta.into());
        }

        let total_indexed = next_doc_id;

        // Write manifest.
        let manifest = Manifest::new(seg_refs, total_indexed);
        manifest.save(&config.index_dir)?;
        manifest.gc_orphan_segments(&config.index_dir)?;

        eprintln!("ripline: indexed {} files into {} segment(s)", total_indexed, manifest.segments.len());

        Self::open(config)
    }

    /// Open an existing index. Loads the manifest, mmaps base segments,
    /// and rebuilds the path index from segment doc tables.
    pub fn open(config: Config) -> Result<Self, IndexError> {
        let manifest = Manifest::load(&config.index_dir)?;

        let mut base_segments: Vec<MmapSegment> = Vec::new();
        let mut segment_base_ids: Vec<u32> = Vec::new();
        let mut all_paths: Vec<String> = Vec::new();
        let mut next_global_id: u32 = 0;

        for seg_ref in &manifest.segments {
            let seg_path = config.index_dir.join(&seg_ref.filename);
            let seg = MmapSegment::open(&seg_path)?;
            segment_base_ids.push(next_global_id);
            // Iterate using local 0-based indices (0..seg.doc_count).
            for local_id in 0..seg.doc_count {
                if let Some(doc) = seg.get_doc(local_id) {
                    all_paths.push(doc.path);
                }
            }
            next_global_id += seg.doc_count;
            base_segments.push(seg);
        }

        all_paths.sort_unstable();
        all_paths.dedup();
        let path_index = PathIndex::build(&all_paths);

        let base = Arc::new(BaseSegments {
            segments: base_segments,
            base_ids: segment_base_ids,
        });

        let doc_to_file_id = path_index.build_doc_to_file_id(
            next_global_id as usize,
            |gid| {
                let seg_idx = base.base_ids.partition_point(|&b| b <= gid).saturating_sub(1);
                let local = gid.checked_sub(base.base_ids[seg_idx])?;
                base.segments[seg_idx].get_doc(local).map(|d| d.path)
            },
        );

        let snapshot = Arc::new(IndexSnapshot {
            base,
            overlay: OverlayView::empty(),
            delete_set: RoaringBitmap::new(),
            path_index,
            doc_to_file_id,
            all_doc_ids_cache: OnceLock::new(),
        });

        Ok(Index {
            config,
            snapshot: ArcSwap::from(snapshot),
            pending: PendingEdits::new(),
        })
    }

    /// Return index statistics from the current snapshot.
    pub fn stats(&self) -> IndexStats {
        let snap = self.snapshot.load();
        stats::compute_stats(snap.as_ref(), &self.config)
    }

    /// Search for a pattern (literal or regex) across the indexed repository.
    pub fn search(
        &self,
        pattern: &str,
        opts: &SearchOptions,
    ) -> Result<Vec<SearchMatch>, IndexError> {
        crate::search::search(self.snapshot(), &self.config, pattern, opts)
    }

    /// Expose the current snapshot for use by the search layer.
    pub fn snapshot(&self) -> Arc<IndexSnapshot> {
        self.snapshot.load_full()
    }

    /// Buffer a file change. NOT visible to queries until `commit_batch()`.
    /// Only records the path; file content is read at commit time.
    ///
    /// Returns `PathOutsideRepo` if `path` is not under `repo_root`.
    pub fn notify_change(&self, path: &Path) -> Result<(), IndexError> {
        let rel = path
            .strip_prefix(&self.config.repo_root)
            .map_err(|_| IndexError::PathOutsideRepo(path.to_path_buf()))?
            .to_string_lossy()
            .replace('\\', "/");
        self.pending.notify_change(&rel);
        Ok(())
    }

    /// Buffer a file deletion. NOT visible to queries until `commit_batch()`.
    ///
    /// Returns `PathOutsideRepo` if `path` is not under `repo_root`.
    pub fn notify_delete(&self, path: &Path) -> Result<(), IndexError> {
        let rel = path
            .strip_prefix(&self.config.repo_root)
            .map_err(|_| IndexError::PathOutsideRepo(path.to_path_buf()))?
            .to_string_lossy()
            .replace('\\', "/");
        self.pending.notify_delete(&rel);
        Ok(())
    }

    /// Atomically commit all pending edits. After return, changes are visible
    /// to subsequent queries. In-flight searches see the old snapshot.
    pub fn commit_batch(&self) -> Result<(), IndexError> {
        let old_snap = self.snapshot.load_full();
        let take = self.pending.take_for_commit();

        // Total base doc count for overlay doc_id assignment.
        let base_doc_count: u32 = old_snap
            .base_segments()
            .iter()
            .map(|s| s.doc_count)
            .sum();

        // Read content from disk only for NEWLY changed paths.
        // Unchanged dirty files are reused from the old overlay via Arc::clone.
        let mut new_files: Vec<(String, Arc<[u8]>)> = Vec::new();
        for path in &take.newly_changed {
            let abs = self.config.repo_root.join(path);
            // Enforce the same max_file_size limit used during full builds.
            let meta = fs::metadata(&abs)?;
            if meta.len() > self.config.max_file_size {
                return Err(IndexError::FileTooLarge {
                    path: abs,
                    size: meta.len(),
                });
            }
            let content = fs::read(&abs)?;
            new_files.push((path.clone(), Arc::from(content)));
        }

        let overlay = OverlayView::build_incremental(
            base_doc_count,
            &old_snap.overlay,
            new_files,
            &take.newly_changed,
            &take.newly_deleted,
        );

        // Compute delete_set: base doc_ids invalidated by changes.
        let delete_set = compute_delete_set(
            &old_snap.base.segments,
            &old_snap.base.base_ids,
            &take.all_changed,
            &take.all_deleted,
        );

        // Rebuild path index to include overlay paths and exclude deleted.
        let mut all_paths: Vec<String> = Vec::new();
        for (seg_idx, seg) in old_snap.base.segments.iter().enumerate() {
            let base_id = old_snap.base.base_ids.get(seg_idx).copied().unwrap_or(0);
            for local_id in 0..seg.doc_count {
                let global_id = base_id + local_id;
                if delete_set.contains(global_id) {
                    continue;
                }
                if let Some(doc) = seg.get_doc(local_id) {
                    all_paths.push(doc.path);
                }
            }
        }
        for doc in &overlay.docs {
            all_paths.push(doc.path.clone());
        }
        all_paths.sort_unstable();
        all_paths.dedup();
        let path_index = PathIndex::build(&all_paths);

        let total_ids = overlay.docs.iter().map(|d| d.doc_id + 1).max()
            .unwrap_or(base_doc_count) as usize;
        let doc_to_file_id = path_index.build_doc_to_file_id(total_ids, |gid| {
            // Check overlay first (overlay doc_ids >= base_doc_count).
            if let Some(doc) = overlay.get_doc(gid) {
                return Some(doc.path.clone());
            }
            if delete_set.contains(gid) { return None; }
            let seg_idx = old_snap.base.base_ids
                .partition_point(|&b| b <= gid).saturating_sub(1);
            let local = gid.checked_sub(old_snap.base.base_ids[seg_idx])?;
            old_snap.base.segments[seg_idx].get_doc(local).map(|d| d.path)
        });

        let new_snap = Arc::new(IndexSnapshot {
            base: Arc::clone(&old_snap.base),
            overlay,
            delete_set,
            path_index,
            doc_to_file_id,
            all_doc_ids_cache: OnceLock::new(),
        });

        self.snapshot.store(new_snap);
        Ok(())
    }

    /// Convenience: `notify_change` + `commit_batch` for a single file.
    pub fn notify_change_immediate(&self, path: &Path) -> Result<(), IndexError> {
        self.notify_change(path)?;
        self.commit_batch()
    }
}

