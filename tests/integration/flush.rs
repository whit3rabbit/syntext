//! Integration tests for durable flush of uncommitted working-tree drift.
//!
//! The thing under test is cross-process visibility. Before `flush_overlay`,
//! `st update` applied uncommitted drift to its own in-memory overlay and
//! exited, so a later `st search` (a fresh process with an empty overlay)
//! re-detected the same files and searched stale. Every assertion below that
//! matters therefore goes through `drop(index)` + `Index::open`: the overlay is
//! empty on reopen, so anything still visible came off disk.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use syntext::index::manifest::Manifest;
use syntext::index::{Index, UpdateLimits, UpdateOutcome};
use syntext::{Config, IndexError, SearchOptions};

/// A git repo whose committed content is indexed, ready for uncommitted edits.
struct Fixture {
    repo: tempfile::TempDir,
    index_dir: tempfile::TempDir,
    index: Index,
}

impl Fixture {
    fn config(&self) -> Config {
        Config {
            index_dir: self.index_dir.path().to_path_buf(),
            repo_root: self.repo.path().to_path_buf(),
            ..Config::default()
        }
    }

    fn git(&self, args: &[&str]) {
        run_git(self.repo.path(), args);
    }

    fn manifest(&self) -> Manifest {
        Manifest::load(self.index_dir.path()).expect("manifest")
    }

    /// Drop this handle and open a fresh one from disk. A fresh handle has an
    /// empty overlay, so whatever it can still find is durable.
    fn reopen(self) -> Fixture {
        let config = self.config();
        let Fixture {
            repo,
            index_dir,
            index,
        } = self;
        drop(index);
        let index = retry_lock(|| Index::open(config.clone())).expect("reopen");
        Fixture {
            repo,
            index_dir,
            index,
        }
    }

    fn count(&self, pattern: &str) -> usize {
        self.index
            .search(pattern, &SearchOptions::default())
            .expect("search")
            .len()
    }

    /// Write `rel` and age its mtime so the anchor's racy-mtime rule treats it
    /// as settled. Without the ageing, a just-written file is deliberately left
    /// out of the anchor and every test about skipping re-applies would be
    /// testing nothing.
    fn write_aged(&self, rel: &str, contents: &str) {
        write_aged(self.repo.path(), rel, contents);
    }

    /// Flush, retrying a `LockConflict` with bounded backoff.
    ///
    /// Not papering over contention. `helpers::classify_try_lock` maps any
    /// non-`WouldBlock` `flock(2)` failure onto `LockConflict` too, and on
    /// macOS a parallel test binary generates enough process churn to exhaust
    /// the kernel lock table (`ENOLCK`) on a private temp dir no other process
    /// has ever opened. `cmd_update` retries for exactly this reason, and so do
    /// the oracle harnesses (see CLAUDE.md). Single-threaded these tests are
    /// green without it.
    fn flush(&self) -> bool {
        retry_lock(|| self.index.flush_overlay()).expect("flush")
    }

    fn update_all(&self) -> UpdateOutcome {
        self.index
            .update_from_git(UpdateLimits {
                max_files: None,
                budget_ms: None,
            })
            .expect("update")
    }
}

/// Retry a `LockConflict` with exponential backoff. Any other error, and any
/// success, returns immediately.
fn retry_lock<T>(mut op: impl FnMut() -> Result<T, IndexError>) -> Result<T, IndexError> {
    let mut delay = Duration::from_millis(20);
    for _ in 0..6 {
        match op() {
            Err(IndexError::LockConflict(_)) => {
                std::thread::sleep(delay);
                delay *= 2;
            }
            other => return other,
        }
    }
    op()
}

fn run_git(repo: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
}

fn write_aged(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, contents).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(10))
        .unwrap();
}

fn setup() -> Fixture {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();
    run_git(repo.path(), &["init"]);
    run_git(repo.path(), &["config", "user.name", "test"]);
    run_git(repo.path(), &["config", "user.email", "test@test"]);
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(
        repo.path().join("src/main.rs"),
        "fn committed_marker() { let shared_token = 1; }\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("src/other.rs"),
        "fn untouched_marker() {}\n",
    )
    .unwrap();
    run_git(repo.path(), &["add", "-A"]);
    run_git(repo.path(), &["commit", "-m", "initial", "--no-gpg-sign"]);

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = retry_lock(|| Index::build(config.clone())).expect("build");
    Fixture {
        repo,
        index_dir,
        index,
    }
}

