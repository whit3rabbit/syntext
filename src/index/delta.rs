//! Durable incremental HEAD-move updates: committed-change detection, the
//! delta-vs-rebuild decision, and the apply path.
//!
//! This is the cheap replacement for a full rebuild on every commit. A moved
//! HEAD (post-commit/checkout/merge/rewrite hooks) leaves a clean working tree
//! that `freshness::detect_changed_files` cannot see, so we diff
//! `base_commit..HEAD` directly. The apply reuses the hardened overlay path:
//! buffer the changes (`apply_changed_paths`), `commit_batch` (which reads
//! files with the same TOCTOU guards, computes the delete-set, and maintains
//! the symbol index), then flush the resulting in-memory overlay to a durable
//! delta segment plus a persistent delete-set (`deletes_idx`). The flush +
//! reopen lives in `delta_apply` to keep this file under the 400-line gate.

// io::Error::new(ErrorKind::Other, ...) is used instead of io::Error::other()
// for Rust < 1.74 compatibility (Windows CI constraint), matching manifest.rs.
#![allow(clippy::io_other_error)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use super::{delta_apply, helpers, Index};
use crate::git_util::is_safe_git_path;
use crate::index::freshness::FreshnessError;
use crate::index::manifest::Manifest;
use crate::path_util::{normalize_to_forward_slashes, path_from_bytes};
use crate::IndexError;

fn is_hex_commit(s: &str) -> bool {
    (40..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Upper bound on files in a single delta apply. A larger commit (bulk import,
/// vendored tree) falls back to a full rebuild, which is cheaper than appending
/// one enormous segment and immediately compacting it.
const DELTA_MAX_FILES: usize = 5000;

/// Result of an attempted delta apply.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DeltaOutcome {
    /// The delta was applied durably; the caller returns its stats.
    Applied,
    /// The change set is not safe/cheap to apply incrementally (e.g. the
    /// overlay would exceed its cap); the caller falls back to a full rebuild.
    Fallback,
}

/// Committed changes between two commits, split by kind. Paths are
/// repository-relative and forward-slash normalized to match the keys in
/// `BaseSegments::path_doc_ids`.
#[derive(Debug, Default)]
pub(super) struct CommittedChanges {
    /// Newly added files.
    pub added: Vec<PathBuf>,
    /// Modified files.
    pub modified: Vec<PathBuf>,
    /// Deleted files.
    pub deleted: Vec<PathBuf>,
}

impl CommittedChanges {
    /// Total number of change records across all three buckets.
    pub fn len(&self) -> usize {
        self.added.len() + self.modified.len() + self.deleted.len()
    }

    /// True when git reported no committed differences.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The union of every changed path, as repo-relative paths, for feeding to
    /// `apply_changed_paths` (which re-classifies present-vs-absent from the
    /// filesystem, so deletes are handled without a separate list).
    fn all_paths(&self) -> HashSet<PathBuf> {
        self.added
            .iter()
            .chain(self.modified.iter())
            .chain(self.deleted.iter())
            .cloned()
            .collect()
    }
}

/// Detect committed changes between `base_commit` and `HEAD`.
///
/// Runs `git diff --name-status -z <base_commit> HEAD`. Renames are decomposed
/// into a delete of the old path plus an add of the new path (a safe superset;
/// we never rely on rename similarity for correctness). Copies add only the new
/// path. Paths that fail `is_safe_git_path` (traversal, absolute) are dropped.
///
/// `git` must be the resolved absolute path from `resolve_git_binary()` and
/// `repo_root` must already be canonicalized by the caller.
pub(super) fn detect_committed_changes(
    repo_root: &Path,
    git: &Path,
    base_commit: &str,
    deadline: Option<Instant>,
) -> Result<CommittedChanges, FreshnessError> {
    if !is_hex_commit(base_commit) {
        return Err(FreshnessError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid base_commit",
        )));
    }
    let args = [
        "diff",
        "--no-renames",
        "--name-status",
        "-z",
        "--end-of-options",
        base_commit,
        "HEAD",
    ];
    let output = super::freshness::run_git_bounded(git, repo_root, &args, deadline)?;
    let bytes = output.ok_or_else(|| {
        FreshnessError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "git diff exited non-zero or was killed at deadline",
        ))
    })?;
    parse_name_status_z(&bytes)
}

