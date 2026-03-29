//! Index builder (`Index::build`) and reader (`Index::open`).

mod build;
mod compact;
pub(crate) mod encoding;
mod io_util;
pub(crate) use io_util::open_readonly_nofollow;
#[cfg(unix)]
pub(crate) use io_util::verify_fd_matches_stat;
pub mod manifest;
pub mod overlay;
pub mod pending;
pub mod segment;
pub mod snapshot;
mod stats;
pub mod walk;

pub use snapshot::{BaseSegments, IndexSnapshot};

pub(crate) use encoding::normalize_encoding;
pub use walk::is_binary;

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path};
use std::process::Command;
use std::sync::Arc;

use fs2::FileExt;

use arc_swap::ArcSwap;
use roaring::RoaringBitmap;

use crate::index::manifest::Manifest;
use crate::index::overlay::{compute_delete_set, OverlayView, PendingEdits};
use crate::index::segment::MmapSegment;
use crate::path::PathIndex;
use crate::{Config, IndexError, IndexStats, SearchMatch, SearchOptions};

/// Fraction of base docs beyond which the overlay is considered too large.
const OVERLAY_WARN_THRESHOLD: f64 = 0.30;

/// Hard cap on total indexed documents across all segments in a manifest.
///
/// A crafted manifest (e.g., sourced from an untrusted SYNTEXT_INDEX_DIR or
/// --index-dir) could claim billions of docs to force a multi-GB
/// `doc_to_file_id` allocation. 50 million is well above any realistic
/// codebase and bounds the allocation to ~200 MB.
const MAX_TOTAL_DOCS: u32 = 50_000_000;

/// Fraction of base docs beyond which `commit_batch` returns `IndexError::OverlayFull`.
const OVERLAY_ENFORCE_THRESHOLD: f64 = 0.50;

fn resolve_git_binary() -> std::path::PathBuf {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("git");
            if candidate.is_file() {
                if let Ok(resolved) = candidate.canonicalize() {
                    return resolved;
                }
            }
        }
    }
    std::path::PathBuf::from("/usr/bin/git")
}

fn current_repo_head(repo_root: &Path) -> Result<Option<String>, IndexError> {
    let canonical_root = std::fs::canonicalize(repo_root)?;
    let output = match Command::new(resolve_git_binary())
        .arg("-C")
        .arg(&canonical_root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
    {
        Ok(output) => output,
        Err(_) => return Ok(None),
    };

    if !output.status.success() {
        return Ok(None);
    }

    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if head.is_empty() {
        Ok(None)
    } else {
        Ok(Some(head))
    }
}

fn acquire_writer_lock(index_dir: &Path) -> Result<std::fs::File, IndexError> {
    let write_lock_path = index_dir.join("write.lock");
    let write_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&write_lock_path)?;
    write_lock
        .try_lock_exclusive()
        .map_err(|_| IndexError::LockConflict(index_dir.to_path_buf()))?;
    Ok(write_lock)
}

fn projected_overlay_doc_count(
    old_overlay: &OverlayView,
    visible_changed: &HashSet<std::path::PathBuf>,
    removed_paths: &HashSet<std::path::PathBuf>,
) -> usize {
    old_overlay
        .docs
        .iter()
        .filter(|doc| !visible_changed.contains(&doc.path) && !removed_paths.contains(&doc.path))
        .count()
        + visible_changed
            .iter()
            .filter(|p| !removed_paths.contains(*p))
            .count()
}

fn base_doc_id_limit(snapshot: &IndexSnapshot) -> u32 {
    snapshot
        .base_segments()
        .iter()
        .enumerate()
        .filter_map(|(seg_idx, seg)| {
            snapshot
                .segment_base_ids()
                .get(seg_idx)
                .and_then(|base| base.checked_add(seg.doc_count))
        })
        .max()
        .unwrap_or(0)
}
/// Top-level index handle. Thread-safe via `ArcSwap<IndexSnapshot>`.
pub struct Index {
    /// The index configuration.
    pub config: Config,
    snapshot: ArcSwap<IndexSnapshot>,
    pending: PendingEdits,
    /// Advisory lock on the index directory. Held for the lifetime of the
    /// Index: shared for readers (open), exclusive for builders (build).
    _dir_lock: std::fs::File,
    /// Canonicalized repo_root, computed once at open time.
    pub canonical_root: std::path::PathBuf,
    /// Optional symbol index (requires `symbols` feature).
    #[cfg(feature = "symbols")]
    pub symbol_index: Option<std::sync::Arc<crate::symbol::SymbolIndex>>,
}

impl Index {
    fn install_rebuilt_index(&self, rebuilt: &Index) -> Result<IndexStats, IndexError> {
        self.snapshot.store(rebuilt.snapshot());
        self.pending.reset();
        #[cfg(feature = "symbols")]
        if let Some(symbol_index) = &self.symbol_index {
            symbol_index.reopen(&self.config.index_dir.join("symbols.db"))?;
        }
        Ok(self.stats())
    }

