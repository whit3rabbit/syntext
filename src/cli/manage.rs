//! Management subcommand handlers: index, verify, update.
//!
//! `st status` lives in [`super::status`], split out for the 400-line gate.

use std::io::{self, Write};

use crate::index::freshness::UpdateLimits;
use crate::index::Index;
use crate::{Config, IndexError};

pub(super) fn cmd_index(mut config: Config, _force: bool, stats: bool, quiet: bool) -> i32 {
    // Index::build always rebuilds; --force is accepted for rg/ug compat.
    // --quiet suppresses library progress output; default CLI behavior is verbose.
    if quiet {
        config.verbose = false;
    } else if !config.verbose {
        // Neither --verbose nor --quiet: default to verbose for CLI users.
        config.verbose = true;
    }
    // The library logs through `log`; sync the level to this subcommand's
    // resolved verbosity (default verbose, `--quiet` off) so the build summary
    // and per-file skips print exactly as they did before the log migration.
    super::logger::set_verbose(config.verbose);
    let index = match Index::build(config) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st index: {e}");
            return 2;
        }
    };

    if stats {
        let s = index.stats();
        let stdout = io::stdout();
        let mut out = stdout.lock();
        if let Err(err) = writeln!(out, "Documents: {}", s.total_documents)
            .and_then(|_| writeln!(out, "Segments:  {}", s.total_segments))
            .and_then(|_| writeln!(out, "Grams:     {}", s.total_grams))
        {
            return handle_output(err);
        }
    }
    drop(index);
    0
}

pub(super) fn cmd_verify(mut config: Config) -> i32 {
    // Full verification at open already covers the per-segment checksums;
    // Index::verify below re-checks via the loaded snapshot so a clean exit
    // means both the open path and the resident segments agree.
    config.verify_on_open = true;
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st verify: {e}");
            return 2;
        }
    };
    let result = index.verify();
    drop(index);
    match result {
        Ok(()) => {
            let stdout = io::stdout();
            let mut out = stdout.lock();
            if let Err(err) = writeln!(out, "index OK: {}", config.index_dir.display()) {
                return handle_output(err);
            }
            0
        }
        Err(e) => {
            eprintln!("st verify: {e}");
            2
        }
    }
}