/// Parse NUL-separated `git diff --name-status -z` records.
///
/// Each record is a status field followed by one path (`A`/`M`/`D`/`T`), all NUL-terminated.
/// Fail closed if any other status or malformed record is found.
fn parse_name_status_z(bytes: &[u8]) -> Result<CommittedChanges, FreshnessError> {
    let mut changes = CommittedChanges::default();
    let mut tokens: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    if tokens.last().is_some_and(|t| t.is_empty()) {
        tokens.pop();
    }

    let clean = |raw: &[u8]| -> Result<PathBuf, FreshnessError> {
        let p = path_from_bytes(raw);
        if !is_safe_git_path(&p) {
            return Err(FreshnessError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsafe path in git diff",
            )));
        }
        Ok(normalize_to_forward_slashes(p))
    };

    let mut i = 0;
    while i < tokens.len() {
        let kind = tokens[i].first().copied().unwrap_or(b'?');
        i += 1;
        match kind {
            b'A' => {
                let path_token = tokens.get(i).ok_or_else(|| {
                    FreshnessError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "malformed name-status output (missing path for added)",
                    ))
                })?;
                let p = clean(path_token)?;
                changes.added.push(p);
                i += 1;
            }
            b'M' | b'T' => {
                let path_token = tokens.get(i).ok_or_else(|| {
                    FreshnessError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "malformed name-status output (missing path for modified)",
                    ))
                })?;
                let p = clean(path_token)?;
                changes.modified.push(p);
                i += 1;
            }
            b'D' => {
                let path_token = tokens.get(i).ok_or_else(|| {
                    FreshnessError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "malformed name-status output (missing path for deleted)",
                    ))
                })?;
                let p = clean(path_token)?;
                changes.deleted.push(p);
                i += 1;
            }
            _ => {
                return Err(FreshnessError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unsupported or malformed status kind: {}", kind as char),
                )));
            }
        }
    }
    Ok(changes)
}