    fn rebuild_with(
        &self,
        build_fn: impl FnOnce(Config) -> Result<Index, IndexError>,
    ) -> Result<IndexStats, IndexError> {
        self._dir_lock.unlock()?;
        let rebuilt = match build_fn(self.config.clone()) {
            Ok(rebuilt) => rebuilt,
            Err(err) => {
                self._dir_lock
                    .try_lock_shared()
                    .map_err(|_| IndexError::LockConflict(self.config.index_dir.clone()))?;
                return Err(err);
            }
        };
        self._dir_lock
            .try_lock_shared()
            .map_err(|_| IndexError::LockConflict(self.config.index_dir.clone()))?;

        self.install_rebuilt_index(&rebuilt)
    }

    fn repo_relative_path(&self, path: &Path) -> Result<std::path::PathBuf, IndexError> {
        let rel = path
            .strip_prefix(&self.config.repo_root)
            .map_err(|_| IndexError::PathOutsideRepo(path.to_path_buf()))?;
        if rel.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(IndexError::PathOutsideRepo(path.to_path_buf()));
        }

        Ok(rel.to_path_buf())
    }

    /// Build the index from scratch, writing segments and a manifest.
    /// Respects `.gitignore`, skips binary files and files exceeding
    /// `config.max_file_size`.
    pub fn build(config: Config) -> Result<Self, IndexError> {
        build::build_index(config)
    }

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
        let mut doc_to_file_id = vec![u32::MAX; max_global_id_exclusive as usize];
        for (gid, path) in base.base_doc_paths.iter().enumerate() {
            if let Some(path) = path {
                if let Some(fid) = path_index.file_id(path) {
                    doc_to_file_id[gid] = fid;
                }
            }
        }

        let snapshot = Arc::new(snapshot::new_snapshot(
            base,
            OverlayView::empty(),
            RoaringBitmap::new(),
            path_index,
            doc_to_file_id,
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

    /// Return index statistics from the current snapshot.
    pub fn stats(&self) -> IndexStats {
        let snap = self.snapshot.load();
        stats::compute_stats(
            snap.as_ref(),
            &self.config,
            self.pending.uncommitted_count(),
        )
    }

    /// Search for a pattern (literal or regex) across the indexed repository.
    pub fn search(
        &self,
        pattern: &str,
        opts: &SearchOptions,
    ) -> Result<Vec<SearchMatch>, IndexError> {
        // Route symbol searches to the symbol index when available.
        #[cfg(feature = "symbols")]
        if let Some((name, kind)) = crate::symbol::parse_symbol_prefix(pattern) {
            if let Some(sym_idx) = &self.symbol_index {
                return sym_idx.search(&name, kind);
            }
            // Symbol index not built — fall through to content search.
        }
        crate::search::search(
            self.snapshot(),
            &self.config,
            &self.canonical_root,
            pattern,
            opts,
        )
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
        let rel = self.repo_relative_path(path)?;
        self.pending.notify_change(&rel);
        Ok(())
    }

    /// Buffer a file deletion. NOT visible to queries until `commit_batch()`.
    ///
    /// Returns `PathOutsideRepo` if `path` is not under `repo_root`.
    pub fn notify_delete(&self, path: &Path) -> Result<(), IndexError> {
        let rel = self.repo_relative_path(path)?;
        self.pending.notify_delete(&rel);
        Ok(())
    }

    /// Atomically commit all pending edits. After return, changes are visible
    /// to subsequent queries. In-flight searches see the old snapshot.
    pub fn commit_batch(&self) -> Result<(), IndexError> {
        if !self.pending.has_uncommitted() {
            return Ok(());
        }

        // Serialize concurrent writers. _write_lock is held until end of
        // function (underscore prefix suppresses unused-variable lint without
        // triggering the immediate-drop behaviour of bare `_`).
        let _write_lock = acquire_writer_lock(&self.config.index_dir)?;

        let old_snap = self.snapshot.load_full();
        let take = self.pending.take_for_commit();

        // Total base doc count for overlay doc_id assignment.
        let base_doc_count: u32 = old_snap.base_segments().iter().map(|s| s.doc_count).sum();
        let base_doc_id_limit = base_doc_id_limit(&old_snap);

        // Read content from disk only for NEWLY changed paths.
        // Unchanged dirty files are reused from the old overlay via Arc::clone.
        let mut new_files: Vec<(std::path::PathBuf, Arc<[u8]>)> = Vec::new();
        let mut excluded_changed = std::collections::HashSet::new();
        for path in &take.newly_changed {
            let abs = self.config.repo_root.join(path);
            // Canonicalize before opening to detect symlink swaps that occurred
            // between notify_change() and commit_batch(). If the resolved path
            // escapes canonical_root, reject it as PathOutsideRepo.
            let resolved = std::fs::canonicalize(&abs)?;
            if !resolved.starts_with(&self.canonical_root) {
                return Err(IndexError::PathOutsideRepo(abs));
            }
            // Stat before open to record expected inode. After open, fstat the fd
            // and compare dev+ino to catch directory-component symlink swaps that
            // occur in the window between canonicalize() and open() (O_NOFOLLOW
            // only blocks the final component, not intermediate ones).
            #[cfg(unix)]
            let pre_open_meta = std::fs::metadata(&resolved)?;
            // Enforce the same max_file_size limit used during full builds.
            // Use bounded read to eliminate TOCTOU race: file can grow between
            // metadata check and read. Read up to max_file_size + 1 bytes to detect overflow.
            let file = io_util::open_readonly_nofollow(&resolved)?;
            #[cfg(unix)]
            if !io_util::verify_fd_matches_stat(&file, &pre_open_meta) {
                return Err(IndexError::PathOutsideRepo(abs.clone()));
            }
            // Use saturating_add to guard against max_file_size == u64::MAX:
            // plain `+ 1` would wrap to 0 and read nothing.
            let mut reader = file.take(self.config.max_file_size.saturating_add(1));
            let mut raw: Vec<u8> = Vec::new();
            reader.read_to_end(&mut raw)?;
            if raw.len() as u64 > self.config.max_file_size {
                return Err(IndexError::FileTooLarge {
                    path: abs,
                    size: raw.len() as u64,
                });
            }
            let content = encoding::normalize_encoding(&raw, self.config.verbose);
            if is_binary(&content) {
                excluded_changed.insert(path.clone());
                continue;
            }
            new_files.push((path.clone(), Arc::from(content.as_ref())));
        }

        let mut visible_changed = take.newly_changed.clone();
        for path in &excluded_changed {
            visible_changed.remove(path);
        }

        let mut removed_paths = take.newly_deleted.clone();
        removed_paths.extend(excluded_changed.iter().cloned());

        let projected_overlay_docs =
            projected_overlay_doc_count(&old_snap.overlay, &visible_changed, &removed_paths);

        // Enforce hard overlay size limit before rebuilding the overlay. Once
        // the overlay grows beyond 50% of base docs, the rebuild cost is
        // wasted work because callers need a full reindex anyway.
        if base_doc_count > 0 {
            let ratio = projected_overlay_docs as f64 / base_doc_count as f64;
            if ratio > OVERLAY_ENFORCE_THRESHOLD {
                return Err(IndexError::OverlayFull {
                    overlay_docs: projected_overlay_docs,
                    base_docs: base_doc_count as usize,
                });
            }
        }

        let overlay = OverlayView::build_incremental(
            base_doc_id_limit,
            &old_snap.overlay,
            new_files,
            &visible_changed,
            &removed_paths,
        )?;

        debug_assert_eq!(overlay.docs.len(), projected_overlay_docs);

        // Compute delete_set: base doc_ids invalidated by changes.
        // Start from the previous snapshot's delete_set and add only the delta.
        // The base is immutable between full builds, so the delete_set grows
        // monotonically and incremental accumulation is always correct.
        let delete_set = compute_delete_set(
            &old_snap.base.path_doc_ids,
            &take.newly_changed,
            &take.newly_deleted,
            &old_snap.delete_set,
        );

        // Update the path index incrementally from the previous snapshot.
        let path_index =
            PathIndex::build_incremental(&old_snap.path_index, &removed_paths, &visible_changed);

        let total_ids = overlay
            .docs
            .iter()
            .map(|d| d.doc_id + 1)
            .max()
            .unwrap_or(base_doc_count) as usize;
        let mut doc_to_file_id = old_snap.doc_to_file_id.clone();
        doc_to_file_id.resize(total_ids, u32::MAX);
        for gid in delete_set.iter() {
            let idx = gid as usize;
            if idx < doc_to_file_id.len() {
                doc_to_file_id[idx] = u32::MAX;
            }
        }
        for doc in &overlay.docs {
            let idx = doc.doc_id as usize;
            if idx < doc_to_file_id.len() {
                if let Some(fid) = path_index.file_id(&doc.path) {
                    doc_to_file_id[idx] = fid;
                }
            }
        }

        let new_snap = Arc::new(snapshot::new_snapshot(
            Arc::clone(&old_snap.base),
            overlay,
            delete_set,
            path_index,
            doc_to_file_id,
            old_snap.scan_threshold,
        ));

        // Pre-populate all_doc_ids so the first post-commit query doesn't pay rebuild cost.
        new_snap.all_doc_ids();

        self.snapshot.store(new_snap);
        if self.config.verbose {
            let snap = self.snapshot.load();
            let base_count: u32 = snap.base_segments().iter().map(|s| s.doc_count).sum();
            let overlay_count = snap.overlay.docs.len() as u32;
            if base_count > 0 {
                let ratio = overlay_count as f64 / base_count as f64;
                if ratio > OVERLAY_WARN_THRESHOLD {
                    eprintln!(
                        "syntext: warning: overlay is {:.0}% of base ({} overlay, {} base docs); \
                         consider running `st index` to rebuild",
                        ratio * 100.0,
                        overlay_count,
                        base_count
                    );
                }
            }
        }
        Ok(())
    }

    /// Convenience: `notify_change` + `commit_batch` for a single file.
    pub fn notify_change_immediate(&self, path: &Path) -> Result<(), IndexError> {
        self.notify_change(path)?;
        self.commit_batch()
    }

    /// Trigger compaction when the current snapshot exceeds simple thresholds.
    ///
    /// Returns `true` when a blocking compaction rebuild ran.
    pub fn maybe_compact(&self) -> Result<bool, IndexError> {
        if self.pending.has_uncommitted() {
            self.commit_batch()?;
        }

        let snapshot = self.snapshot();
        if compact::plan(snapshot.as_ref(), &self.config).is_none() {
            return Ok(false);
        }

        self.compact()?;
        Ok(true)
    }

    /// Blocking compaction path.
    ///
    /// Rewrites fresh base segments from the current snapshot state, folding
    /// live overlay docs into the base index without rereading unchanged files
    /// from the working tree.
    pub fn compact(&self) -> Result<(), IndexError> {
        if self.pending.has_uncommitted() {
            self.commit_batch()?;
        }

        let snapshot = self.snapshot();
        let Some(plan) = compact::forced_plan(snapshot.as_ref(), &self.config) else {
            return Ok(());
        };
        self.rebuild_with(|config| compact::compact_index(config, snapshot, plan))?;
        Ok(())
    }

    pub fn rebuild_if_stale(&self) -> Result<Option<IndexStats>, IndexError> {
        if self.pending.has_uncommitted() {
            self.commit_batch()?;
        }

        let manifest = Manifest::load(&self.config.index_dir)?;
        let current_head = current_repo_head(&self.config.repo_root)?;
        if manifest.base_commit == current_head {
            return Ok(None);
        }

        self.rebuild_with(build::build_index).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;
    use xxhash_rust::xxh64::xxh64;

    fn serial_index_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn git(args: &[&str], repo: &std::path::Path) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    fn init_git_repo(repo: &std::path::Path) {
        git(&["init"], repo);
        git(&["config", "user.name", "Syntext Tests"], repo);
        git(&["config", "user.email", "syntext@example.com"], repo);
    }

    fn commit_all(repo: &std::path::Path, message: &str) -> String {
        git(&["add", "."], repo);
        git(&["commit", "-m", message], repo);
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(output.status.success(), "git rev-parse HEAD failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn base_doc_hash(index: &Index, relative_path: &std::path::Path) -> Option<u64> {
        let snapshot = index.snapshot();
        for seg in snapshot.base_segments() {
            for local_doc_id in 0..seg.doc_count {
                let doc = seg.get_doc(local_doc_id)?;
                if doc.path == relative_path {
                    return Some(doc.content_hash);
                }
            }
        }
        None
    }

    fn write_segment_with_global_doc_id(
        index_dir: &std::path::Path,
        doc_id: u32,
        relative_path: &str,
        content: &[u8],
    ) -> crate::index::manifest::SegmentRef {
        let mut writer = crate::index::segment::SegmentWriter::new();
        writer.add_document(
            doc_id,
            std::path::Path::new(relative_path),
            xxh64(content, 0),
            content.len() as u64,
        );
        for gram_hash in crate::tokenizer::build_all(content) {
            writer.add_gram_posting(gram_hash, doc_id);
        }
        let mut seg_ref: crate::index::manifest::SegmentRef =
            writer.write_to_dir(index_dir).unwrap().into();
        seg_ref.base_doc_id = Some(doc_id);
        seg_ref
    }

    fn write_sparse_manifest_index(repo: &std::path::Path, index_dir: &std::path::Path) -> Config {
        std::fs::write(repo.join("a.rs"), b"fn alpha() {}\n").unwrap();
        std::fs::write(repo.join("b.rs"), b"fn beta() {}\n").unwrap();

        let seg_a = write_segment_with_global_doc_id(index_dir, 0, "a.rs", b"fn alpha() {}\n");
        let seg_b = write_segment_with_global_doc_id(index_dir, 5, "b.rs", b"fn beta() {}\n");
        let mut manifest = crate::index::manifest::Manifest::new(vec![seg_a, seg_b], 2);
        manifest.scan_threshold_fraction = Some(0.10);
        manifest.save(index_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(index_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        Config {
            index_dir: index_dir.to_path_buf(),
            repo_root: repo.to_path_buf(),
            ..Config::default()
        }
    }

    #[test]
    fn build_produces_calibrated_threshold_in_valid_range() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        // A corpus large enough that calibration has real files to sample.
        for i in 0..50 {
            std::fs::write(
                repo.path().join(format!("file_{i:03}.rs")),
                format!("fn func_{i}() {{ let x = {i}; }}\n").repeat(20),
            )
            .unwrap();
        }

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config.clone()).unwrap();

        // The manifest must contain a calibrated threshold.
        let manifest = crate::index::manifest::Manifest::load(&config.index_dir).unwrap();
        let threshold = manifest
            .scan_threshold_fraction
            .expect("build() must populate scan_threshold_fraction");

        assert!(
            (0.01..=0.50).contains(&threshold),
            "calibrated threshold {threshold} must be in [0.01, 0.50]"
        );

        // The loaded snapshot must use the calibrated value.
        let snap = index.snapshot();
        assert_eq!(
            snap.scan_threshold, threshold,
            "snapshot.scan_threshold must match manifest value"
        );
    }

    #[test]
    fn open_accepts_manifest_with_gapped_base_doc_ids() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();
        let config = write_sparse_manifest_index(repo.path(), index_dir.path());
        let index = Index::open(config).unwrap();

        assert_eq!(index.snapshot().segment_base_ids(), &[0, 5]);
        let all_doc_ids: Vec<u32> = index.snapshot().all_doc_ids().iter().collect();
        assert_eq!(all_doc_ids, vec![0, 5]);
        assert_eq!(
            index
                .search("alpha", &SearchOptions::default())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            index
                .search("beta", &SearchOptions::default())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn commit_batch_overlay_ids_start_after_max_base_doc_id() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();
        let config = write_sparse_manifest_index(repo.path(), index_dir.path());
        let index = Index::open(config).unwrap();

        let new_path = repo.path().join("c.rs");
        std::fs::write(&new_path, b"fn gamma() {}\n").unwrap();
        index.notify_change(&new_path).unwrap();
        index.commit_batch().unwrap();

        let overlay_ids: Vec<u32> = index
            .snapshot()
            .overlay
            .docs
            .iter()
            .map(|doc| doc.doc_id)
            .collect();
        assert_eq!(overlay_ids, vec![6]);
    }

    #[test]
    fn commit_batch_bounded_read_rejects_file_that_exceeds_limit() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        let path = repo.path().join("big.rs");
        std::fs::write(&path, b"fn small() {}\n").unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            max_file_size: 10, // very small limit
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        // Write content that exceeds the limit.
        std::fs::write(&path, b"fn small_but_now_too_big() { let x = 1; }\n").unwrap();
        index.notify_change(&path).unwrap();
        let result = index.commit_batch();
        assert!(
            matches!(result, Err(IndexError::FileTooLarge { .. })),
            "commit_batch must reject files that exceed max_file_size at read time: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn commit_batch_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let _serial = serial_index_lock();

        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        // Create a legitimate file so the index builds.
        std::fs::write(repo.path().join("real.rs"), b"fn real() {}").unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        // Create a file outside the repo for the symlink to point to.
        let target_outside = std::env::temp_dir().join("syntext_test_escape_target");
        std::fs::write(&target_outside, b"sensitive content").unwrap();

        // Create a symlink inside the repo pointing outside.
        let link_path = repo.path().join("escape.rs");
        symlink(&target_outside, &link_path).unwrap();

        index.notify_change(&link_path).unwrap();
        let result = index.commit_batch();

        // Clean up regardless of result.
        let _ = std::fs::remove_file(&target_outside);
        let _ = std::fs::remove_file(&link_path);

        assert!(
            matches!(result, Err(IndexError::PathOutsideRepo(_))),
            "commit_batch must reject symlinks that escape the repo root, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn commit_batch_accepts_symlink_target_inside_repo() {
        use std::os::unix::fs::symlink;
        let _serial = serial_index_lock();

        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        for i in 0..4 {
            std::fs::write(
                repo.path().join(format!("base_{i}.rs")),
                format!("fn base_{i}() {{}}\n"),
            )
            .unwrap();
        }
        let real = repo.path().join("real.rs");
        std::fs::write(&real, b"fn original() {}\n").unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        let link = repo.path().join("alias.rs");
        symlink(&real, &link).unwrap();
        std::fs::write(&real, b"fn alias_visible() {}\n").unwrap();

        index.notify_change(&link).unwrap();
        index.commit_batch().unwrap();

        let matches = index
            .search("alias_visible", &SearchOptions::default())
            .unwrap();
        assert!(
            matches
                .iter()
                .any(|m| m.path.to_string_lossy() == "alias.rs"),
            "symlink inside repo should remain indexable through commit_batch"
        );
    }

    // Regression test: directory-component symlink swap between canonicalize and open.
    // O_NOFOLLOW only blocks the final path component; an intermediate directory
    // replaced by a symlink would escape the repo without this check.
    #[cfg(unix)]
    #[test]
    fn commit_batch_rejects_intermediate_symlink_swap() {
        use std::os::unix::fs::symlink;
        let _serial = serial_index_lock();

        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();

        // Create a real directory with a file inside the repo.
        let subdir = repo.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();
        std::fs::write(subdir.join("target.rs"), b"fn real() {}").unwrap();
        // Also write a base file so Index::build has at least one document.
        std::fs::write(repo.path().join("base.rs"), b"fn base() {}").unwrap();

        let config = Config {
            repo_root: repo.path().to_path_buf(),
            index_dir: index_dir.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        // Notify about a file inside the real directory -- path validation passes.
        index.notify_change(&subdir.join("target.rs")).unwrap();

        // Simulate the race: replace the real directory with a symlink to outside.
        std::fs::remove_dir_all(&subdir).unwrap();
        // Place a file at the expected name in the outside dir so the open succeeds
        // if the symlink is followed (confirming the attack path would work without the fix).
        std::fs::write(outside.path().join("target.rs"), b"fn attacker() {}").unwrap();
        symlink(outside.path(), &subdir).unwrap();

        // commit_batch must detect the swap and reject with PathOutsideRepo.
        // The existing canonicalize check catches the case where subdir is now a symlink
        // pointing outside the repo. The new inode check covers the narrower race where
        // the swap happens after canonicalize but before open.
        let result = index.commit_batch();
        assert!(
            matches!(result, Err(IndexError::PathOutsideRepo(_))),
            "expected PathOutsideRepo after intermediate symlink swap, got: {result:?}"
        );
    }

    #[test]
    fn commit_batch_returns_overlay_full_when_overlay_ratio_exceeded() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        for i in 0..10 {
            std::fs::write(
                repo.path().join(format!("base_{i:03}.rs")),
                format!("fn base_{i}() {{ let x = {i}; }}\n"),
            )
            .unwrap();
        }

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        for i in 0..6 {
            let path = repo.path().join(format!("overlay_{i:03}.rs"));
            std::fs::write(&path, format!("fn overlay_{i}() {{}}\n")).unwrap();
            index.notify_change(&path).unwrap();
        }

        let result = index.commit_batch();
        assert!(
            matches!(result, Err(IndexError::OverlayFull { .. })),
            "commit_batch must return OverlayFull when overlay exceeds 50% of base, got: {result:?}"
        );
    }

    #[test]
    fn commit_batch_binary_changes_do_not_count_toward_overlay_limit() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        for i in 0..10 {
            std::fs::write(
                repo.path().join(format!("base_{i:03}.rs")),
                format!("fn base_{i}() {{ let x = {i}; }}\n"),
            )
            .unwrap();
        }

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        for i in 0..6 {
            let path = repo.path().join(format!("overlay_{i:03}.bin"));
            std::fs::write(&path, b"\0not indexed\n").unwrap();
            index.notify_change(&path).unwrap();
        }

        let result = index.commit_batch();
        assert!(
            result.is_ok(),
            "binary-only changes should be excluded before overlay limit check: {result:?}"
        );
        assert_eq!(
            index.snapshot().overlay.docs.len(),
            0,
            "binary-only changes must not create overlay docs"
        );
    }

    #[test]
    fn build_succeeds_and_opens_cleanly() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();
        std::fs::write(repo.path().join("lib.rs"), b"fn f() {}").unwrap();
        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let result = Index::build(config);
        assert!(result.is_ok(), "build() must succeed: {:?}", result.err());
    }

    #[cfg(unix)]
    #[test]
    fn open_rejects_permissive_index_dir_mode() {
        use std::os::unix::fs::PermissionsExt;
        let _serial = serial_index_lock();

        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();
        std::fs::write(repo.path().join("lib.rs"), b"fn f() {}").unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        Index::build(config).unwrap();

        std::fs::set_permissions(index_dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            strict_permissions: true,
            ..Config::default()
        };
        let result = Index::open(config);
        match &result {
            Err(IndexError::CorruptIndex(msg)) => {
                assert!(
                    msg.contains("0755"),
                    "error message should mention mode 0755: {msg}"
                );
            }
            Err(e) => panic!("expected CorruptIndex, got: {e}"),
            Ok(_) => panic!("open() must reject permissive dir mode"),
        }
    }

    #[test]
    fn build_index_returns_valid_index_without_lock_gap() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();
        std::fs::write(repo.path().join("lib.rs"), b"fn f() {}").unwrap();
        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();
        let snap = index.snapshot();
        assert!(
            snap.base_segments()
                .iter()
                .map(|s| s.doc_count)
                .sum::<u32>()
                > 0
        );
    }

    #[test]
    fn maintenance_apis_are_noops_when_no_work_is_needed() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        std::fs::write(repo.path().join("main.rs"), b"fn main() {}\n").unwrap();
        init_git_repo(repo.path());
        commit_all(repo.path(), "initial");

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        assert!(!index.maybe_compact().unwrap());
        index.compact().unwrap();
        assert!(index.rebuild_if_stale().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn open_allows_permissive_mode_when_strict_permissions_disabled() {
        use std::os::unix::fs::PermissionsExt;
        let _serial = serial_index_lock();

        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();
        std::fs::write(repo.path().join("lib.rs"), b"fn f() {}").unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        Index::build(config).unwrap();

        std::fs::set_permissions(index_dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            strict_permissions: false,
            ..Config::default()
        };
        let result = Index::open(config);
        assert!(
            result.is_ok(),
            "open() must succeed when strict_permissions is false, got: {}",
            result.err().map(|e| e.to_string()).unwrap_or_default()
        );
    }

    #[test]
    fn compact_reduces_segment_count_to_config_limit() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        for i in 0..6 {
            std::fs::write(
                repo.path().join(format!("file_{i}.rs")),
                format!("fn marker_{i}() {{ println!(\"{i}\"); }}\n"),
            )
            .unwrap();
        }

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            max_segments: 2,
            ..Config::default()
        };
        let index = build::build_index_with_batch_size(config, 1).unwrap();
        assert!(
            index.stats().total_segments > 2,
            "test fixture must start fragmented"
        );

        index.compact().unwrap();

        let stats = index.stats();
        assert!(
            stats.total_segments <= 2,
            "compact() must reduce segment count to config.max_segments, got {}",
            stats.total_segments
        );
        assert!(
            index
                .search("marker_5", &SearchOptions::default())
                .unwrap()
                .iter()
                .any(|m| m.path == std::path::Path::new("file_5.rs")),
            "search results must survive compaction"
        );
        assert_eq!(index.snapshot().overlay.docs.len(), 0);
    }

    #[test]
    fn compact_preserves_untouched_prefix_segments_in_manifest() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        for i in 0..4 {
            std::fs::write(
                repo.path().join(format!("file_{i}.rs")),
                format!("fn marker_{i}() {{ println!(\"{i}\"); }}\n"),
            )
            .unwrap();
        }

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            max_segments: 3,
            ..Config::default()
        };
        let index = build::build_index_with_batch_size(config.clone(), 1).unwrap();
        let before = Manifest::load(&config.index_dir).unwrap();
        assert_eq!(
            before.segments.len(),
            4,
            "fixture must begin with four segments"
        );

        index.compact().unwrap();

        let after = Manifest::load(&config.index_dir).unwrap();
        assert_eq!(
            after.segments.len(),
            3,
            "selective compaction should rewrite only the suffix"
        );
        assert_eq!(after.segments[0].segment_id, before.segments[0].segment_id);
        assert_eq!(after.segments[1].segment_id, before.segments[1].segment_id);
        assert_ne!(after.segments[2].segment_id, before.segments[2].segment_id);
    }

    #[test]
    fn compact_preserves_actual_total_files_for_gapped_prefix_manifest() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();
        let config = write_sparse_manifest_index(repo.path(), index_dir.path());
        let index = Index::open(config.clone()).unwrap();

        index.compact().unwrap();

        let manifest = Manifest::load(&config.index_dir).unwrap();
        assert_eq!(
            manifest.total_files_indexed, 2,
            "compact() must record actual live file count, not max doc_id + 1, when base ranges are sparse"
        );
        assert_eq!(
            manifest.total_docs(),
            manifest.total_files_indexed,
            "manifest doc_count sum and reported total files should stay aligned after gapped compaction"
        );
    }

    #[test]
    fn maybe_compact_rebuilds_when_overlay_ratio_exceeds_threshold() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        init_git_repo(repo.path());
        for i in 0..10 {
            std::fs::write(
                repo.path().join(format!("base_{i}.rs")),
                format!("fn base_{i}() {{}}\n"),
            )
            .unwrap();
        }
        commit_all(repo.path(), "initial");

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            max_segments: 10,
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        for i in 0..4 {
            let path = repo.path().join(format!("base_{i}.rs"));
            std::fs::write(&path, format!("fn updated_{i}() {{}}\n")).unwrap();
            index.notify_change(&path).unwrap();
        }
        index.commit_batch().unwrap();
        assert_eq!(index.snapshot().overlay.docs.len(), 4);

        let snap = index.snapshot();
        let base_docs: usize = snap
            .base_segments()
            .iter()
            .map(|s| s.doc_count as usize)
            .sum();
        let overlay_docs = snap.overlay.docs.len();
        let total_segments = snap.base.segments.len();
        drop(snap);

        assert!(
            index.maybe_compact().unwrap(),
            "overlay ratio > 10% should compact (base_docs={base_docs}, overlay_docs={overlay_docs}, total_segments={total_segments})"
        );
        assert_eq!(
            index.snapshot().overlay.docs.len(),
            0,
            "compaction must fold overlay docs back into the base index"
        );
        assert!(
            index
                .search("updated_1", &SearchOptions::default())
                .unwrap()
                .iter()
                .any(|m| m.path == std::path::Path::new("base_1.rs")),
            "compaction must preserve the updated working tree content"
        );
    }

    #[test]
    fn compact_preserves_base_snapshot_when_working_tree_drifts() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        let path = repo.path().join("tracked.rs");
        std::fs::write(&path, "fn alpha() {}\n").unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();
        let relative = std::path::Path::new("tracked.rs");
        let alpha_hash = xxh64(b"fn alpha() {}\n", 0);
        let beta_hash = xxh64(b"fn beta() {}\n", 0);
        assert_eq!(base_doc_hash(&index, relative), Some(alpha_hash));

        std::fs::write(&path, "fn beta() {}\n").unwrap();
        index.compact().unwrap();

        assert_eq!(
            base_doc_hash(&index, relative),
            Some(alpha_hash),
            "compact() must preserve the indexed base snapshot, not reread unrelated working tree changes"
        );
        assert!(
            base_doc_hash(&index, relative) != Some(beta_hash),
            "compact() must not absorb uncommitted working tree edits into base metadata"
        );
    }

    #[test]
    fn compact_folds_overlay_snapshot_without_rereading_disk() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        for i in 0..10 {
            std::fs::write(
                repo.path().join(format!("tracked_{i}.rs")),
                format!("fn alpha_{i}() {{}}\n"),
            )
            .unwrap();
        }
        let path = repo.path().join("tracked_0.rs");

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();
        let relative = std::path::Path::new("tracked_0.rs");
        let bravo_hash = xxh64(b"fn bravo() {}\n", 0);
        let charlie_hash = xxh64(b"fn charlie() {}\n", 0);

        std::fs::write(&path, "fn bravo() {}\n").unwrap();
        index.notify_change(&path).unwrap();
        index.commit_batch().unwrap();

        std::fs::write(&path, "fn charlie() {}\n").unwrap();
        index.compact().unwrap();

        assert_eq!(
            base_doc_hash(&index, relative),
            Some(bravo_hash),
            "compact() must fold the committed overlay snapshot into base segments"
        );
        assert!(
            base_doc_hash(&index, relative) != Some(charlie_hash),
            "compact() must not reread newer uncommitted disk content while folding overlay docs"
        );
        assert_eq!(index.snapshot().overlay.docs.len(), 0);
    }

    #[test]
    fn rebuild_if_stale_refreshes_snapshot_after_head_change() {
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        init_git_repo(repo.path());
        let file = repo.path().join("main.rs");
        std::fs::write(&file, b"fn old_name() {}\n").unwrap();
        let first_head = commit_all(repo.path(), "first");

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();
        assert_eq!(
            index.stats().base_commit.as_deref(),
            Some(first_head.as_str())
        );

        std::fs::write(&file, b"fn new_name() {}\n").unwrap();
        let second_head = commit_all(repo.path(), "second");

        let stats = index
            .rebuild_if_stale()
            .unwrap()
            .expect("HEAD changed, rebuild must run");
        assert_eq!(stats.base_commit.as_deref(), Some(second_head.as_str()));
        assert!(
            index
                .search("new_name", &SearchOptions::default())
                .unwrap()
                .iter()
                .any(|m| m.path == std::path::Path::new("main.rs")),
            "rebuilt snapshot must include the new committed content"
        );
        assert!(
            index
                .search("old_name", &SearchOptions::default())
                .unwrap()
                .is_empty(),
            "rebuilt snapshot must stop returning content from the old HEAD"
        );
        assert_eq!(index.stats().pending_edits, 0);
    }

    #[test]
    fn commit_batch_max_file_size_saturates_not_wraps() {
        // Verify that the take() sentinel does not wrap to 0 for u64::MAX.
        // saturating_add(1) stays at u64::MAX; plain + 1 would wrap to 0.
        let sentinel = u64::MAX.saturating_add(1);
        assert_eq!(sentinel, u64::MAX, "saturating_add must not wrap");
        assert_ne!(sentinel, 0u64, "must not wrap to 0");
    }
}
