//! Index loading: `Index::open`, `open_with_lock`, and `open_inner`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;
use roaring::RoaringBitmap;

use super::{snapshot, Index, MAX_TOTAL_DOCS};
use crate::index::manifest::Manifest;
use crate::index::overlay::{OverlayView, PendingEdits};
use crate::index::segment::{DictVerify, MmapSegment, PostVerify};
use crate::index::snapshot::BaseSegments;
use crate::path::PathIndex;
use crate::{Config, IndexError};

impl Index {
    /// Open an existing index. Loads the manifest, mmaps base segments,
    /// and rebuilds the path index from segment doc tables.
    pub fn open(config: Config) -> Result<Self, IndexError> {
        // Shared lock: multiple readers are fine, but blocks an active build.
        // A missing index directory surfaces here (the lock file cannot be
        // created), before Manifest::load gets a chance to report it.
        let dir_lock = match super::helpers::open_dir_lock_file(&config.index_dir) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(IndexError::IndexNotFound(config.index_dir.clone()));
            }
            Err(e) => return Err(e.into()),
        };
        dir_lock
            .try_lock_shared()
            .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;

        // Security: reject (or warn about) index directories readable/writable
        // by group/other. Permissive modes allow concurrent ftruncate() races
        // (SIGBUS DoS) and crafted-file injection. New builds enforce 0700 via
        // build_index(); this check catches pre-existing indexes.
        //
        // Scope note: this verifies only the index *directory* mode, not each
        // segment file's mode. A pre-existing 0644 segment inside a 0700 dir
        // passes. That is acceptable because directory traversal is blocked
        // (a non-owner cannot enumerate/create index entries to reach a
        // ftruncate target), so the per-file mode is not the load-bearing
        // control — the directory mode is. Adding per-file checks would be
        // defense-in-depth but is not required for the threat model.
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
                    } else {
                        log::debug!(
                            "index dir {:?} has mode {:04o}; \
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
            let mut seg = if !seg_ref.dict_filename.is_empty() && !seg_ref.post_filename.is_empty()
            {
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
                // O(1) truncation/extension detection: compare the on-disk
                // .post length against the length recorded at write time.
                // Manifests predating post_len skip this check (None).
                if let Some(expected_len) = seg_ref.post_len {
                    let actual_len = std::fs::metadata(&post_path)?.len();
                    if actual_len != expected_len {
                        return Err(IndexError::CorruptIndex(format!(
                            "post file {} length changed: expected {}, got {}",
                            seg_ref.post_filename, expected_len, actual_len
                        )));
                    }
                }
                let verify = if config.verify_on_open {
                    PostVerify::Full
                } else {
                    PostVerify::Structural
                };
                // Same flag drives both: `st verify` (verify_on_open = true)
                // wants the full dict checksum too, not just the postings one.
                let dict_verify = if config.verify_on_open {
                    DictVerify::Full
                } else {
                    DictVerify::Structural
                };
                MmapSegment::open_split(&dict_path, &post_path, verify, dict_verify)?
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
            seg.doc_bytes = seg_ref.doc_bytes;
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
            // Bulk-read the whole doc table in one pass (one read per segment)
            // instead of 3 preads per doc; slot index is the local 0-based
            // doc_id, matching the previous `0..seg.doc_count` loop.
            for (local_id, maybe_doc) in seg.iter_docs().into_iter().enumerate() {
                let local_id = local_id as u32;
                if let Some(doc) = maybe_doc {
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
        // `paths.idx` caches the sorted path list plus the extension/component
        // bitmaps that `PathIndex::build` below would otherwise recompute from
        // scratch on every open. Since bounded auto-update-on-search reopens
        // (or re-derives) the index on every search, that rebuild is the fixed
        // cost this sidecar exists to eliminate. It is only ever a cache: any
        // failure to load falls back to the rebuild path unconditionally.
        //
        // The manifest's `paths_idx_version` is checked before even opening
        // the file: it is written atomically together with `manifest.json`
        // by build.rs/compact.rs at the same time as `paths.idx` itself, so a
        // mismatch (including `None`, from a manifest predating this field or
        // from a manifest written by a build that skipped the sidecar write)
        // means the two files were not written by the same generation and
        // the on-disk `paths.idx` layout cannot be trusted even if it happens
        // to pass its own internal magic/checksum checks.
        let path_index = if manifest.paths_idx_version == Some(super::paths_idx::FORMAT_VERSION) {
            match super::paths_idx::read_paths_idx(&config.index_dir) {
                Ok(index) => index,
                Err(e) => {
                    log::debug!("paths.idx not used ({e}); rebuilding path index");
                    PathIndex::build(&all_paths)
                }
            }
        } else {
            log::debug!(
                "paths.idx sidecar version {:?} does not match expected {}; \
                 rebuilding path index",
                manifest.paths_idx_version,
                super::paths_idx::FORMAT_VERSION
            );
            PathIndex::build(&all_paths)
        };

        let base = Arc::new(BaseSegments {
            segments: base_segments,
            base_ids: segment_base_ids,
            base_doc_paths,
            path_doc_ids,
            base_doc_to_file_id: std::sync::OnceLock::new(),
        });

        // Final sanity check: the per-segment guard above should have caught
        // any overage; verify the accumulated total. `base_doc_to_file_id` is
        // no longer built eagerly here -- it is derived lazily from
        // `base.base_doc_paths` (sized to this same bound) on first use; see
        // `BaseSegments::base_doc_to_file_id`.
        if max_global_id_exclusive > MAX_TOTAL_DOCS {
            return Err(IndexError::CorruptIndex(format!(
                "manifest claims {max_global_id_exclusive} total docs, exceeds safety limit of {MAX_TOTAL_DOCS}"
            )));
        }

        // Load the persistent delete-set (base doc_ids superseded/removed by
        // durable incremental deltas). Unlike paths.idx this is a SOURCE OF
        // TRUTH, not a cache: a base doc is hidden from search only by
        // delete_set, and the verifier re-reads live file bytes for base docs,
        // so a lost delete-set would surface a modified file's stale base doc
        // AND its new delta doc as duplicate matches. Therefore FAIL CLOSED:
        // if the manifest names a deletes sidecar and it will not load, refuse
        // to open rather than silently start empty. `None` means a fresh build
        // or post-compaction index with no deletes, where empty is correct.
        let delete_set = match &manifest.overlay_deletes_file {
            None => RoaringBitmap::new(),
            Some(name) => {
                super::deletes_idx::read_deletes_idx(&config.index_dir, name).map_err(|e| {
                    IndexError::CorruptIndex(format!(
                        "delete-set sidecar {name} unreadable ({e}); run `st index` to rebuild"
                    ))
                })?
            }
        };

        let snapshot = Arc::new(snapshot::new_snapshot(
            base,
            OverlayView::empty(),
            delete_set,
            path_index,
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
