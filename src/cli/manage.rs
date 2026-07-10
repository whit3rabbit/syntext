//! Management subcommand handlers: index, status, update.

use std::io::{self, Write};

use crate::index::freshness::{self, UpdateLimits};
use crate::index::Index;
use crate::{Config, IndexError};

/// Detect how many files the index is behind the working tree, bounded by
/// `config.auto_update_budget_ms`. Read-only: unlike `update_from_git`, this
/// never applies changes to the overlay.
///
/// Returns a lower-bound count and `None` on any detection failure (no git
/// binary, non-git directory, or a spawn error) so callers can report
/// `files_behind` as unknown/0 without erroring the command. When the time
/// budget is exhausted mid-detection, the returned count is a partial
/// (lower-bound) estimate, matching `UpdateOutcome::BudgetExceeded` semantics.
fn detect_files_behind(index: &Index, config: &Config) -> Option<usize> {
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return None;
    }
    match freshness::detect_changed_files(
        &index.canonical_root,
        &git,
        Some(config.auto_update_budget_ms),
    ) {
        Ok(change_set) => Some(change_set.budget_exceeded.unwrap_or(change_set.paths.len())),
        Err(_) => None,
    }
}

pub(super) fn cmd_index(mut config: Config, _force: bool, stats: bool, quiet: bool) -> i32 {
    // Index::build always rebuilds; --force is accepted for rg/ug compat.
    // --quiet suppresses library progress output; default CLI behavior is verbose.
    if quiet {
        config.verbose = false;
    } else if !config.verbose {
        // Neither --verbose nor --quiet: default to verbose for CLI users.
        config.verbose = true;
    }
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

pub(super) fn cmd_status(config: Config, json: bool) -> i32 {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st status: {e}");
            return 2;
        }
    };

    let s = index.stats();
    // Bounded by config.auto_update_budget_ms; None means detection failed
    // (no git binary, non-git directory) and is reported as unknown/null.
    let files_behind = detect_files_behind(&index, &config);

    let git = crate::git_util::resolve_git_binary();
    let mut base_stale_msg = None;
    let behind = if let Some(ref base) = s.base_commit {
        let canonical_root = std::fs::canonicalize(&config.repo_root).unwrap_or_else(|_| config.repo_root.clone());
        if let Ok(output) = std::process::Command::new(&git)
            .arg("-C")
            .arg(&canonical_root)
            .args(["rev-list", "--count", &format!("{base}..HEAD")])
            .output()
        {
            if output.status.success() {
                let n = String::from_utf8_lossy(&output.stdout).trim().parse::<usize>().unwrap_or(0);
                if n > 0 {
                    base_stale_msg = Some(format!("stale base, behind HEAD by {n} commit(s)"));
                }
                Some(n)
            } else {
                // rev-list only fails when `base` is not a resolvable ref
                // (gc'd, shallow clone, or repo_root is no longer a git repo).
                // A merely non-ancestor HEAD still succeeds, so do not claim
                // "non-ancestor" here.
                base_stale_msg =
                    Some("stale base, base commit not found (cannot compare to HEAD)".to_string());
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    if json {
        // Use serde_json to avoid malformed output when index_dir contains
        // characters that need JSON escaping (quotes, backslashes, etc.).
        let obj = serde_json::json!({
            "documents": s.total_documents,
            "segments": s.total_segments,
            "grams": s.total_grams,
            "index_dir": config.index_dir.display().to_string(),
            "files_behind": files_behind,
            "base_behind_commits": behind,
        });
        let stdout = io::stdout();
        let mut out = stdout.lock();
        if let Err(err) = writeln!(out, "{obj}") {
            return handle_output(err);
        }
    } else {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let files_behind_display = match (files_behind, &base_stale_msg) {
            (Some(fb), Some(msg)) => format!("{fb} ({msg})"),
            (Some(fb), None) => fb.to_string(),
            (None, Some(msg)) => format!("unknown ({msg})"),
            (None, None) => "unknown".to_string(),
        };
        if let Err(err) = writeln!(out, "Index:     {}", config.index_dir.display())
            .and_then(|_| writeln!(out, "Documents: {}", s.total_documents))
            .and_then(|_| writeln!(out, "Segments:  {}", s.total_segments))
            .and_then(|_| writeln!(out, "Grams:     {}", s.total_grams))
            .and_then(|_| writeln!(out, "Behind:    {files_behind_display}"))
        {
            return handle_output(err);
        }
        if let Some(ref commit) = s.base_commit {
            if let Err(err) = writeln!(out, "Commit:    {commit}") {
                return handle_output(err);
            }
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
    // of `update_from_git`'s three git commands (diff HEAD, diff --cached,
    // ls-files --others) see anything: they only detect *uncommitted* drift.
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
                    format!("st: rebuilt index ({} document(s), HEAD changed)", stats.total_documents)
                } else {
                    format!("st: applied delta update ({} document(s), HEAD changed)", stats.total_documents)
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
            if !quiet {
                let stdout = io::stdout();
                let mut out = stdout.lock();
                if let Err(err) = writeln!(out, "st: updated {} file(s)", files) {
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
                    eprintln!("st update: lock conflict, retrying in {}ms...", delay.as_millis());
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

fn handle_output(err: io::Error) -> i32 {
    if err.kind() == io::ErrorKind::BrokenPipe {
        0
    } else {
        eprintln!("st: {err}");
        2
    }
}

/// Print supported file types in ripgrep-compatible format.
pub(super) fn cmd_type_list() -> i32 {
    use ignore::types::TypesBuilder;
    let mut builder = TypesBuilder::new();
    builder.add_defaults();
    let mut entries: Vec<(String, Vec<String>)> = Vec::new();
    for def in builder.definitions() {
        let globs: Vec<String> = def.globs().iter().map(|g| g.to_string()).collect();
        entries.push((def.name().to_string(), globs));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for (name, globs) in &entries {
        let joined = globs.join(", ");
        if writeln!(out, "{name}: {joined}").is_err() {
            return 0; // broken pipe
        }
    }
    0
}
