//! Read-side snapshot types: `BaseSegments` and `IndexSnapshot`.
//!
//! These are the immutable-from-the-outside view of the index that search
//! operations work against. `BaseSegments` is Arc-shared across snapshot swaps
//! so base segment data is never copied. `IndexSnapshot` is the point-in-time
//! view handed to a query, combining the base segments with the current overlay
//! and delete set.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use roaring::RoaringBitmap;

use crate::index::overlay::OverlayView;
use crate::index::segment::MmapSegment;
use crate::path::PathIndex;

/// Byte budget for cached merged posting bitmaps per snapshot.
///
/// The cache evicts oldest-first (FIFO) once the cumulative serialized size of
/// stored bitmaps would exceed this budget, rather than clearing wholesale at an
/// entry count. FIFO avoids the mid-query cliff where a multi-spec or `--refs`
/// search that re-looks-up the same grams loses the entire cache in one insert;
/// the byte budget bounds true memory (a count cap let ~1024 dense bitmaps reach
/// multiple GB, since a single Roaring bitmap can serialize to megabytes).
pub(crate) const POSTING_BITMAP_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Snapshot-local cache of merged posting bitmaps with FIFO byte-bounded
/// eviction. Sizes are cached alongside each bitmap so eviction never recomputes
/// `serialized_size()`.
struct PostingCache {
    map: HashMap<u64, (Arc<RoaringBitmap>, usize)>,
    /// Insertion order of live keys, oldest at the front, for FIFO eviction.
    order: VecDeque<u64>,
    /// Cumulative serialized size of the bitmaps currently in `map`.
    bytes: usize,
}

impl PostingCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }

    fn get(&self, key: u64) -> Option<Arc<RoaringBitmap>> {
        self.map.get(&key).map(|(bm, _)| Arc::clone(bm))
    }

    /// Insert `bitmap` under `key`, evicting oldest entries FIFO until the
    /// cumulative serialized size fits `budget`. Always keeps the new entry even
    /// if it alone exceeds `budget`. Returns the already-stored Arc on duplicate.
    fn insert_with_budget(
        &mut self,
        key: u64,
        bitmap: Arc<RoaringBitmap>,
        budget: usize,
    ) -> Arc<RoaringBitmap> {
        if let Some((existing, _)) = self.map.get(&key) {
            return Arc::clone(existing);
        }
        let size = bitmap.serialized_size();
        while self.bytes + size > budget {
            let Some(old) = self.order.pop_front() else {
                break; // nothing left to evict; keep the new entry regardless
            };
            if let Some((_, old_size)) = self.map.remove(&old) {
                self.bytes -= old_size;
            }
        }
        self.bytes += size;
        self.order.push_back(key);
        self.map.insert(key, (Arc::clone(&bitmap), size));
        bitmap
    }

    fn insert(&mut self, key: u64, bitmap: Arc<RoaringBitmap>) -> Arc<RoaringBitmap> {
        self.insert_with_budget(key, bitmap, POSTING_BITMAP_CACHE_MAX_BYTES)
    }
}

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
    /// Lazily-built global base doc_id -> PathIndex file_id map. Left
    /// uninitialized here (`OnceLock::new()` in every constructor); use
    /// [`BaseSegments::base_doc_to_file_id`] to build-or-fetch it. Not fully
    /// `pub` since it must only ever be touched through that accessor.
    pub(crate) base_doc_to_file_id: OnceLock<Arc<Vec<u32>>>,
}

impl BaseSegments {
    /// Get or lazily build the base doc_id -> file_id map.
    ///
    /// `base_doc_paths` never changes after `open()` (base segments are
    /// immutable between full rebuilds/compactions), so the derived mapping
    /// is safe to compute once and cache here, then share across every
    /// `IndexSnapshot` built on top of this `BaseSegments` -- even across
    /// commits that incrementally rebuild `path_index`, because
    /// `PathIndex::build_incremental` preserves `file_id`s for paths that
    /// survive, and paths that don't survive are already excluded from
    /// search results via `delete_set` before this map is consulted.
    ///
    /// Building this eagerly at `open()` cost every `st` invocation a
    /// `base_doc_paths.len()`-sized allocation and scan even when the run
    /// never used a path filter or `--files`; deferring it here means that
    /// cost is paid only by callers that actually need it.
    pub(crate) fn base_doc_to_file_id(&self, path_index: &crate::path::PathIndex) -> Arc<Vec<u32>> {
        Arc::clone(self.base_doc_to_file_id.get_or_init(|| {
            let mut map = vec![u32::MAX; self.base_doc_paths.len()];
            for (gid, path) in self.base_doc_paths.iter().enumerate() {
                if let Some(path) = path {
                    if let Some(fid) = path_index.file_id(path) {
                        map[gid] = fid;
                    }
                }
            }
            Arc::new(map)
        }))
    }
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
    /// Maps overlay doc_id -> PathIndex file_id. Rebuilt on each commit.
    pub overlay_doc_to_file_id: HashMap<u32, u32>,
    /// Cached bitmap of all valid doc IDs. Lazy-initialized on first access.
    all_doc_ids_cache: OnceLock<RoaringBitmap>,
    /// Cached merged posting bitmaps for repeated gram lookups in this snapshot.
    posting_bitmap_cache: OnceLock<Mutex<PostingCache>>,
    /// Memoized glob→bitmap cache for path-scoped queries.
    pub(crate) glob_cache: OnceLock<Mutex<HashMap<String, RoaringBitmap>>>,
    /// Calibrated index-vs-scan crossover fraction. Populated from
    /// `Manifest::scan_threshold_fraction` on open; defaults to 0.10.
    pub scan_threshold: f64,
}

