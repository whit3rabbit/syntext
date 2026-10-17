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

use super::{helpers, Index};
use crate::git_util::{is_hex_commit, is_safe_git_path};
use crate::index::freshness::FreshnessError;
use crate::index::manifest::Manifest;
use crate::path_util::{normalize_to_forward_slashes, path_from_bytes};
use crate::IndexError;

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

/// Why a moved HEAD was not applied as a delta. Every variant is answered by
/// a full rebuild, which is always correct, so a fallback costs time, not
/// results, and nothing else reports it. `rebuild_if_stale` logs it so a
/// rebuild that should have been a delta says which gate sent it there.
#[derive(Debug)]
pub(super) enum DeltaFallback {
    /// The manifest records no `base_commit` (built outside git, or HEAD was
    /// unreadable when it was built).
    NoBaseCommit,
    /// `git rev-parse HEAD` produced no commit now.
    NoHead,
    /// No git binary at the resolved path.
    GitMissing,
    /// git answered that `base_commit` is not an ancestor of HEAD.
    NotAncestor,
    /// The ancestry check itself failed: a spawn error or an unexpected exit.
    AncestryUnknown(std::io::Error),
    /// `git diff --name-status` failed or returned incomplete output.
    DetectFailed(FreshnessError),
    /// Empty change set, over `DELTA_MAX_FILES`, or no segment headroom.
    NotWorthIt(usize),
    /// The change set would overflow the overlay cap.
    OverlayFull,
    /// The apply or the durable flush returned an error.
    ApplyFailed(IndexError),
}

impl std::fmt::Display for DeltaFallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBaseCommit => write!(f, "no base_commit in the manifest"),
            Self::NoHead => write!(f, "HEAD is unreadable"),
            Self::GitMissing => write!(f, "git binary not found"),
            Self::NotAncestor => write!(f, "base_commit is not an ancestor of HEAD"),
            Self::AncestryUnknown(e) => write!(f, "ancestry check failed: {e}"),
            Self::DetectFailed(e) => write!(f, "{e}"),
            Self::NotWorthIt(n) => write!(f, "{n} changed path(s) do not fit a delta"),
            Self::OverlayFull => write!(f, "change set overflows the overlay"),
            Self::ApplyFailed(e) => write!(f, "delta apply failed: {e}"),
        }
    }
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
    let bytes = match super::freshness::run_git_bounded(git, repo_root, &args, deadline)? {
        super::freshness::GitOutput::Complete(bytes) => bytes,
        // A partial name-status list must NEVER seed a durable delta:
        // applying an incomplete change set advances `base_commit` past files
        // it never applied, leaving them permanently missing from the index
        // (no later detection pass re-reports them). `NoData` (non-zero exit)
        // is likewise not a valid diff. Fail closed; the caller
        // (`try_committed_delta`) answers any `Err` with a full rebuild,
        // which is always correct.
        super::freshness::GitOutput::Partial(_) | super::freshness::GitOutput::NoData => {
            return Err(FreshnessError::Io(std::io::Error::other(
                "git diff --name-status produced no complete output",
            )));
        }
    };
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

/// Whether `head` is a descendant of `base` (`git merge-base --is-ancestor`).
/// A rebase / amend / force-push makes `base` unreachable from `head`, so a
/// delta over `base..HEAD` would be wrong; those cases fall back to a rebuild.
///
/// `Ok(false)` only for git's own "not an ancestor" answer (exit 1). A spawn
/// failure or any other exit is `Err`: ancestry is unknown, which still means
/// a rebuild, but the log must not call it a rewritten history.
pub(super) fn is_ancestor(
    git: &Path,
    repo_root: &Path,
    base: &str,
    head: &str,
) -> std::io::Result<bool> {
    if !is_hex_commit(base) || !is_hex_commit(head) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid commit id",
        ));
    }
    let status = Command::new(git)
        .arg("-C")
        .arg(repo_root)
        .args([
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            base,
            head,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(std::io::Error::other(format!(
            "git merge-base --is-ancestor: {status}"
        ))),
    }
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
    /// Attempt a durable delta for a moved HEAD, returning `Ok(stats)` if one
    /// was applied and `Err(reason)` if the caller should fall back to a full
    /// rebuild.
    ///
    /// Falls back when there is no recorded `base_commit`, HEAD is not a
    /// descendant of it (rebase/amend/force-push), git detection fails, the
    /// change set is too large (`should_delta`), or the apply overflows the
    /// overlay. A delta apply error is answered by a rebuild, never surfaced,
    /// since a rebuild is always correct. The caller logs the reason.
    pub(super) fn try_committed_delta(
        &self,
        manifest: &Manifest,
        current_head: Option<&str>,
    ) -> Result<Result<crate::IndexStats, DeltaFallback>, IndexError> {
        let Some(base) = manifest.base_commit.as_deref() else {
            return Ok(Err(DeltaFallback::NoBaseCommit));
        };
        let Some(head) = current_head else {
            return Ok(Err(DeltaFallback::NoHead));
        };
        let git = crate::git_util::resolve_git_binary();
        if !git.is_file() {
            return Ok(Err(DeltaFallback::GitMissing));
        }
        match is_ancestor(&git, &self.canonical_root, base, head) {
            Ok(true) => {}
            Ok(false) => return Ok(Err(DeltaFallback::NotAncestor)),
            Err(e) => return Ok(Err(DeltaFallback::AncestryUnknown(e))),
        }
        let changes = match detect_committed_changes(&self.canonical_root, &git, base, None) {
            Ok(changes) => changes,
            Err(e) => return Ok(Err(DeltaFallback::DetectFailed(e))),
        };
        if !should_delta(manifest, &changes, &self.config) {
            return Ok(Err(DeltaFallback::NotWorthIt(changes.len())));
        }
        match self.apply_committed_delta_update(changes) {
            Ok(DeltaOutcome::Applied) => {
                // Bound segment growth / reclaim superseded base docs.
                // maybe_compact commits nothing new here (pending is empty
                // post-delta) and only fires when a threshold is crossed.
                self.maybe_compact()?;
                Ok(Ok(self.stats()))
            }
            Ok(DeltaOutcome::Fallback) => Ok(Err(DeltaFallback::OverlayFull)),
            Err(e) => Ok(Err(DeltaFallback::ApplyFailed(e))),
        }
    }

    /// Apply a committed-change delta durably and swap it into this handle.
    ///
    /// Buffers the changes onto the overlay and commits them (reusing
    /// `commit_batch`'s hardened read + delete-set + symbol maintenance), then
    /// flushes the overlay to a durable delta segment via
    /// [`super::flush::Index::run_durable_flush`]. Returns [`DeltaOutcome::Fallback`]
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

        // Lock choreography, snapshot capture, flush and install all live in
        // `flush::run_durable_flush`, shared with the uncommitted-drift caller.
        // The only difference is here, in the argument: this path really did
        // advance to a new commit.
        self.run_durable_flush(super::flush::FlushAnchor::AdvanceHead(head))?;
        Ok(DeltaOutcome::Applied)
    }
}
