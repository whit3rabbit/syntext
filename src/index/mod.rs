//! Index builder (`Index::build`) and reader (`Index::open`).

#[cfg(not(target_arch = "wasm32"))]
mod build;
#[cfg(not(target_arch = "wasm32"))]
mod commit;
#[cfg(not(target_arch = "wasm32"))]
mod compact;
#[cfg(not(target_arch = "wasm32"))]
mod compact_plan;
pub(crate) mod encoding;
mod helpers;
pub(crate) mod io_util;
pub(crate) use io_util::open_readonly_nofollow;
#[cfg(any(unix, windows))]
pub(crate) use io_util::verify_fd_matches_stat;
#[cfg(not(target_arch = "wasm32"))]
pub mod manifest;
#[cfg(not(target_arch = "wasm32"))]
mod open;
pub mod overlay;
pub mod pending;
pub mod segment;
pub mod snapshot;
#[cfg(not(target_arch = "wasm32"))]
mod stats;
pub mod walk;
#[cfg(feature = "wasm")]
pub mod wasm_index;

#[cfg(not(target_arch = "wasm32"))]
pub use build::ExternalFileRecord;
pub use snapshot::{BaseSegments, IndexSnapshot};

pub(crate) use encoding::normalize_encoding;
pub use walk::is_binary;

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Component, Path};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use arc_swap::ArcSwap;

#[cfg(not(target_arch = "wasm32"))]
use crate::index::manifest::Manifest;
#[cfg(not(target_arch = "wasm32"))]
use crate::index::overlay::PendingEdits;
#[cfg(not(target_arch = "wasm32"))]
use crate::{Config, IndexError, IndexStats, SearchMatch, SearchOptions};

/// Fraction of base docs beyond which the overlay is considered too large.
#[cfg(not(target_arch = "wasm32"))]
const OVERLAY_WARN_THRESHOLD: f64 = 0.30;

/// Hard cap on total indexed documents across all segments in a manifest.
///
/// A crafted manifest (e.g., sourced from an untrusted SYNTEXT_INDEX_DIR or
/// --index-dir) could claim billions of docs to force a multi-GB
/// `doc_to_file_id` allocation. 50 million is well above any realistic
/// codebase and bounds the allocation to ~200 MB.
#[cfg(not(target_arch = "wasm32"))]
const MAX_TOTAL_DOCS: u32 = 50_000_000;

/// Fraction of base docs beyond which `commit_batch` returns `IndexError::OverlayFull`.
#[cfg(not(target_arch = "wasm32"))]
const OVERLAY_ENFORCE_THRESHOLD: f64 = 0.50;

/// Top-level index handle. Thread-safe via `ArcSwap<IndexSnapshot>`.
#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
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

    /// Rebuild the index by releasing the shared dir lock, running `build_fn`,
    /// then re-acquiring shared.
    ///
    /// # Lock gap
    /// File locks do not support atomic shared-to-exclusive promotion. We must
    /// release shared before `build_fn` can acquire exclusive internally. During
    /// this window another process could grab exclusive and modify the index.
    /// Both the success and error paths call `try_lock_shared`; if re-acquisition
    /// fails (another writer took the lock), we return `LockConflict` rather than
    /// leaving the `Index` without a directory lock.
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

    /// Build the index from a caller-supplied corpus instead of walking the
    /// repository internally.
    ///
    /// This preserves syntext's locking, manifest, calibration, and symbol
    /// build behavior while letting the caller own discovery policy.
    pub fn build_from_file_records(
        config: Config,
        records: Vec<ExternalFileRecord>,
    ) -> Result<Self, IndexError> {
        build::build_index_from_external_records(config, records)
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

        // Quick check: bail early if no compaction is needed.
        {
            let snap = self.snapshot();
            if compact::forced_plan(snap.as_ref(), &self.config).is_none() {
                return Ok(());
            }
        }

        // Acquire write.lock BEFORE taking the snapshot. This prevents a
        // concurrent commit_batch from modifying the overlay between snapshot
        // capture and compact_index's lock acquisition (B13 fix).
        let write_lock = helpers::acquire_writer_lock(&self.config.index_dir)?;
        let snapshot = self.snapshot();
        let Some(plan) = compact::forced_plan(snapshot.as_ref(), &self.config) else {
            return Ok(());
        };

        // Release the shared dir lock so compact_index can acquire exclusive.
        // Same lock-gap caveat as rebuild_with: another process could grab
        // exclusive between unlock and compact_index's lock acquisition.
        // write_lock (held above) prevents concurrent commit_batch; if
        // compact_index fails to lock, we re-acquire shared and return error.
        self._dir_lock.unlock()?;
        let rebuilt = match compact::compact_index(self.config.clone(), snapshot, plan, write_lock)
        {
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

        self.install_rebuilt_index(&rebuilt)?;
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