impl IndexSnapshot {
    /// Maps base doc_id -> PathIndex file_id for O(1) path filter lookup.
    /// Value is u32::MAX for docs with no PathIndex entry. Built on first
    /// use (search or `--files`) rather than eagerly at `open()`; see
    /// [`BaseSegments::base_doc_to_file_id`].
    pub fn base_doc_to_file_id(&self) -> Arc<Vec<u32>> {
        self.base.base_doc_to_file_id(&self.path_index)
    }

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

    fn posting_bitmap_cache(&self) -> &Mutex<PostingCache> {
        self.posting_bitmap_cache
            .get_or_init(|| Mutex::new(PostingCache::new()))
    }

    /// # Poison recovery
    /// Recovery is safe: the cache is a derived, performance-only structure over
    /// immutable base segment data. The worst case after a panic mid-operation is
    /// a partially cleared HashMap; the next query simply recomputes the entry.
    /// No correctness invariant depends on cache completeness.
    pub(crate) fn cached_posting_bitmap(&self, gram_hash: u64) -> Option<Arc<RoaringBitmap>> {
        let cache = self
            .posting_bitmap_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.get(gram_hash)
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
        cache.insert(gram_hash, bitmap)
    }

    #[cfg(test)]
    pub(crate) fn clone_for_test(&self) -> IndexSnapshot {
        IndexSnapshot {
            base: Arc::clone(&self.base),
            overlay: self.overlay.clone(),
            delete_set: self.delete_set.clone(),
            path_index: self.path_index.clone(),
            overlay_doc_to_file_id: self.overlay_doc_to_file_id.clone(),
            scan_threshold: self.scan_threshold,
            all_doc_ids_cache: OnceLock::new(),
            posting_bitmap_cache: OnceLock::new(),
            glob_cache: OnceLock::new(),
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
                    .map
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
    overlay_doc_to_file_id: HashMap<u32, u32>,
    scan_threshold: f64,
) -> IndexSnapshot {
    IndexSnapshot {
        base,
        overlay,
        delete_set,
        path_index,
        overlay_doc_to_file_id,
        scan_threshold,
        // Left as OnceLock::new() (not pre-populated) intentionally.
        // commit_batch() calls all_doc_ids() eagerly after constructing the
        // snapshot, filling the cache before readers see it. Keeping this
        // constructor cheap lets tests (clone_for_test, with_scan_threshold)
        // skip the bitmap cost.
        all_doc_ids_cache: OnceLock::new(),
        posting_bitmap_cache: OnceLock::new(),
        glob_cache: OnceLock::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(id: u32) -> Arc<RoaringBitmap> {
        Arc::new(RoaringBitmap::from_iter([id]))
    }

    #[test]
    fn posting_cache_evicts_oldest_first_under_byte_budget() {
        let mut cache = PostingCache::new();
        // Budget for two single-element bitmaps; the third evicts the oldest.
        let one = bitmap(0).serialized_size();
        let budget = one * 2;

        cache.insert_with_budget(0, bitmap(0), budget);
        cache.insert_with_budget(1, bitmap(1), budget);
        cache.insert_with_budget(2, bitmap(2), budget);

        assert!(cache.get(0).is_none(), "oldest entry must be evicted");
        assert!(
            cache.get(1).is_some(),
            "recent entry must survive the cliff"
        );
        assert!(cache.get(2).is_some(), "newest entry must be present");
        assert!(cache.bytes <= budget, "byte total stays bounded");
    }

    #[test]
    fn posting_cache_keeps_new_entry_even_if_alone_over_budget() {
        let mut cache = PostingCache::new();
        // Budget smaller than a single entry: still keep the newest.
        cache.insert_with_budget(7, bitmap(7), 0);
        assert!(cache.get(7).is_some());
    }

    #[test]
    fn posting_cache_dedups_and_returns_existing() {
        let mut cache = PostingCache::new();
        let first = cache.insert(3, bitmap(3));
        let second = cache.insert(3, bitmap(3));
        assert!(Arc::ptr_eq(&first, &second), "duplicate returns stored Arc");
        assert_eq!(cache.map.len(), 1);
    }
}
