//! Git-based change detection for bounded auto-update-on-search.
//!
//! `detect_changed_files` runs three git commands to discover modified, staged,
//! and untracked files since the last index build. An optional elapsed-time
//! budget lets the caller bound latency: when the budget is exhausted between
//! git calls, the function returns a partial `ChangeSet` with
//! `budget_exceeded` set so the caller can proceed with a stale index rather
//! than blocking the search.
//!
//! The three git commands are:
//! 1. `git diff -z --name-only HEAD`  — working-tree changes since HEAD
//! 2. `git diff -z --name-only --cached` — staged changes (covers repos with no commits)
//! 3. `git ls-files -z --others --exclude-standard` — untracked files

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::git_util::is_safe_git_path;
use crate::path_util::{normalize_to_forward_slashes, path_from_bytes};

// fsmonitor helpers were split into a sibling module to keep this file under
// the 400-line quality gate. Re-export so `freshness::enable_fsmonitor` /
// `freshness::maybe_print_fsmonitor_tip` call sites (and the `freshness_tests`
// module via `use super::*`) resolve unchanged.
pub use crate::index::fsmonitor::{enable_fsmonitor, maybe_print_fsmonitor_tip};
#[cfg(test)]
pub(crate) use crate::index::fsmonitor::{is_fsmonitor_enabled, FSMONITOR_TIP_STAMP};

/// Changed-file paths discovered by git, possibly incomplete if the time
/// budget was exhausted before all three git commands completed.
#[derive(Debug, Clone)]
pub struct ChangeSet {
    /// Changed file paths (repo-relative, forward-slash normalized where
    /// the platform applies).
    pub paths: HashSet<PathBuf>,
    /// `Some(n)` when the time budget was exhausted during detection,
    /// where `n` is the number of files found before the budget ran out.
    /// `None` when all three git commands completed within budget.
    pub budget_exceeded: Option<usize>,
    /// Wall-clock time spent running the three git detection commands,
    /// in milliseconds. Measured whether or not the budget was exhausted,
    /// so callers can compare it against their own budget (e.g. to decide
    /// whether to suggest enabling `core.fsmonitor`).
    pub detect_elapsed_ms: u64,
}

/// Errors from change-detection git operations.
#[derive(Debug)]
pub enum FreshnessError {
    /// I/O error from a git command.
    Io(std::io::Error),
}

impl From<std::io::Error> for FreshnessError {
    fn from(err: std::io::Error) -> Self {
        FreshnessError::Io(err)
    }
}

impl std::fmt::Display for FreshnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreshnessError::Io(e) => write!(f, "git detection error: {e}"),
        }
    }
}

/// Outcome of an `update_from_git` call on the index.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum UpdateOutcome {
    /// Successfully applied `files` changed-file notifications. `skipped` is
    /// the count of detected changes that could not be applied (path escaped
    /// the repo, or a per-file notify error): those files are left stale for
    /// the next cycle rather than aborting the whole update. A dangling
    /// symlink is applied as a change, not skipped. A non-zero `skipped`
    /// signals a partial update. `detect_elapsed_ms` is the wall-clock time
    /// the three git detection commands took (see `ChangeSet::detect_elapsed_ms`).
    Updated {
        files: usize,
        skipped: usize,
        detect_elapsed_ms: u64,
    },
    /// No changes detected since the last index build.
    NoChanges { detect_elapsed_ms: u64 },
    /// Time budget exceeded; the index was not updated.
    /// `files_behind_estimate` is the count of changed files discovered
    /// before the budget ran out (a non-zero lower bound).
    BudgetExceeded {
        files_behind_estimate: usize,
        detect_elapsed_ms: u64,
    },
    /// The detected change set exceeded `max_files`; the index was not
    /// updated. Call `Index::update_from_git` with a larger `max_files`
    /// or run `st update` (which has no file-count limit).
    TooManyFiles {
        files_behind: usize,
        detect_elapsed_ms: u64,
    },
    /// Applying the detected changes would push the overlay past its
    /// 50%-of-base cap (`IndexError::OverlayFull`). Returned only from a
    /// *bounded* `update_from_git` call (the search hot path): a full reindex
    /// there would blow the latency budget, so the index is left unchanged and
    /// the caller is expected to search stale and spawn the unbounded catch-up
    /// (`st update`), which rebuilds off the hot path. An *unbounded* call
    /// rebuilds inline instead and never returns this. `files_behind` is the
    /// number of detected changes that could not be applied.
    OverlayFull {
        files_behind: usize,
        detect_elapsed_ms: u64,
    },
}

