//! Bounded auto-update-on-search: runs git change detection before a search,
//! emits the staleness notice, and (when still stale afterward) spawns a
//! detached `st update --quiet` catch-up in the background so a later search
//! sees a fresher index without the current search paying the unbounded
//! update cost.

use crate::index::Index;
use crate::{Config, IndexError};

/// Runs the bounded auto-update git-detection step ahead of a search and
/// emits the staleness notice on stderr when applicable.
///
/// On any error (including `LockConflict` and `OverlayFull`), search
/// proceeds with the stale index -- a stale index can only miss matches,
/// never report wrong ones, because the verifier re-reads real file bytes.
///
/// Returns `true` when detection reported real staleness
/// (`TooManyFiles`/`BudgetExceeded` with a positive estimate) regardless of
/// `quiet`, so the caller can still spawn the detached catch-up even when
/// the stderr notice itself is suppressed.
pub(super) fn run_bounded_auto_update(index: &Index, config: &Config, quiet: bool) -> bool {
    if !config.auto_update {
        return false;
    }

    use crate::index::freshness::{self, UpdateLimits, UpdateOutcome};
    let limits = UpdateLimits {
        max_files: Some(config.auto_update_max_files),
        budget_ms: Some(config.auto_update_budget_ms),
    };
    match index.update_from_git(limits) {
        Ok(outcome) => {
            // Best-effort UX hint: fires at most once (stamp file in the
            // index dir) and only when detection ate more than half the
            // budget with core.fsmonitor still unset. Never affects the
            // outcome below or the caller's exit code.
            let git = crate::git_util::resolve_git_binary();
            freshness::maybe_print_fsmonitor_tip(
                &index.canonical_root,
                &git,
                &config.index_dir,
                outcome.detect_elapsed_ms(),
                config.auto_update_budget_ms,
            );

            // Any of these three means "index is behind and stays behind after
            // this bounded pass": spawn the detached catch-up regardless of the
            // estimate. The estimate can legitimately be 0 (git emitted nothing
            // within the budget on a heavy-churn repo), so gating the *spawn* on
            // `> 0` would leave the repos most behind unable to self-heal; only
            // the human-facing stderr notice is gated on a positive estimate.
            match &outcome {
                UpdateOutcome::BudgetExceeded {
                    files_behind_estimate: n,
                    ..
                }
                | UpdateOutcome::TooManyFiles {
                    files_behind: n, ..
                }
                | UpdateOutcome::OverlayFull {
                    files_behind: n, ..
                } => {
                    if !quiet && *n > 0 {
                        eprintln!(
                            "st: index is ~{n} files behind; \
                             searching stale (run 'st update')"
                        );
                    }
                    true
                }
                _ => false,
            }
        }
        Err(IndexError::LockConflict(_)) => {
            // Another process is updating; search stale silently.
            // Exit-code contract: no early return here, so this arm never
            // changes the 0/1/2 outcome the caller returns -- it only decides
            // whether we print an informational line to stderr. stdout is
            // untouched.
            false
        }
        Err(IndexError::OverlayFull { .. }) => {
            if !quiet {
                eprintln!("st: overlay full; run st index to rebuild");
            }
            // Exit-code contract: the eprintln! above writes to stderr only.
            // Like the other arms, this falls through to the search below,
            // so the final exit code still reflects match/no-match/error
            // from the (stale) index, not this update failure.
            false
        }
        Err(_) => {
            // Other errors (e.g. Io): search stale silently.
            // Exit-code contract: same as LockConflict -- no stdout write,
            // no early return, so a failed auto-update can only ever make
            // results stale, never flip the reported exit code.
            false
        }
    }
}

/// Spawn a detached `st update --quiet` so a stale index catches up in the
/// background. Gated by `Config::auto_update_async_catchup` (itself gated by
/// `SYNTEXT_NO_ASYNC_UPDATE=1`, parsed in `cli::config`). Stdio is null and
/// the child is never waited on: the caller's exit code and timing must stay
/// unaffected by whether the spawn succeeds, fails, or is still running.
pub(super) fn maybe_spawn_async_catchup(config: &Config) {
    if !config.auto_update_async_catchup {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .arg("update")
        .arg("--quiet")
        .arg("--index-dir")
        .arg(&config.index_dir)
        .arg("--repo-root")
        .arg(&config.repo_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