/// True when `head` is a descendant of `base` (`git merge-base --is-ancestor`).
/// A rebase / amend / force-push makes `base` unreachable from `head`, so a
/// delta over `base..HEAD` would be wrong; those cases fall back to a rebuild.
pub(super) fn is_ancestor(git: &Path, repo_root: &Path, base: &str, head: &str) -> bool {
    if !is_hex_commit(base) || !is_hex_commit(head) {
        return false;
    }
    Command::new(git)
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", "--is-ancestor", "--end-of-options", base, head])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Decide whether a detected change set can be applied as a delta rather than a
/// full rebuild. Conservative on purpose: any "no" is answered by a rebuild,
/// which is always correct.
pub(super) fn should_delta(
    manifest: &Manifest,
    changes: &CommittedChanges,
    config: &crate::Config,
) -> bool {
    !changes.is_empty()
        && changes.len() <= DELTA_MAX_FILES
        // Leave room for the appended segment; at the cap, rebuild (which resets
        // segment count) instead of appending an over-cap segment.
        // Micro-optimization: only require headroom when we are going to write a new segment (added/modified is non-empty).
        && (changes.added.is_empty() && changes.modified.is_empty()
            || manifest.segments.len() < config.max_segments.max(1))
}

impl Index {
    /// Attempt a durable delta for a moved HEAD, returning `Some(stats)` if one
    /// was applied and `None` if the caller should fall back to a full rebuild.
    ///
    /// Falls back (`None`) when there is no recorded `base_commit`, HEAD is not a
    /// descendant of it (rebase/amend/force-push), git detection fails, the
    /// change set is too large (`should_delta`), or the apply overflows the
    /// overlay. A delta apply error is logged (verbose) and answered by a
    /// rebuild, never surfaced, since a rebuild is always correct.
    pub(super) fn try_committed_delta(
        &self,
        manifest: &Manifest,
        current_head: Option<&str>,
    ) -> Result<Option<crate::IndexStats>, IndexError> {
        let (Some(base), Some(head)) = (manifest.base_commit.as_deref(), current_head) else {
            return Ok(None);
        };
        let git = crate::git_util::resolve_git_binary();
        if !git.is_file() || !is_ancestor(&git, &self.canonical_root, base, head) {
            return Ok(None);
        }
        let Ok(changes) = detect_committed_changes(&self.canonical_root, &git, base, None) else {
            return Ok(None);
        };
        if !should_delta(manifest, &changes, &self.config) {
            return Ok(None);
        }
        match self.apply_committed_delta_update(changes) {
            Ok(DeltaOutcome::Applied) => {
                // Bound segment growth / reclaim superseded base docs.
                // maybe_compact commits nothing new here (pending is empty
                // post-delta) and only fires when a threshold is crossed.
                self.maybe_compact()?;
                Ok(Some(self.stats()))
            }
            Ok(DeltaOutcome::Fallback) => Ok(None),
            Err(e) => {
                if self.config.verbose {
                    eprintln!("syntext: delta apply failed ({e}); full rebuild instead");
                }
                Ok(None)
            }
        }
    }

    /// Apply a committed-change delta durably and swap it into this handle.
    ///
    /// Buffers the changes onto the overlay and commits them (reusing
    /// `commit_batch`'s hardened read + delete-set + symbol maintenance), then
    /// flushes the overlay to a durable delta segment via
    /// [`delta_apply::flush_overlay_as_delta`]. Returns [`DeltaOutcome::Fallback`]
    /// when the overlay would overflow its cap (delta too large for an
    /// incremental apply). The caller (`rebuild_if_stale`) then does a full
    /// rebuild, which is always correct.
    pub(super) fn apply_committed_delta_update(
        &self,
        changes: CommittedChanges,
    ) -> Result<DeltaOutcome, IndexError> {
        let (_applied, _skipped) = self.apply_changed_paths(&changes.all_paths());
        match self.commit_batch() {
            Ok(()) => {}
            // Overlay would exceed its 50%-of-base cap: too big to apply
            // incrementally. commit_batch's RequeueGuard already requeued the
            // buffered edits; a subsequent full rebuild reindexes from the tree.
            Err(IndexError::OverlayFull { .. }) => return Ok(DeltaOutcome::Fallback),
            Err(e) => return Err(e),
        }

        let head = helpers::current_repo_head(&self.config.repo_root)?;

        // Same lock choreography as compact(): take the writer lock, capture the
        // just-committed snapshot, release the shared dir lock, let the flush
        // acquire exclusive + reopen, then install. On error re-acquire shared.
        let write_lock = helpers::acquire_writer_lock(&self.config.index_dir)?;
        let snapshot = self.snapshot();

        self._dir_lock.unlock()?;
        let rebuilt = match delta_apply::flush_overlay_as_delta(
            self.config.clone(),
            snapshot,
            head,
            write_lock,
        ) {
            Ok(rebuilt) => rebuilt,
            Err(err) => {
                if let Err(e) = self._dir_lock.try_lock_shared() {
                    if self.config.verbose {
                        eprintln!(
                            "syntext: warning: failed to re-acquire shared directory lock after delta error: {e}"
                        );
                    }
                }
                return Err(err);
            }
        };
        self._dir_lock
            .try_lock_shared()
            .map_err(|_| IndexError::LockConflict(self.config.index_dir.clone()))?;

        self.install_rebuilt_index(&rebuilt)?;
        Ok(DeltaOutcome::Applied)
    }
}