#[test]
fn an_uncommitted_edit_survives_a_reopen_and_leaves_no_duplicate() {
    let fx = setup();
    fx.write_aged(
        "src/main.rs",
        "fn drift_marker() { let shared_token = 1; }\n",
    );

    assert!(matches!(fx.update_all(), UpdateOutcome::Updated { .. }));
    assert!(fx.flush());

    let fx = fx.reopen();
    assert_eq!(
        fx.count("drift_marker"),
        1,
        "the uncommitted edit must be visible to a fresh process"
    );
    assert_eq!(
        fx.count("committed_marker"),
        0,
        "the superseded base doc must be hidden by the delete-set"
    );
    assert_eq!(
        fx.count("shared_token"),
        1,
        "one match, not a stale-base plus delta duplicate"
    );
}

#[test]
fn an_uncommitted_add_and_delete_both_survive_a_reopen() {
    let fx = setup();
    fx.write_aged("src/added.rs", "fn added_marker() {}\n");
    fs::remove_file(fx.repo.path().join("src/other.rs")).unwrap();

    fx.update_all();
    assert!(fx.flush());

    let fx = fx.reopen();
    assert_eq!(fx.count("added_marker"), 1);
    assert_eq!(fx.count("untouched_marker"), 0, "the deletion is durable");
}

#[test]
fn a_flush_appends_a_segment_and_an_anchor_without_touching_base_commit() {
    let fx = setup();
    let before = fx.manifest();
    fx.write_aged("src/main.rs", "fn drift_marker() {}\n");
    fx.update_all();
    assert!(fx.flush());

    let after = fx.manifest();
    assert_eq!(after.segments.len(), before.segments.len() + 1);
    assert_eq!(
        after.base_commit, before.base_commit,
        "uncommitted drift belongs to no commit; advancing base_commit would \
         make rebuild_if_stale skip the real one"
    );
    assert!(after.worktree_anchor_file.is_some());
    assert_eq!(after.overlay_gen, before.overlay_gen + 1);
    assert!(after.overlay_deletes_file.is_some());
}

#[test]
fn a_flushed_path_stops_being_re_detected_until_it_changes_again() {
    let fx = setup();
    fx.write_aged("src/main.rs", "fn drift_marker() {}\n");
    fx.update_all();
    fx.flush();

    // A limit of 1 file would trip TooManyFiles if the flushed path were still
    // counted, so NoChanges is the assertion that the anchor is doing its job.
    let bounded = fx
        .index
        .update_from_git(UpdateLimits {
            max_files: Some(1),
            budget_ms: None,
        })
        .expect("update");
    assert!(
        matches!(bounded, UpdateOutcome::NoChanges { .. }),
        "a flushed, unchanged path must not be re-applied, got {bounded:?}"
    );

    // Editing it again brings it back.
    fx.write_aged("src/main.rs", "fn drift_marker_two() {}\n");
    assert!(matches!(fx.update_all(), UpdateOutcome::Updated { .. }));
}

#[test]
fn a_flushed_deletion_stops_being_re_detected() {
    let fx = setup();
    fs::remove_file(fx.repo.path().join("src/other.rs")).unwrap();
    fx.update_all();
    fx.flush();

    assert!(matches!(fx.update_all(), UpdateOutcome::NoChanges { .. }));
}

#[test]
fn a_racily_recent_edit_is_re_applied_rather_than_lost() {
    // Not aged: written in the same tick as the read, so the anchor refuses to
    // trust it. The safe direction is to re-apply, never to skip.
    let fx = setup();
    fs::write(
        fx.repo.path().join("src/main.rs"),
        "fn racy_marker() {}\n",
    )
    .unwrap();
    fx.update_all();
    fx.flush();

    assert!(
        matches!(fx.update_all(), UpdateOutcome::Updated { .. }),
        "a racily-timed edit must be re-applied on the next pass"
    );

    let fx = fx.reopen();
    assert_eq!(fx.count("racy_marker"), 1, "still exactly one match");
}

#[test]
fn a_corrupt_anchor_costs_re_applies_not_correctness() {
    // The anchor fails OPEN, unlike the delete-set sidecar. Losing it means
    // paths get re-applied, which `compute_delete_set` makes idempotent.
    let fx = setup();
    fx.write_aged("src/main.rs", "fn drift_marker() {}\n");
    fx.update_all();
    fx.flush();

    let name = fx.manifest().worktree_anchor_file.expect("anchor written");
    fs::write(fx.index_dir.path().join(&name), b"garbage").unwrap();

    let fx = fx.reopen();
    assert_eq!(
        fx.count("drift_marker"),
        1,
        "the index still opens and still answers correctly"
    );
    assert!(
        matches!(fx.update_all(), UpdateOutcome::Updated { .. }),
        "with no usable anchor the path is simply re-applied"
    );
    assert_eq!(fx.count("drift_marker"), 1, "and still no duplicate");
}