impl UpdateOutcome {
    /// Wall-clock time the three git detection commands took, regardless of
    /// which variant resulted. Used by the bounded auto-update path to decide
    /// whether detection is slow enough to warrant the `core.fsmonitor` tip.
    pub fn detect_elapsed_ms(&self) -> u64 {
        match self {
            UpdateOutcome::Updated {
                detect_elapsed_ms, ..
            }
            | UpdateOutcome::NoChanges { detect_elapsed_ms }
            | UpdateOutcome::BudgetExceeded {
                detect_elapsed_ms, ..
            }
            | UpdateOutcome::TooManyFiles {
                detect_elapsed_ms, ..
            }
            | UpdateOutcome::OverlayFull {
                detect_elapsed_ms, ..
            } => *detect_elapsed_ms,
        }
    }
}

/// Bounds for an `update_from_git` call: file-count cap and elapsed-time
/// budget. Both are optional; `None` means no limit.
#[derive(Debug, Clone)]
pub struct UpdateLimits {
    /// Maximum number of changed files to process in one call. When the
    /// detected change set exceeds this, the method returns
    /// `UpdateOutcome::TooManyFiles` without applying any changes.
    pub max_files: Option<usize>,
    /// Elapsed-time budget in milliseconds for the three git detection
    /// commands. When exhausted, returns `BudgetExceeded` with a partial
    /// file count.
    pub budget_ms: Option<u64>,
}

/// Parse NUL-terminated git output into repo-relative paths.
///
/// NUL (`\0`) is the only safe record separator for git output: filenames on
/// Unix may contain literal newlines. Splitting on `\n` would produce spurious
/// tokens and yield incorrect results.
///
/// Paths are normalized to forward-slash separators before being returned so
/// that the same file reported by two different git commands (e.g. `git diff
/// HEAD` and `git ls-files --others`) collapses to one entry when inserted
/// into `ChangeSet.paths`. Without this, the same logical path could be
/// counted twice against `max_files` and falsely trip `TooManyFiles`.
fn parse_nul_paths(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|&b| b == 0)
        .map(path_from_bytes)
        .filter(|path| is_safe_git_path(path))
        .map(normalize_to_forward_slashes)
        .collect()
}

/// Detect changed files in the repository.
///
/// Runs the three git detection commands against `repo_root` using `git`,
/// each bounded by an absolute `deadline` derived from `budget_ms`. When the
/// budget is exhausted mid-command the running git process is killed, the
/// partial result so far is returned with `budget_exceeded` set, and later
/// commands are skipped.
///
/// # Error vs. empty-result semantics
///
/// Git commands that SPAWN but exit non-zero are treated as "no data from
/// this command", NOT an error: `git diff HEAD` exits non-zero on a repo with
/// no commits, and all three exit non-zero in a non-git directory, which st
/// supports (indexes can be built without git). Such repos degrade cleanly to
/// `NoChanges`. Only a failure to SPAWN git (broken exec) is surfaced as
/// `FreshnessError::Io`, signalling a genuinely broken git setup rather than
/// an empty result.
///
/// # Security
///
/// `repo_root` must be canonicalized by the caller before passing here (see
/// `cli/manage.rs::cmd_update` for rationale). `git` must be the resolved
/// absolute path from `resolve_git_binary()`.
pub fn detect_changed_files(
    repo_root: &Path,
    git: &Path,
    budget_ms: Option<u64>,
) -> Result<ChangeSet, FreshnessError> {
    let start = Instant::now();
    let deadline = budget_ms.map(|ms| start + Duration::from_millis(ms));
    let mut changed: HashSet<PathBuf> = HashSet::new();

    let commands: [&[&str]; 3] = [
        // Fails gracefully (non-zero) on repos with no commits.
        &["diff", "-z", "--name-only", "HEAD"],
        // Staged changes (covers initial commit / repos with no commits).
        &["diff", "-z", "--name-only", "--cached"],
        // Untracked files that git-diff doesn't report.
        &["ls-files", "-z", "--others", "--exclude-standard"],
    ];

    for args in commands {
        // Skip (and report budget exhaustion) once the deadline has passed, so
        // budget_ms=0 performs no git work at all instead of running the first
        // command unbounded.
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(partial(changed, start));
        }
        match run_git_bounded(git, repo_root, args, deadline)? {
            GitOutput::Complete(stdout) => changed.extend(parse_nul_paths(&stdout)),
            GitOutput::Partial(stdout) => {
                // Keep what git managed to emit (a real lower-bound
                // estimate), but the set is incomplete: report budget
                // exhaustion even when this was the FINAL command.
                // Previously a kill here returned `Ok(Some(buf))` (same shape
                // as success), fell through the loop, and the result claimed
                // `budget_exceeded: None` — masking staleness and suppressing
                // the detached async catch-up.
                changed.extend(parse_nul_paths(&stdout));
                return Ok(partial(changed, start));
            }
            GitOutput::NoData => {}
        }
    }

    Ok(ChangeSet {
        budget_exceeded: None,
        detect_elapsed_ms: elapsed_ms(start),
        paths: changed,
    })
}

