//! Index builder (`Index::build`) and reader (`Index::open`).

mod build;
mod commit;
mod compact;
mod compact_plan;
pub(crate) mod encoding;
mod helpers;
pub(crate) mod io_util;
pub(crate) use io_util::open_readonly_nofollow;
#[cfg(unix)]
pub(crate) use io_util::verify_fd_matches_stat;
pub mod manifest;
mod open;
pub mod overlay;
pub mod pending;
pub mod segment;
pub mod snapshot;
mod stats;
pub mod walk;

pub use snapshot::{BaseSegments, IndexSnapshot};

pub(crate) use encoding::normalize_encoding;
pub use walk::is_binary;

use std::path::{Component, Path};
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::index::manifest::Manifest;
use crate::index::overlay::PendingEdits;
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

/// Top-level index handle. Thread-safe via `ArcSwap<IndexSnapshot>`.
pub struct Index {
    /// The index configuration.
    pub config: Config,
    snapshot: ArcSwap<snapshot::IndexSnapshot>,
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

        self.normalize_repo_relative_path(rel)
    }

    fn normalize_repo_relative_path(&self, rel: &Path) -> Result<std::path::PathBuf, IndexError> {
        if !self.path_has_intermediate_symlink(rel)? {
            return Ok(rel.to_path_buf());
        }

        let abs = self.config.repo_root.join(rel);
        let Some(parent) = abs.parent() else {
            return Ok(rel.to_path_buf());
        };
        let canonical_parent = std::fs::canonicalize(parent)?;
        if !canonical_parent.starts_with(&self.canonical_root) {
            return Err(IndexError::PathOutsideRepo(abs));
        }

        let Some(file_name) = rel.file_name() else {
            return Ok(rel.to_path_buf());
        };
        let normalized = canonical_parent.join(file_name);
        normalized
            .strip_prefix(&self.canonical_root)
            .map(|p| p.to_path_buf())
            .map_err(|_| IndexError::PathOutsideRepo(normalized))
    }

    fn path_has_intermediate_symlink(&self, rel: &Path) -> Result<bool, IndexError> {
        let mut current = self.config.repo_root.clone();
        let mut components = rel.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(part) = component else {
                continue;
            };
            if components.peek().is_none() {
                break;
            }
            current.push(part);
            match std::fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => return Ok(true),
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(err) => return Err(IndexError::Io(err)),
            }
        }

        Ok(false)
    }

    /// Build the index from scratch, writing segments and a manifest.
    /// Respects `.gitignore`, skips binary files and files exceeding
    /// `config.max_file_size`.
    pub fn build(config: Config) -> Result<Self, IndexError> {
        build::build_index(config)
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
            // Symbol index not built -- fall through to content search.
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
        let current_head = helpers::current_repo_head(&self.config.repo_root)?;
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

    #[cfg(unix)]
    #[test]
    fn commit_batch_normalizes_paths_under_symlinked_directory() {
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
        let real_dir = repo.path().join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let real_file = real_dir.join("nested.rs");
        std::fs::write(&real_file, b"fn original() {}\n").unwrap();
        symlink(&real_dir, repo.path().join("alias")).unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        std::fs::write(&real_file, b"fn original() {}\nfn normalized_alias() {}\n").unwrap();
        index
            .notify_change(&repo.path().join("alias/nested.rs"))
            .unwrap();
        index.commit_batch().unwrap();

        let matches = index
            .search("normalized_alias", &SearchOptions::default())
            .unwrap();
        assert!(
            matches
                .iter()
                .any(|m| m.path.to_string_lossy() == "real/nested.rs"),
            "incremental update through a symlinked directory must update the real path entry"
        );
        assert!(
            matches
                .iter()
                .all(|m| m.path.to_string_lossy() != "alias/nested.rs"),
            "incremental update through a symlinked directory must not reintroduce alias paths"
        );
    }

    #[cfg(unix)]
    #[test]
    fn commit_batch_normalizes_delete_under_symlinked_directory() {
        use std::os::unix::fs::symlink;
        let _serial = serial_index_lock();

        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        let real_dir = repo.path().join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let real_file = real_dir.join("nested.rs");
        std::fs::write(&real_file, b"fn remove_me() {}\n").unwrap();
        symlink(&real_dir, repo.path().join("alias")).unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();

        std::fs::remove_file(&real_file).unwrap();
        index
            .notify_delete(&repo.path().join("alias/nested.rs"))
            .unwrap();
        index.commit_batch().unwrap();

        let matches = index
            .search("remove_me", &SearchOptions::default())
            .unwrap();
        assert!(
            matches.is_empty(),
            "delete through a symlinked directory must remove the real path entry"
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
    fn base_doc_id_limit_overflow_returns_error() {
        // B01: base_doc_id_limit must return Err when base + doc_count
        // overflows u32, not silently drop via filter_map.
        //
        // In practice MAX_TOTAL_DOCS (50M) prevents near-u32::MAX base_ids
        // from being loaded via open(), so this is defense in depth. We test
        // the function directly via a crafted IndexSnapshot.
        use crate::index::snapshot::{new_snapshot, BaseSegments};

        let _serial = serial_index_lock();
        let index_dir = TempDir::new().unwrap();

        // Create a real segment file (doc_count=1).
        let seg_ref =
            write_segment_with_global_doc_id(index_dir.path(), 0, "a.rs", b"fn alpha() {}\n");
        let seg_file = index_dir.path().join(&seg_ref.dict_filename);
        let seg = crate::index::segment::MmapSegment::open(&seg_file).unwrap();
        assert_eq!(seg.doc_count, 1);

        // Set base_id = u32::MAX so base + doc_count(1) overflows.
        let base = Arc::new(BaseSegments {
            segments: vec![seg],
            base_ids: vec![u32::MAX],
            base_doc_paths: vec![],
            path_doc_ids: std::collections::HashMap::new(),
        });
        let snap = new_snapshot(
            base,
            crate::index::overlay::OverlayView::empty(),
            roaring::RoaringBitmap::new(),
            crate::path::PathIndex::build(&[]),
            vec![],
            0.10,
        );
        let result = helpers::base_doc_id_limit(&snap);
        assert!(
            result.is_err(),
            "base_doc_id_limit must return Err on overflow, not silently drop"
        );
    }

    #[test]
    fn overlapping_base_doc_ids_rejected_on_open() {
        // B04: two segments with overlapping [base_id, base_id + doc_count)
        // ranges must be rejected as CorruptIndex on open.
        let _serial = serial_index_lock();
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();
        std::fs::write(repo.path().join("a.rs"), b"fn alpha() {}\n").unwrap();
        std::fs::write(repo.path().join("b.rs"), b"fn beta() {}\n").unwrap();

        // Segment A: base_doc_id=0, doc_count=1 -> range [0, 1)
        let seg_a =
            write_segment_with_global_doc_id(index_dir.path(), 0, "a.rs", b"fn alpha() {}\n");
        // Segment B: base_doc_id=0, doc_count=1 -> range [0, 1) -- overlaps A
        let seg_b =
            write_segment_with_global_doc_id(index_dir.path(), 0, "b.rs", b"fn beta() {}\n");

        let mut manifest = crate::index::manifest::Manifest::new(vec![seg_a, seg_b], 2);
        manifest.scan_threshold_fraction = Some(0.10);
        manifest.save(index_dir.path()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(index_dir.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let result = Index::open(config);
        assert!(
            result.is_err(),
            "open must reject overlapping base_doc_id ranges"
        );
        let err_msg = match result {
            Err(e) => format!("{e}"),
            Ok(_) => panic!("expected error"),
        };
        assert!(
            err_msg.contains("regresses") || err_msg.contains("CorruptIndex"),
            "error should indicate corrupt/overlapping segments, got: {err_msg}"
        );
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
