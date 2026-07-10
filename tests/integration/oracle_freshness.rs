//! Differential oracle for the bounded-update staleness invariant.
//!
//! `oracle_self`, `oracle_incremental`, and `oracle_cli` only exercise the
//! "fresh" path: they build/commit with `auto_update: false` and always
//! compare `st` against `rg` run against the *current* working tree. None of
//! them drive `Index::search_fresh` / `UpdateLimits`
//! (`src/index/freshness.rs`, `src/cli/catchup.rs`), so the bounded-update
//! contract itself -- "a stale search is safe" -- has no differential
//! coverage of its stale branch (`UpdateOutcome::BudgetExceeded` /
//! `UpdateOutcome::TooManyFiles`).
//!
//! This file encodes the "staleness invariant pair": for one mutation applied
//! to a live working tree,
//! 1. **Stale half**: `Index::search_fresh` called with a genuinely tiny
//!    `UpdateLimits` (forcing `UpdateOutcome::TooManyFiles`) must match `rg`
//!    run against the *pre-mutation* snapshot of the tree -- never a
//!    fabricated mix of old and new content, and never content that was
//!    never indexed.
//! 2. **Fresh half**: the same index, `search_fresh` called again with
//!    generous `UpdateLimits`, must match `rg` run against the live
//!    (post-mutation) tree.

#[path = "oracle_helpers.rs"]
mod oracle_helpers;

use oracle_helpers::{normalize_ndjson, rg_available, snapshot_tree, CanonicalMatch};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use syntext::index::freshness::{UpdateLimits, UpdateOutcome};
use syntext::index::Index;
use syntext::{Config, SearchOptions};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok();
}

fn init_git_repo(repo: &Path) {
    git(repo, &["init"]);
    git(repo, &["config", "user.name", "oracle"]);
    git(repo, &["config", "user.email", "oracle@example.com"]);
}

fn commit_all(repo: &Path, msg: &str) {
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", msg, "--no-gpg-sign"]);
}

/// Run `rg --json <query> .` against `dir` and return the (path, line_number)
/// key set, using the same NDJSON normalizer the other oracle targets use.
/// Line-level keys (not full `CanonicalMatch`) are sufficient here: this test
/// asserts *which lines* matched, matching the Tier A/B level of the other
/// incremental oracle assertions (`oracle_incremental.rs::assert_st_matches_rg`).
fn rg_line_keys(dir: &Path, query: &str) -> HashSet<(String, usize)> {
    let output = Command::new("rg")
        .args([
            "--json",
            "--hidden",
            "--crlf",
            "--glob",
            "!.gitignore",
            "--glob",
            "!.syntext",
            query,
            ".",
        ])
        .current_dir(dir)
        .output()
        .expect("failed to run rg");
    let matches: std::collections::BTreeSet<CanonicalMatch> =
        normalize_ndjson(&output.stdout).expect("rg NDJSON parse error");
    matches
        .into_iter()
        .map(|m| (m.path, m.line_number))
        .collect()
}

/// `st`'s `search_fresh` matches, reduced to the same (path, line_number) key
/// shape as `rg_line_keys` for a direct comparison.
fn st_line_keys(matches: &[syntext::SearchMatch]) -> HashSet<(String, usize)> {
    matches
        .iter()
        .map(|m| {
            let path = m.path.to_string_lossy().replace('\\', "/");
            (path, m.line_number as usize)
        })
        .collect()
}

