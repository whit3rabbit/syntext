//! `Index::update_from_git` and `Index::search_fresh`: git-driven bounded
//! auto-update on search.
//!
//! Split from `mod.rs` to keep it under the 400-line quality gate. As a child
//! module of `index`, this reaches `Index`'s private helpers (`notify_change`,
//! `notify_delete`, `commit_batch`, `repo_relative_path`) directly.

use crate::index::freshness;
use crate::{IndexError, SearchMatch, SearchOptions};

impl super::Index {
    /// Detect changed files via git and apply them to the overlay.
    ///
    /// Runs the three git detection commands bounded by `limits.budget_ms`.
    /// When the budget is exhausted or the file count exceeds
    /// `limits.max_files`, no changes are applied and the corresponding
    /// `UpdateOutcome` variant is returned so the caller can proceed with
    /// a stale index.
    ///
    /// Path verification mirrors `cmd_update` in `cli/manage.rs`:
    /// canonicalize + check that the resolved path is still under
    /// `canonical_root` before calling `notify_change`/`notify_delete`.
    pub fn update_from_git(
        &self,
        limits: freshness::UpdateLimits,
    ) -> Result<freshness::UpdateOutcome, IndexError> {
        let git = crate::git_util::resolve_git_binary();
        if !git.is_file() {
            return Err(IndexError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("git not found at {}", git.display()),
            )));
        }

        let change_set =
            freshness::detect_changed_files(&self.canonical_root, &git, limits.budget_ms).map_err(
                |e| match e {
                    freshness::FreshnessError::Io(io) => IndexError::Io(io),
                },
            )?;

        let detect_elapsed_ms = change_set.detect_elapsed_ms;

        if let Some(behind) = change_set.budget_exceeded {
            return Ok(freshness::UpdateOutcome::BudgetExceeded {
                files_behind_estimate: behind,
                detect_elapsed_ms,
            });
        }

        if change_set.paths.is_empty() {
            return Ok(freshness::UpdateOutcome::NoChanges { detect_elapsed_ms });
        }

        // Apply max_files limit before doing any work.
        if let Some(max) = limits.max_files {
            if change_set.paths.len() > max {
                return Ok(freshness::UpdateOutcome::TooManyFiles {
                    files_behind: change_set.paths.len(),
                    detect_elapsed_ms,
                });
            }
        }

        let (count, skipped) = self.apply_changed_paths(&change_set.paths);

        // Applying the buffered changes would push the overlay past its
        // 50%-of-base cap (`OverlayFull`). `commit_batch` rejects before
        // building the overlay, so the changed files land nowhere and a
        // follow-up search would silently run stale and miss files that exist
        // on disk (a Tier-A false negative against the live tree).
        //
        // Recovery depends on whether this call is bounded:
        // - Bounded (search hot path, `budget_ms`/`max_files` set): a full
        //   reindex here would blow the latency budget the caller promised, so
        //   return `OverlayFull` and let the caller search stale + spawn the
        //   unbounded `st update` catch-up, which rebuilds off the hot path.
        // - Unbounded (CLI `st update`): reindex the working tree inline, since
        //   `OverlayFull`'s contract is "callers need a full reindex anyway".
        //   `commit_batch`'s RequeueGuard already requeued the buffered edits;
        //   the rebuild reindexes them from the tree and resets pending.
        //   Requires the `ignore` feature for tree walking; without it there is
        //   no rebuild path, so the error propagates as before.
        let bounded = limits.max_files.is_some() || limits.budget_ms.is_some();
        match self.commit_batch() {
            Ok(()) => {}
            Err(e) => match e {
                IndexError::OverlayFull { .. } if bounded => {
                    return Ok(freshness::UpdateOutcome::OverlayFull {
                        files_behind: change_set.paths.len(),
                        detect_elapsed_ms,
                    });
                }
                #[cfg(feature = "ignore")]
                IndexError::OverlayFull { .. } => {
                    self.rebuild_with(crate::index::build::build_index)?;
                }
                other => return Err(other),
            },
        }

        Ok(freshness::UpdateOutcome::Updated {
            files: count,
            skipped,
            detect_elapsed_ms,
        })
    }

    /// Apply a set of changed/deleted repo-relative paths to the overlay via
    /// `notify_change`/`notify_delete`, returning `(applied, skipped)`.
    ///
    /// Extracted from `update_from_git` so the path-escape guard lives in one
    /// place. Callers must still call `commit_batch` afterward to make the
    /// changes visible. Does not enforce `max_files`: callers gate that.
    ///
    /// Security: each path is joined onto `canonical_root`, canonicalized, and
    /// verified to still resolve under `canonical_root` before `notify_change`,
    /// so a compromised git binary emitting escape paths cannot index or delete
    /// outside the repo. Per-file failures are skipped, never fatal, matching
    /// `commit_batch`'s skip-on-failure semantics.
    pub(super) fn apply_changed_paths(
        &self,
        paths: &std::collections::HashSet<std::path::PathBuf>,
    ) -> (usize, usize) {
        let mut count = 0usize;
        let mut skipped = 0usize;
        for path in paths {
            let abs = self.canonical_root.join(path);
            // symlink_metadata (not exists()) distinguishes "gone entirely"
            // (notify_delete) from "present but unresolvable" such as a broken
            // symlink: exists() follows symlinks and reports a broken symlink
            // as absent, which would misclassify a git-reported modification as
            // a deletion and evict a still-present path.
            let present = std::fs::symlink_metadata(&abs).is_ok();
            if present {
                // Canonicalize and verify the resolved path is still under
                // canonical_root. A compromised git binary could emit paths
                // that exploit OS-specific resolution to escape the repo.
                match abs.canonicalize() {
                    Ok(resolved) if resolved.starts_with(&self.canonical_root) => {
                        // Per-file notify errors no longer abort the batch
                        // (matching commit_batch's skip-on-failure semantics);
                        // a single bad path cannot wedge the whole update.
                        if let Err(e) = self.notify_change(&resolved) {
                            if self.config.verbose {
                                eprintln!(
                                    "syntext: warning: skip changed file {}: {e}",
                                    path.display()
                                );
                            }
                            skipped += 1;
                        } else {
                            count += 1;
                        }
                    }
                    // Resolves outside repo root: a compromised git binary or
                    // symlinked path escaping canonical_root. Skip rather than
                    // index or delete.
                    Ok(_escaped) => skipped += 1,
                    // canonicalize() failed to resolve the final component,
                    // e.g. a dangling symlink whose target does not exist.
                    // The entry itself is present (symlink_metadata succeeded
                    // above), so this is still a change, not a deletion: fall
                    // back to notify_change with the un-canonicalized path.
                    // notify_change's own repo_relative_path /
                    // path_has_intermediate_symlink check still guards
                    // against a symlinked *parent* directory escaping
                    // canonical_root; only the unresolvable final target is
                    // left unchecked here, which is safe because there is no
                    // real target to escape to.
                    Err(_) => {
                        if let Err(e) = self.notify_change(&abs) {
                            if self.config.verbose {
                                eprintln!(
                                    "syntext: warning: skip changed file {}: {e}",
                                    path.display()
                                );
                            }
                            skipped += 1;
                        } else {
                            count += 1;
                        }
                    }
                }
            } else {
                // NOTE: there is a narrow TOCTOU window between this existence
                // check and commit_batch's eviction: a file deleted then
                // recreated before the commit is evicted as a deletion and is
                // absent from the new snapshot until the NEXT update cycle,
                // when git re-reports it changed. The race is self-healing.
                if let Err(e) = self.notify_delete(&abs) {
                    if self.config.verbose {
                        eprintln!(
                            "syntext: warning: skip deleted file {}: {e}",
                            path.display()
                        );
                    }
                    skipped += 1;
                } else {
                    count += 1;
                }
            }
        }
        (count, skipped)
    }

    /// Bounded auto-update-on-search: run `update_from_git(limits)`, then
    /// search, in one call.
    ///
    /// This is the one-call library equivalent of what the CLI wires up by
    /// hand across `cli/catchup.rs::run_bounded_auto_update` and the search
    /// command: detect + apply changes bounded by `limits`, then search
    /// against whatever the index looks like afterward.
    ///
    /// Same "stale search is safe" contract as `run_bounded_auto_update`:
    /// `update_from_git`'s `Err` is never surfaced as a hard failure here.
    /// A stale index can only miss matches, never report wrong ones, because
    /// the verifier re-reads real file bytes. On error, search proceeds
    /// against the stale index and the returned `UpdateOutcome` is
    /// `NoChanges` with `detect_elapsed_ms: 0` -- there is no real "nothing
    /// to report" variant for "detection itself failed", and `NoChanges` is
    /// the variant that already means "proceed with what's on disk".
    ///
    /// Only `self.search`'s own `Err` is propagated: a failed search is a
    /// real error, unlike a failed freshness check.
    pub fn search_fresh(
        &self,
        pattern: &str,
        opts: &SearchOptions,
        limits: freshness::UpdateLimits,
    ) -> Result<(Vec<SearchMatch>, freshness::UpdateOutcome), IndexError> {
        let outcome = self
            .update_from_git(limits)
            .unwrap_or(freshness::UpdateOutcome::NoChanges {
                detect_elapsed_ms: 0,
            });
        let matches = self.search(pattern, opts)?;
        Ok((matches, outcome))
    }
}
