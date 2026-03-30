//! Index loading: `Index::open`, `open_with_lock`, and `open_inner`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;
use roaring::RoaringBitmap;

use super::{snapshot, Index, MAX_TOTAL_DOCS};
use crate::index::manifest::Manifest;
use crate::index::overlay::{OverlayView, PendingEdits};
use crate::index::segment::MmapSegment;
use crate::index::snapshot::BaseSegments;
use crate::path::PathIndex;
use crate::{Config, IndexError};

impl Index {
    /// Open an existing index. Loads the manifest, mmaps base segments,
    /// and rebuilds the path index from segment doc tables.
    pub fn open(config: Config) -> Result<Self, IndexError> {
        // Shared lock: multiple readers are fine, but blocks an active build.
        let lock_path = config.index_dir.join("lock");
        let dir_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        dir_lock
            .try_lock_shared()
            .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;

        // Security: reject (or warn about) index directories readable/writable
        // by group/other. Permissive modes allow concurrent ftruncate() races
        // (SIGBUS DoS) and crafted-file injection. New builds enforce 0700 via
        // build_index(); this check catches pre-existing indexes.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Ok(meta) = std::fs::metadata(&config.index_dir) {
                if meta.mode() & 0o077 != 0 {
                    if config.strict_permissions {
                        return Err(IndexError::CorruptIndex(format!(
                            "index dir {:?} has mode {:04o}; expected 0700 (no group/other bits). \
                             group/other access enables SIGBUS DoS via ftruncate. \
                             Fix with: chmod 700, or set strict_permissions=false",
                            config.index_dir,
                            meta.mode() & 0o777,
                        )));
                    } else if config.verbose {
                        eprintln!(
                            "syntext: warning: index dir {:?} has mode {:04o}; \
                             recommend chmod 700 to prevent injection and SIGBUS DoS",
                            config.index_dir,
                            meta.mode() & 0o777,
                        );
                    }
                }
            }
        }

        Self::open_inner(config, dir_lock)
    }

    /// Open an existing index using an already-held directory lock.
    /// Called by `build_index` after downgrading the exclusive lock to shared,
    /// avoiding the gap where a competing build could start.
    pub(super) fn open_with_lock(
        config: Config,
        dir_lock: std::fs::File,
    ) -> Result<Self, IndexError> {
        // Lock is already held (shared) and permissions were verified by
        // build_index, so skip both checks.
        Self::open_inner(config, dir_lock)
    }

    /// Shared implementation for `open` and `open_with_lock`.
    fn open_inner(config: Config, dir_lock: std::fs::File) -> Result<Self, IndexError> {
        let manifest = Manifest::load(&config.index_dir)?;

        let scan_threshold = manifest
            .scan_threshold_fraction
            .unwrap_or(0.10)
            .clamp(0.01, 0.50);

        let mut base_segments: Vec<MmapSegment> = Vec::new();
        let mut segment_base_ids: Vec<u32> = Vec::new();
        let mut base_doc_paths: Vec<Option<std::path::PathBuf>> = Vec::new();
        let mut all_paths: Vec<std::path::PathBuf> = Vec::new();
        let mut path_doc_ids: HashMap<std::path::PathBuf, Vec<u32>> = HashMap::new();
        let mut max_global_id_exclusive: u32 = 0;
        let mut prev_segment_end: u32 = 0;

        for seg_ref in &manifest.segments {
            let seg = if !seg_ref.dict_filename.is_empty() && !seg_ref.post_filename.is_empty() {
                // v3: split .dict + .post files. Validate both filenames.
                for filename in [&seg_ref.dict_filename, &seg_ref.post_filename] {
                    if filename.contains('/')
                        || filename.contains('\\')
                        || filename.contains("..")
                        || Path::new(filename).is_absolute()
                    {
                        return Err(IndexError::CorruptIndex(format!(
                            "invalid segment filename in manifest: {:?}",
                            filename
                        )));
                    }
                }
                let dict_path = config.index_dir.join(&seg_ref.dict_filename);
                let post_path = config.index_dir.join(&seg_ref.post_filename);
                MmapSegment::open_split(&dict_path, &post_path)?
            } else {
                // v2: single combined .seg file. Accept `dict_filename` as a
                // compatibility fallback for older transitional manifests.
                let open_filename = if !seg_ref.filename.is_empty() {
                    &seg_ref.filename
                } else {
                    &seg_ref.dict_filename
                };
                if open_filename.contains('/')
                    || open_filename.contains('\\')
                    || open_filename.contains("..")
                    || Path::new(open_filename).is_absolute()
                {
                    return Err(IndexError::CorruptIndex(format!(
                        "invalid segment filename in manifest: {:?}",
                        open_filename
                    )));
                }
                let seg_path = config.index_dir.join(open_filename);
                MmapSegment::open(&seg_path)?
            };
            // Security: check the per-segment doc count against MAX_TOTAL_DOCS
            // BEFORE iterating the segment's doc entries and inserting them into
            // base_doc_paths and path_doc_ids.
            //
            // Without this early check, a crafted segment with doc_count close to
            // MAX_TOTAL_DOCS and path_len = 65535 per entry could force several
            // gigabytes of PathBuf allocations into path_doc_ids before the
            // post-loop guard triggers. The per-segment check caps the allocation
            // to at most one segment's worth of entries at a time.
            let segment_base_id = seg_ref.base_doc_id.unwrap_or(prev_segment_end);
            if segment_base_id < prev_segment_end {
                return Err(IndexError::CorruptIndex(format!(
                    "segment base_doc_id {} regresses previous end {}",
                    segment_base_id, prev_segment_end
                )));
            }
            let new_global_id_exclusive =
                segment_base_id
                    .checked_add(seg.doc_count)
                    .ok_or(IndexError::DocIdOverflow {
                        base_doc_count: segment_base_id,
                        overlay_docs: 0,
                    })?;
            if new_global_id_exclusive > MAX_TOTAL_DOCS {
                return Err(IndexError::CorruptIndex(format!(
                    "segment would push total docs to {new_global_id_exclusive}, exceeds safety limit of {MAX_TOTAL_DOCS}"
                )));
            }

            segment_base_ids.push(segment_base_id);
            // Iterate using local 0-based indices (0..seg.doc_count).
            for local_id in 0..seg.doc_count {
                if let Some(doc) = seg.get_doc(local_id) {
                    let expected_doc_id = segment_base_id.saturating_add(local_id);
                    if doc.doc_id != expected_doc_id {
                        return Err(IndexError::CorruptIndex(format!(
                            "segment doc_id {} does not match expected {}",
                            doc.doc_id, expected_doc_id
                        )));
                    }
                    let doc_idx = doc.doc_id as usize;
                    if base_doc_paths.len() <= doc_idx {
                        base_doc_paths.resize(doc_idx + 1, None);
                    }
                    if base_doc_paths[doc_idx].is_some() {
                        return Err(IndexError::CorruptIndex(format!(
                            "duplicate base doc_id {} across segments",
                            doc.doc_id
                        )));
                    }
                    base_doc_paths[doc_idx] = Some(doc.path.clone());
                    path_doc_ids
                        .entry(doc.path.clone())
                        .or_default()
                        .push(doc.doc_id);
                    all_paths.push(doc.path);
                }
            }
            prev_segment_end = new_global_id_exclusive;
            max_global_id_exclusive = max_global_id_exclusive.max(new_global_id_exclusive);
            base_segments.push(seg);
        }

        all_paths.sort_unstable();
        all_paths.dedup();
        let path_index = PathIndex::build(&all_paths);

        let base = Arc::new(BaseSegments {
            segments: base_segments,
            base_ids: segment_base_ids,
            base_doc_paths,
            path_doc_ids,
        });

        // Final sanity check: the per-segment guard above should have caught
        // any overage, but verify the accumulated total before the vec allocation.
        if max_global_id_exclusive > MAX_TOTAL_DOCS {
            return Err(IndexError::CorruptIndex(format!(
                "manifest claims {max_global_id_exclusive} total docs, exceeds safety limit of {MAX_TOTAL_DOCS}"
            )));
        }
        let mut base_doc_to_file_id = vec![u32::MAX; max_global_id_exclusive as usize];
        for (gid, path) in base.base_doc_paths.iter().enumerate() {
            if let Some(path) = path {
                if let Some(fid) = path_index.file_id(path) {
                    base_doc_to_file_id[gid] = fid;
                }
            }
        }
        let base_doc_to_file_id = Arc::new(base_doc_to_file_id);

        let snapshot = Arc::new(snapshot::new_snapshot(
            base,
            OverlayView::empty(),
            RoaringBitmap::new(),
            path_index,
            base_doc_to_file_id,
            HashMap::new(),
            scan_threshold,
        ));

        // Open symbol index if it exists on disk.
        #[cfg(feature = "symbols")]
        let symbol_index = {
            let db_path = config.index_dir.join("symbols.db");
            if db_path.exists() {
                crate::symbol::SymbolIndex::open(&db_path)
                    .ok()
                    .map(std::sync::Arc::new)
            } else {
                None
            }
        };

        let canonical_root = std::fs::canonicalize(&config.repo_root)?;

        Ok(Index {
            config,
            snapshot: ArcSwap::from(snapshot),
            pending: PendingEdits::new(),
            _dir_lock: dir_lock,
            canonical_root,
            #[cfg(feature = "symbols")]
            symbol_index,
        })
    }
}