/// Milliseconds elapsed since `start`, saturating to `u64::MAX` (wall-clock
/// detection times never approach this, so saturation is unreachable in
/// practice but avoids a panic on an as-cast overflow).
fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Outcome of one bounded git command.
///
/// `Partial` is the load-bearing variant: a command killed at the deadline
/// previously returned the same shape as a clean success (`Ok(Some(buf))`),
/// so a budget hit during the LAST detection command was reported as a
/// complete change set (`budget_exceeded: None`) — suppressing the staleness
/// notice and the detached async catch-up. Callers must treat `Partial` as
/// budget exhaustion, and durable-delta callers must fail closed on it.
#[derive(Debug)]
pub(super) enum GitOutput {
    /// git exited 0; stdout is the complete output.
    Complete(Vec<u8>),
    /// git was killed at the deadline; stdout holds whatever it wrote before
    /// the kill. A torn final `-z` record has no trailing NUL and is filtered
    /// by `parse_nul_paths` / `is_safe_git_path`, so the prefix is safe to
    /// parse for a lower-bound files-behind estimate.
    Partial(Vec<u8>),
    /// git exited non-zero (repo with no commits, or a non-git directory):
    /// "no data from this command", not an error.
    NoData,
}

/// Build a partial `ChangeSet` flagged as budget-exhausted.
fn partial(changed: HashSet<PathBuf>, start: Instant) -> ChangeSet {
    ChangeSet {
        budget_exceeded: Some(changed.len()),
        detect_elapsed_ms: elapsed_ms(start),
        paths: changed,
    }
}

/// Run one git command, bounding wall-clock time by `deadline`.
///
/// Returns a [`GitOutput`] so a deadline kill (`Partial`) is never confused
/// with a clean success (`Complete`) or a legitimate no-data non-zero exit
/// (`NoData`). See the variant docs for why that distinction is load-bearing.
///
/// With a deadline, stdout is drained on a reader thread while the main thread
/// polls `try_wait` and kills the child at the deadline. Draining concurrently
/// means git never blocks on a full OS pipe buffer (~64 KB), so a fast but
/// verbose command (`git diff --name-only` on a heavy-churn repo emits far more
/// than 64 KB) actually *completes* within budget instead of stalling on
/// backpressure and being killed. If the deadline is genuinely hit, the reader
/// still recovers whatever git wrote before the kill, and that partial output
/// is returned (not discarded) so the caller gets a real, non-zero
/// files-behind estimate. Without a deadline (CLI `st update`),
/// `wait_with_output` drains concurrently for the same reason.
pub(super) fn run_git_bounded(
    git: &Path,
    repo_root: &Path,
    args: &[&str],
    deadline: Option<Instant>,
) -> Result<GitOutput, FreshnessError> {
    let mut child = Command::new(git)
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(FreshnessError::Io)?;

    // No budget (CLI update): drain stdout concurrently, then wait.
    let Some(deadline) = deadline else {
        let output = child.wait_with_output().map_err(FreshnessError::Io)?;
        return Ok(if output.status.success() {
            GitOutput::Complete(output.stdout)
        } else {
            GitOutput::NoData
        });
    };

    // Bounded: drain stdout on a thread so git never blocks on a full pipe,
    // while the main thread polls try_wait and kills the child at the deadline. Killing
    // the child closes its stdout write end, so read_to_end returns and the
    // thread finishes. Portable (no wait_timeout on Windows); 2 ms poll keeps
    // the overshoot negligible.
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout {
            let _ = out.read_to_end(&mut buf);
        }
        buf
    });

    let killed = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(None) => {
                // Deadline elapsed: kill the (possibly still-running) child.
                let _ = child.kill();
                break true;
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(FreshnessError::Io(e));
            }
        }
    };

    let status = child.wait().map_err(FreshnessError::Io)?;
    let buf = reader.join().unwrap_or_default();

    // Deadline kill: hand back the partial output — the caller reports a
    // real, non-zero behind estimate — but tagged as `Partial` so it can
    // never be mistaken for a complete change set. (A truncated final `-z`
    // record has no trailing NUL and is filtered by `parse_nul_paths` /
    // `is_safe_git_path`, so it only perturbs an estimate.)
    if killed {
        return Ok(GitOutput::Partial(buf));
    }
    // Clean but non-zero exit (no-commits / non-git repo): no data.
    if !status.success() {
        return Ok(GitOutput::NoData);
    }
    Ok(GitOutput::Complete(buf))
}

#[cfg(test)]
#[path = "freshness_tests.rs"]
mod tests;