/// Stale half of the staleness invariant pair: `search_fresh` called with a
/// genuinely tiny `UpdateLimits` (`max_files: Some(0)`) on a repo where a
/// brand-new file has just been created on disk (untracked, uncommitted)
/// must:
/// - return `UpdateOutcome::TooManyFiles` (the real `BudgetExceeded`/
///   `TooManyFiles` code path in `src/index/freshness.rs`, not a mock), and
/// - report exactly what `rg` finds on the *pre-mutation* snapshot of the
///   tree: zero matches for a query that only exists in the new file, since
///   that file was never indexed (the bounded update was skipped).
///
/// A brand-new file is used rather than a content edit to an existing,
/// already-indexed file because `resolve_doc` (`src/search/resolver.rs`)
/// re-reads *live* file bytes off disk for base-segment documents at query
/// time, regardless of the posting list's staleness. So a query matching
/// content that existed pre-mutation but was edited away post-mutation would
/// still report zero matches even before any bounded update runs -- that
/// tests the verifier's live-read behavior, not the staleness contract. A
/// query whose only match lives in a file the stale index never learned
/// about (because it was skipped by `TooManyFiles`) is the case that
/// actually exercises "never fabricate content the bounded update declined
/// to apply."
#[test]
fn search_fresh_too_many_files_matches_rg_on_pre_mutation_tree() {
    if !rg_available() {
        return;
    }

    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(
        repo.path().join("src/main.rs"),
        b"fn baseline_marker() {}\n",
    )
    .unwrap();
    fs::write(repo.path().join(".gitignore"), b".syntext/\n.git/\n").unwrap();
    init_git_repo(repo.path());
    commit_all(repo.path(), "initial");

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).expect("build index");

    // Snapshot the tree BEFORE the mutation, so the stale-half assertion can
    // ask "what did rg see before the change?" after the live tree has moved
    // on (see `snapshot_tree`'s doc comment for why a plain filesystem copy
    // is used instead of a second checked-out tree).
    let pre_mutation = snapshot_tree(repo.path());

    // Premise check: rg on the pre-mutation snapshot must find the
    // soon-to-be-created file's marker nowhere (the file doesn't exist yet).
    let expected_stale = rg_line_keys(pre_mutation.path(), "only_in_new_file");
    assert!(
        expected_stale.is_empty(),
        "premise check: only_in_new_file must not exist on the pre-mutation snapshot"
    );

    // Mutation: create a brand-new, untracked file on disk WITHOUT going
    // through notify_change/commit_batch. Only `search_fresh`'s own
    // `update_from_git` call should see this.
    fs::write(
        repo.path().join("src/new_file.rs"),
        b"fn only_in_new_file() {}\n",
    )
    .unwrap();

    // Premise check: rg on the LIVE (post-mutation) tree DOES find it, so the
    // zero-match assertions below prove the stale index actually skipped
    // real content, not that the query was simply never matchable.
    let live_matches = rg_line_keys(repo.path(), "only_in_new_file");
    assert_eq!(
        live_matches.len(),
        1,
        "premise check: rg must find only_in_new_file on the live (post-mutation) tree"
    );

    // A genuinely tiny UpdateLimits: max_files=0 means even the single new
    // file exceeds the cap, forcing the real TooManyFiles code path (not a
    // mock/stub) rather than budget_ms=0, which is already covered directly
    // in src/index/freshness.rs's own unit tests.
    let tiny_limits = UpdateLimits {
        max_files: Some(0),
        budget_ms: None,
    };
    let (matches, outcome) = index
        .search_fresh("only_in_new_file", &SearchOptions::default(), tiny_limits)
        .expect("search_fresh must not hard-fail on a stale index");

    match outcome {
        UpdateOutcome::TooManyFiles { files_behind, .. } => {
            assert_eq!(
                files_behind, 1,
                "exactly the one new file should be reported behind"
            );
        }
        other => panic!("expected TooManyFiles, got {other:?}"),
    }

    let actual_stale = st_line_keys(&matches);
    assert_eq!(
        actual_stale, expected_stale,
        "stale search_fresh (TooManyFiles) must match rg on the pre-mutation tree exactly \
         (zero matches): the new file's content must never be fabricated into a result"
    );

    // Baseline content untouched by the mutation must still be found under
    // the stale index, proving the TooManyFiles path didn't just black-hole
    // the whole index.
    let (baseline_matches, baseline_outcome) = index
        .search_fresh(
            "baseline_marker",
            &SearchOptions::default(),
            UpdateLimits {
                max_files: Some(0),
                budget_ms: None,
            },
        )
        .expect("search_fresh must not hard-fail on a stale index");
    assert!(matches!(
        baseline_outcome,
        UpdateOutcome::TooManyFiles { .. }
    ));
    assert_eq!(
        st_line_keys(&baseline_matches),
        rg_line_keys(pre_mutation.path(), "baseline_marker"),
        "unrelated, already-indexed content must still be found under a stale index"
    );

    // Fresh half of the pair, on the SAME index: call `search_fresh` again,
    // this time with generous `UpdateLimits` (no caps), so `update_from_git`
    // actually applies the pending change instead of bailing out again. This
    // closes the pair by proving the same index that was safely stale a
    // moment ago becomes exactly as fresh as the live tree once the bounded
    // update is allowed to run.
    let generous_limits = UpdateLimits {
        max_files: None,
        budget_ms: None,
    };
    let (fresh_matches, fresh_outcome) = index
        .search_fresh(
            "only_in_new_file",
            &SearchOptions::default(),
            generous_limits,
        )
        .expect("search_fresh must not hard-fail when applying the pending change");

    match fresh_outcome {
        UpdateOutcome::Updated { files, .. } => {
            assert_eq!(
                files, 1,
                "exactly the one new file should be applied by the generous update"
            );
        }
        other => panic!("expected Updated, got {other:?}"),
    }

    assert_eq!(
        st_line_keys(&fresh_matches),
        live_matches,
        "fresh search_fresh (Updated) must match rg on the live (post-mutation) tree exactly: \
         the newly created file's content must now be found"
    );

    drop(index);
}
