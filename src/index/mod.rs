//! Index builder (`Index::build`) and reader (`Index::open`).

mod build;
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

pub use walk::is_binary;

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path};
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

/// Fraction of base docs beyond which `commit_batch` returns `IndexError::OverlayFull`.
const OVERLAY_ENFORCE_THRESHOLD: f64 = 0.50;

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
        .filter(|doc| {
            !visible_changed.contains(&doc.path) && !removed_paths.contains(&doc.path)
        })
        .count()
        + visible_changed.len()
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

        let manifest = Manifest::load(&config.index_dir)?;

        let scan_threshold = manifest
            .scan_threshold_fraction
            .unwrap_or(0.10)
            .clamp(0.01, 0.50);

        let mut base_segments: Vec<MmapSegment> = Vec::new();
        let mut segment_base_ids: Vec<u32> = Vec::new();
        let mut base_doc_paths: Vec<std::path::PathBuf> = Vec::new();
        let mut all_paths: Vec<std::path::PathBuf> = Vec::new();
        let mut path_doc_ids: HashMap<std::path::PathBuf, Vec<u32>> = HashMap::new();
        let mut next_global_id: u32 = 0;

        for seg_ref in &manifest.segments {
            let seg = if !seg_ref.dict_filename.is_empty() && !seg_ref.post_filename.is_empty() {
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
                let open_filename = &seg_ref.filename;
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
            segment_base_ids.push(next_global_id);
            // Iterate using local 0-based indices (0..seg.doc_count).
            for local_id in 0..seg.doc_count {
                if let Some(doc) = seg.get_doc(local_id) {
                    debug_assert_eq!(doc.doc_id as usize, base_doc_paths.len());
                    base_doc_paths.push(doc.path.clone());
                    path_doc_ids.entry(doc.path.clone()).or_default().push(doc.doc_id);
                    all_paths.push(doc.path);
                }
            }
            // Security: guard against u32 overflow when summing doc_counts
            // from segments loaded via manifest (which could be crafted).
            next_global_id = next_global_id.checked_add(seg.doc_count).ok_or(
                IndexError::DocIdOverflow {
                    base_doc_count: next_global_id,
                    overlay_docs: 0,
                },
            )?;
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

        let mut doc_to_file_id = vec![u32::MAX; next_global_id as usize];
        for (gid, path) in base.base_doc_paths.iter().enumerate() {
            if let Some(fid) = path_index.file_id(path) {
                doc_to_file_id[gid] = fid;
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
        stats::compute_stats(snap.as_ref(), &self.config)
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
                let kind_str = kind.as_ref().map(|k| k.as_str());
                return sym_idx.search(&name, kind_str);
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
            let mut reader = file.take(self.config.max_file_size + 1);
            let mut content: Vec<u8> = Vec::new();
            reader.read_to_end(&mut content)?;
            if content.len() as u64 > self.config.max_file_size {
                return Err(IndexError::FileTooLarge {
                    path: abs,
                    size: content.len() as u64,
                });
            }
            if is_binary(&content) {
                excluded_changed.insert(path.clone());
                continue;
            }
            new_files.push((path.clone(), Arc::from(content)));
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
            base_doc_count,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_produces_calibrated_threshold_in_valid_range() {
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
    fn commit_batch_bounded_read_rejects_file_that_exceeds_limit() {
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
}
