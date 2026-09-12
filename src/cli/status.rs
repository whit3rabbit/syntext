//! The `st status` subcommand.
//!
//! Split from [`super::manage`] to keep that file under the 400-line quality
//! gate. Behaviour is unchanged by the move.

use std::io::{self, Write};

use crate::index::freshness;
use crate::index::Index;
use crate::Config;

use super::manage::handle_output;

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
    let (behind, base_stale_msg) = base_commit_lag(&config, s.base_commit.as_deref());

    // Which checkout this index belongs to. Plain file reads (`.git`, `HEAD`),
    // never a subprocess: `freshness` budgets exactly one `git status` spawn per
    // detection and two tests in tests/integration/cli.rs assert that count.
    let checkout = crate::git_checkout::identify_checkout(&config.repo_root);

    let code = if json {
        // Use serde_json to avoid malformed output when index_dir contains
        // characters that need JSON escaping (quotes, backslashes, etc.).
        let obj = serde_json::json!({
            "documents": s.total_documents,
            "segments": s.total_segments,
            "grams": s.total_grams,
            "index_dir": config.index_dir.display().to_string(),
            "files_behind": files_behind,
            "base_behind_commits": behind,
            "worktree_kind": checkout.as_ref().map(|c| c.kind.as_str()),
            "worktree_name": checkout.as_ref().and_then(|c| c.name.clone()),
            "branch": checkout.as_ref().and_then(|c| c.branch.clone()),
            "detached_head": checkout.as_ref().and_then(|c| c.head.clone()),
        });
        let stdout = io::stdout();
        let mut out = stdout.lock();
        writeln!(out, "{obj}").err().map(handle_output)
    } else {
        write_text_status(
            &config,
            &s,
            files_behind,
            &base_stale_msg,
            checkout.as_ref(),
        )
    };

    drop(index);
    code.unwrap_or(0)
}

/// Render the human-readable status block. Returns `Some(exit_code)` only when
/// writing to stdout failed.
fn write_text_status(
    config: &Config,
    s: &crate::IndexStats,
    files_behind: Option<usize>,
    base_stale_msg: &Option<String>,
    checkout: Option<&crate::git_checkout::CheckoutIdentity>,
) -> Option<i32> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let files_behind_display = match (files_behind, base_stale_msg) {
        (Some(fb), Some(msg)) => format!("{fb} ({msg})"),
        (Some(fb), None) => fb.to_string(),
        (None, Some(msg)) => format!("unknown ({msg})"),
        (None, None) => "unknown".to_string(),
    };
    let result = writeln!(out, "Index:     {}", config.index_dir.display())
        .and_then(|_| writeln!(out, "Documents: {}", s.total_documents))
        .and_then(|_| writeln!(out, "Segments:  {}", s.total_segments))
        .and_then(|_| writeln!(out, "Grams:     {}", s.total_grams))
        .and_then(|_| writeln!(out, "Behind:    {files_behind_display}"))
        .and_then(|_| match s.base_commit {
            Some(ref commit) => writeln!(out, "Commit:    {commit}"),
            None => Ok(()),
        })
        .and_then(|_| write_checkout(&mut out, checkout));
    result.err().map(handle_output)
}

/// Append the worktree/branch lines. Both are omitted when there is nothing to
/// say: a plain checkout with an unreadable HEAD would otherwise add noise.
fn write_checkout(
    out: &mut impl Write,
    checkout: Option<&crate::git_checkout::CheckoutIdentity>,
) -> io::Result<()> {
    let Some(c) = checkout else {
        return Ok(());
    };
    if let Some(ref name) = c.name {
        writeln!(out, "Worktree:  {name} ({})", c.kind.as_str())?;
    }
    match (&c.branch, &c.head) {
        (Some(b), _) => writeln!(out, "Branch:    {b}"),
        (None, Some(sha)) => writeln!(out, "Branch:    {sha} (detached)"),
        (None, None) => Ok(()),
    }
}

/// How many commits HEAD is ahead of the index's base commit, plus a message
/// explaining an unusable base. `(None, None)` when there is no base commit.
fn base_commit_lag(config: &Config, base: Option<&str>) -> (Option<usize>, Option<String>) {
    let Some(base) = base else {
        return (None, None);
    };
    if !crate::git_util::is_hex_commit(base) {
        return (
            None,
            Some("stale base, invalid base commit hash in manifest".to_string()),
        );
    }
    let git = crate::git_util::resolve_git_binary();
    let canonical_root =
        std::fs::canonicalize(&config.repo_root).unwrap_or_else(|_| config.repo_root.clone());
    let Ok(output) = std::process::Command::new(&git)
        .arg("-C")
        .arg(&canonical_root)
        .args([
            "rev-list",
            "--count",
            "--end-of-options",
            &format!("{base}..HEAD"),
        ])
        .output()
    else {
        return (None, None);
    };
    if !output.status.success() {
        // rev-list only fails when `base` is not a resolvable ref (gc'd,
        // shallow clone, or repo_root is no longer a git repo). A merely
        // non-ancestor HEAD still succeeds, so do not claim "non-ancestor".
        return (
            None,
            Some("stale base, base commit not found (cannot compare to HEAD)".to_string()),
        );
    }
    let n = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<usize>()
        .unwrap_or(0);
    let msg = (n > 0).then(|| format!("stale base, behind HEAD by {n} commit(s)"));
    (Some(n), msg)
}

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
        Ok(mut change_set) => {
            // Discount paths a durable flush already made permanent, so
            // `st update` followed by `st status` reports 0 instead of the
            // same uncommitted files forever. A budget-exceeded run reports
            // its partial estimate unfiltered: detection stopped early, so
            // the set is not the real change set to filter against.
            match change_set.budget_exceeded {
                Some(behind) => Some(behind),
                None => {
                    index.retain_unflushed(&mut change_set.paths);
                    Some(change_set.paths.len())
                }
            }
        }
        Err(_) => None,
    }
}