#[test]
fn flushing_then_committing_leaves_no_duplicates() {
    let fx = setup();
    fx.write_aged(
        "src/main.rs",
        "fn drift_marker() { let shared_token = 1; }\n",
    );
    fx.update_all();
    fx.flush();

    fx.git(&["add", "-A"]);
    fx.git(&["commit", "-m", "land the drift", "--no-gpg-sign"]);

    // base_commit was deliberately not advanced by the flush, so the real
    // commit is still seen as a HEAD move and gets applied.
    let moved = retry_lock(|| fx.index.rebuild_if_stale()).expect("rebuild_if_stale");
    assert!(moved.is_some(), "the commit must still register as a move");

    let fx = fx.reopen();
    assert_eq!(fx.count("drift_marker"), 1);
    assert_eq!(fx.count("shared_token"), 1);
}

#[test]
fn a_no_op_flush_reports_that_it_did_nothing() {
    let fx = setup();
    assert!(!fx.flush());
    let before = fx.manifest();
    assert!(!fx.flush());
    assert_eq!(fx.manifest().segments.len(), before.segments.len());
}

#[test]
fn repeated_flushes_stay_under_the_segment_cap() {
    let fx = setup();
    let max_segments = fx.config().max_segments;

    for i in 0..(max_segments + 3) {
        fx.write_aged("src/main.rs", &format!("fn drift_marker_{i}() {{}}\n"));
        fx.update_all();
        fx.flush();
    }

    assert!(
        fx.manifest().segments.len() <= max_segments,
        "compaction must bound segment growth, got {}",
        fx.manifest().segments.len()
    );

    let fx = fx.reopen();
    assert_eq!(fx.count("drift_marker_0"), 0, "old content is gone");
    assert_eq!(
        fx.count(&format!("drift_marker_{}", max_segments + 2)),
        1,
        "the newest content survived compaction"
    );
}

#[test]
fn compaction_carries_the_anchor_and_the_base_commit_forward() {
    let fx = setup();
    fx.write_aged("src/main.rs", "fn drift_marker() {}\n");
    fx.update_all();
    fx.flush();

    let before = fx.manifest();
    retry_lock(|| fx.index.compact()).expect("compact");
    let after = fx.manifest();

    assert_eq!(
        after.base_commit, before.base_commit,
        "compaction indexes nothing new, so it has no claim on any commit"
    );
    assert_eq!(
        after.worktree_anchor_file, before.worktree_anchor_file,
        "the same paths are still indexed with the same content"
    );

    // And the anchor is still honored after the rewrite.
    assert!(matches!(fx.update_all(), UpdateOutcome::NoChanges { .. }));
}

#[test]
fn a_flush_under_a_competing_handle_is_a_retryable_lock_conflict() {
    let fx = setup();
    fx.write_aged("src/main.rs", "fn drift_marker() {}\n");
    fx.update_all();

    let competing = retry_lock(|| Index::open(fx.config())).expect("second handle");
    let conflict = fx.index.flush_overlay();
    assert!(
        matches!(conflict, Err(IndexError::LockConflict(_))),
        "expected LockConflict, got {conflict:?}"
    );

    drop(competing);
    assert!(
        fx.flush(),
        "the overlay is intact, so the retry succeeds"
    );

    let fx = fx.reopen();
    assert_eq!(fx.count("drift_marker"), 1);
}

#[test]
fn an_anchor_covers_only_the_paths_it_recorded() {
    // Guards the shape `retain_unflushed` relies on: an untouched path is never
    // anchored, so it is still detected the first time it changes.
    let fx = setup();
    fx.write_aged("src/main.rs", "fn drift_marker() {}\n");
    fx.update_all();
    fx.flush();

    fx.write_aged("src/other.rs", "fn sibling_marker() {}\n");
    let outcome = fx.update_all();
    assert!(
        matches!(outcome, UpdateOutcome::Updated { files: 1, .. }),
        "exactly the newly-changed file, got {outcome:?}"
    );

    fx.flush();
    let fx = fx.reopen();
    assert_eq!(fx.count("drift_marker"), 1);
    assert_eq!(fx.count("sibling_marker"), 1);
}