fn try_update_once(config: Config, quiet: bool) -> Result<i32, IndexError> {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        // A missing index is expected when `st update` runs from a git hook
        // (e.g. post-checkout) before the repo has ever been indexed. Under
        // --quiet (the documented hook-safe mode), exit 0 with no stderr so
        // hooks don't spam or fail; otherwise propagate the error.
        Err(IndexError::IndexNotFound(_)) if quiet => {
            return Ok(0);
        }
        Err(e) => return Err(e),
    };

    // A moved HEAD (commit, checkout, merge, rebase/rewrite -- exactly the
    // events post-commit/post-checkout/post-merge/post-rewrite hooks fire
    // on) leaves the working tree clean and matching the new HEAD, so none
    // `update_from_git`'s `git status` detection sees nothing: it only
    // detects *uncommitted* drift.
    // Check base_commit staleness first and do a full rebuild when it
    // fired, so a hook-triggered `st update` actually picks up newly
    // committed content instead of silently no-op'ing.
    // Whether a durable committed-HEAD delta was applied. A delta advances and
    // persists base_commit before we fall through to the uncommitted-drift
    // detection below, so the primary work is already done and durable: the
    // trailing update_from_git pass must not contradict or override it.
    let mut delta_applied = false;
    match index.rebuild_if_stale() {
        Ok(Some((stats, full))) => {
            if !quiet {
                let stdout = io::stdout();
                let mut out = stdout.lock();
                let msg = if full {
                    format!(
                        "st: rebuilt index ({} document(s), HEAD changed)",
                        stats.total_documents
                    )
                } else {
                    format!(
                        "st: applied delta update ({} document(s), HEAD changed)",
                        stats.total_documents
                    )
                };
                if let Err(err) = writeln!(out, "{}", msg) {
                    return Ok(handle_output(err));
                }
            }
            if full {
                drop(index);
                return Ok(0);
            }
            delta_applied = true;
        }
        Ok(None) => {}
        Err(e) => {
            drop(index);
            return Err(e);
        }
    }

    // CLI update has no limits: process all changed files with no time budget.
    let limits = UpdateLimits {
        max_files: None,
        budget_ms: None,
    };

    match index.update_from_git(limits) {
        Ok(crate::index::freshness::UpdateOutcome::Updated { files, skipped, .. }) => {
            // `st update` always persists. Before this, an uncommitted-drift
            // update applied to this process's overlay and exited, so the next
            // `st search` (a fresh process, empty overlay) re-detected the same
            // files and searched stale. A flush failure is reported but does
            // not fail the command: the in-memory update still happened, and
            // the next update retries the flush.
            let flushed = match index.flush_overlay() {
                Ok(flushed) => flushed,
                Err(e) => {
                    eprintln!("st update: applied {files} file(s) but could not persist them: {e}");
                    false
                }
            };
            if !quiet {
                let stdout = io::stdout();
                let mut out = stdout.lock();
                let msg = if flushed {
                    format!("st: updated and flushed {files} file(s)")
                } else {
                    format!("st: updated {files} file(s)")
                };
                if let Err(err) = writeln!(out, "{msg}") {
                    return Ok(handle_output(err));
                }
            }
            // Surface partial updates: files git reported as changed but that
            // could not be applied (escaped the repo, broken symlink, notify
            // error). Exit 1 (matching the pre-rewrite contract) so scripts can
            // detect a partial update. Run `st update --verbose` (Config.verbose)
            // for per-file skip reasons.
            if skipped > 0 {
                eprintln!("st update: {skipped} file(s) skipped (run with verbose for details)");
            }
            drop(index);
            if skipped > 0 {
                Ok(1)
            } else {
                Ok(0)
            }
        }
        Ok(_) => {
            // NoChanges, BudgetExceeded, TooManyFiles — none apply to CLI
            // update (no budget, no max_files). Treat as no-changes. Suppress
            // the "no changes detected" line when a delta already reported an
            // update: the committed HEAD move was applied, so "no changes"
            // would contradict the message just printed.
            if !quiet && !delta_applied {
                let stdout = io::stdout();
                let mut out = stdout.lock();
                if let Err(err) = writeln!(out, "st: no changes detected") {
                    return Ok(handle_output(err));
                }
            }
            Ok(0)
        }
        Err(e) => {
            drop(index);
            // If a durable delta was already applied, the uncommitted-drift
            // pass failing is non-fatal: the committed HEAD update succeeded
            // and is persisted. Warn but report success rather than masking the
            // durable update behind a total-failure exit code.
            if delta_applied {
                eprintln!("st update: delta applied, but uncommitted-change scan failed: {e}");
                Ok(0)
            } else {
                Err(e)
            }
        }
    }
}

/// `--flush` is accepted for compatibility and does nothing: `st update`
/// always persists what it applies.
pub(super) fn cmd_update(config: Config, _flush: bool, quiet: bool) -> i32 {
    let mut attempt = 0;
    let base_delay = std::time::Duration::from_millis(50);
    loop {
        match try_update_once(config.clone(), quiet) {
            Ok(code) => return code,
            Err(IndexError::LockConflict(_)) if attempt < 5 => {
                attempt += 1;
                let delay = base_delay * (1 << (attempt - 1));
                if config.verbose && !quiet {
                    eprintln!(
                        "st update: lock conflict, retrying in {}ms...",
                        delay.as_millis()
                    );
                }
                std::thread::sleep(delay);
            }
            Err(e) => {
                eprintln!("st update: {e}");
                return 2;
            }
        }
    }
}

pub(super) fn handle_output(err: io::Error) -> i32 {
    if err.kind() == io::ErrorKind::BrokenPipe {
        0
    } else {
        eprintln!("st: {err}");
        2
    }
}
