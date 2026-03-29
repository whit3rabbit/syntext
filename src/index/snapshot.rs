//! Read-side snapshot types: `BaseSegments` and `IndexSnapshot`.
//!
//! These are the immutable-from-the-outside view of the index that search
//! operations work against. `BaseSegments` is Arc-shared across snapshot swaps
//! so base segment data is never copied. `IndexSnapshot` is the point-in-time
//! view handed to a query, combining the base segments with the current overlay
//! and delete set.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use roaring::RoaringBitmap;

use crate::index::overlay::OverlayView;
use crate::index::segment::MmapSegment;
use crate::path::PathIndex;

/// Hard cap for cached merged posting bitmaps per snapshot.
///
/// When the cache reaches this size and a new gram is inserted, we clear the
/// whole cache and keep only the newest entry. This is intentionally coarse:
/// it bounds memory without adding LRU bookkeeping to the search hot path.
pub(crate) const POSTING_BITMAP_CACHE_MAX_ENTRIES: usize = 1024;

/// Shared base segments (Arc-shared across snapshot swaps).
pub struct BaseSegments {
    /// The memory mapped segments.
    pub segments: Vec<MmapSegment>,
    /// Global doc_id offsets for each segment.
    pub base_ids: Vec<u32>,
    /// Global base doc_id -> repository-relative path, sparse for gapped ranges.
    pub base_doc_paths: Vec<Option<PathBuf>>,
    /// Repository-relative path -> all base doc_ids for that path.
    pub path_doc_ids: HashMap<PathBuf, Vec<u32>>,
}

/// A consistent point-in-time view of the index for querying.
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
    /// Cached merged posting bitmaps for repeated gram lookups in this snapshot.
    posting_bitmap_cache: OnceLock<Mutex<HashMap<u64, Arc<RoaringBitmap>>>>,
    /// Calibrated index-vs-scan crossover fraction. Populated from
    /// `Manifest::scan_threshold_fraction` on open; defaults to 0.10.
    pub scan_threshold: f64,
}

impl IndexSnapshot {
    /// Return the immutable base segments.
    pub fn base_segments(&self) -> &[MmapSegment] {
        &self.base.segments
    }
    /// Return the global doc_id offsets for each base segment.
    pub fn segment_base_ids(&self) -> &[u32] {
        &self.base.base_ids
    }

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

    fn posting_bitmap_cache(&self) -> &Mutex<HashMap<u64, Arc<RoaringBitmap>>> {
        self.posting_bitmap_cache
            .get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(crate) fn cached_posting_bitmap(&self, gram_hash: u64) -> Option<Arc<RoaringBitmap>> {
        let cache = self
            .posting_bitmap_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.get(&gram_hash).cloned()
    }

    pub(crate) fn store_posting_bitmap(
        &self,
        gram_hash: u64,
        bitmap: Arc<RoaringBitmap>,
    ) -> Arc<RoaringBitmap> {
        let mut cache = self
            .posting_bitmap_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !cache.contains_key(&gram_hash) && cache.len() >= POSTING_BITMAP_CACHE_MAX_ENTRIES {
            cache.clear();
        }
        cache
            .entry(gram_hash)
            .or_insert_with(|| Arc::clone(&bitmap))
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn clone_for_test(&self) -> IndexSnapshot {
        IndexSnapshot {
            base: Arc::clone(&self.base),
            overlay: self.overlay.clone(),
            delete_set: self.delete_set.clone(),
            path_index: self.path_index.clone(),
            doc_to_file_id: self.doc_to_file_id.clone(),
            scan_threshold: self.scan_threshold,
            all_doc_ids_cache: OnceLock::new(),
            posting_bitmap_cache: OnceLock::new(),
        }
    }

    /// Clone this snapshot with a different `scan_threshold`. Used by tests to
    /// verify that `should_use_index` reads from the snapshot rather than a
    /// hard-coded constant.
    #[cfg(test)]
    pub(crate) fn with_scan_threshold(&self, threshold: f64) -> IndexSnapshot {
        IndexSnapshot {
            scan_threshold: threshold,
            ..self.clone_for_test()
        }
    }

    #[cfg(test)]
    pub(crate) fn posting_bitmap_cache_len(&self) -> usize {
        self.posting_bitmap_cache
            .get()
            .map(|cache| {
                cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .len()
            })
            .unwrap_or(0)
    }
}

/// Construct an `IndexSnapshot`, initializing the private `OnceLock` fields.
///
/// This is the only way to build an `IndexSnapshot` from outside this module,
/// since `all_doc_ids_cache` and `posting_bitmap_cache` are private.
pub fn new_snapshot(
    base: Arc<BaseSegments>,
    overlay: crate::index::overlay::OverlayView,
    delete_set: roaring::RoaringBitmap,
    path_index: crate::path::PathIndex,
    doc_to_file_id: Vec<u32>,
    scan_threshold: f64,
) -> IndexSnapshot {
    IndexSnapshot {
        base,
        overlay,
        delete_set,
        path_index,
        doc_to_file_id,
        scan_threshold,
        all_doc_ids_cache: OnceLock::new(),
        posting_bitmap_cache: OnceLock::new(),
    }
}
