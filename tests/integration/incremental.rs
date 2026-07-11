//! Integration tests for incremental index updates (T044).
//!
//! Build index -> modify file -> commit_batch -> search for new content.
//! Verifies read-your-writes freshness, delete visibility, and
//! interleaved edit+search consistency.

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use syntext::index::Index;
use syntext::{Config, IndexError, SearchOptions};

/// Create a temp directory with some source files, build an index, return both.
fn setup() -> (tempfile::TempDir, tempfile::TempDir, Index) {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    // Create initial files
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(
        repo.path().join("src/main.rs"),
        "fn parse_query() { println!(\"hello\"); }\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn process_batch() { /* batch processing */ }\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("src/util.rs"),
        "fn helper() { let x = 42; }\n",
    )
    .unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).expect("build");
    (repo, index_dir, index)
}

fn search(index: &Index, pattern: &str) -> Vec<(String, u32)> {
    let opts = SearchOptions::default();
    index
        .search(pattern, &opts)
        .unwrap()
        .into_iter()
        .map(|m| (m.path.to_string_lossy().into_owned(), m.line_number))
        .collect()
}

fn commit_batch_with_retry(index: &Index) {
    const MAX_ATTEMPTS: usize = 5;
    const RETRY_DELAY: Duration = Duration::from_millis(10);

    for attempt in 1..=MAX_ATTEMPTS {
        match index.commit_batch() {
            Ok(()) => return,
            Err(IndexError::LockConflict(_)) if attempt < MAX_ATTEMPTS => {
                thread::sleep(RETRY_DELAY);
            }
            Err(err) => panic!("commit_batch failed on attempt {attempt}: {err}"),
        }
    }
}

/// Like `commit_batch_with_retry`, but returns the result instead of
/// panicking on non-LockConflict errors. Used by tests that assert on
/// specific error variants.
fn commit_batch_result(index: &Index) -> Result<(), IndexError> {
    const MAX_ATTEMPTS: usize = 5;
    const RETRY_DELAY: Duration = Duration::from_millis(10);

    for attempt in 1..=MAX_ATTEMPTS {
        match index.commit_batch() {
            Err(IndexError::LockConflict(_)) if attempt < MAX_ATTEMPTS => {
                thread::sleep(RETRY_DELAY);
            }
            other => return other,
        }
    }
    unreachable!()
}

// ---------------------------------------------------------------------------
// T044: Build -> modify -> commit_batch -> search
// ---------------------------------------------------------------------------

/// After modifying a file and committing, new content is searchable.
#[test]
fn modify_file_new_content_found() {
    let (repo, _idx, index) = setup();

    // Verify original content is found
    let results = search(&index, "parse_query");
    assert!(!results.is_empty(), "parse_query should be found initially");

    // Modify the file: replace parse_query with transform_data
    let main_path = repo.path().join("src/main.rs");
    fs::write(
        &main_path,
        "fn transform_data() { println!(\"changed\"); }\n",
    )
    .unwrap();

    // Commit the change
    index.notify_change(&main_path).unwrap();
    commit_batch_with_retry(&index);

    // After commit: new content is visible
    let results = search(&index, "transform_data");
    assert!(
        !results.is_empty(),
        "transform_data should be visible after commit"
    );
    drop(index);
}

/// After modifying a file, old content from that file is no longer found.
#[test]
fn modify_file_old_content_gone() {
    let (repo, _idx, index) = setup();

    // Verify original content
    let results = search(&index, "parse_query");
    assert!(!results.is_empty());

    // Modify: remove parse_query
    let main_path = repo.path().join("src/main.rs");
    fs::write(&main_path, "fn completely_different() {}\n").unwrap();

    index.notify_change(&main_path).unwrap();
    commit_batch_with_retry(&index);

    // parse_query should no longer appear in src/main.rs results
    let results = search(&index, "parse_query");
    let main_results: Vec<_> = results.iter().filter(|(p, _)| p == "src/main.rs").collect();
    assert!(
        main_results.is_empty(),
        "parse_query should not be in modified file, got {:?}",
        main_results
    );
    drop(index);
}

/// Deleting a file removes it from search results.
#[test]
fn delete_file_removes_from_results() {
    let (repo, _idx, index) = setup();

    // Verify file is searchable
    let results = search(&index, "process_batch");
    assert!(!results.is_empty());

    // Delete the file
    let lib_path = repo.path().join("src/lib.rs");
    fs::remove_file(&lib_path).unwrap();

    index.notify_delete(&lib_path).unwrap();
    commit_batch_with_retry(&index);

    // Should no longer find results from deleted file
    let results = search(&index, "process_batch");
    let lib_results: Vec<_> = results.iter().filter(|(p, _)| p == "src/lib.rs").collect();
    assert!(
        lib_results.is_empty(),
        "deleted file should not appear in results"
    );
    drop(index);
}

/// notify_change_immediate is equivalent to notify_change + commit_batch.
#[test]
fn notify_change_immediate_works() {
    let (repo, _idx, index) = setup();

    let main_path = repo.path().join("src/main.rs");
    fs::write(&main_path, "fn immediate_test() {}\n").unwrap();

    index.notify_change_immediate(&main_path).unwrap();

    let results = search(&index, "immediate_test");
    assert!(
        !results.is_empty(),
        "immediate update should be visible right away"
    );
    drop(index);
}

/// Pending edits for new files are invisible before commit_batch.
///
/// New files only become searchable after commit_batch creates overlay
/// doc_ids. Base files modified on disk may be seen by the verifier
/// (which reads current content), but NEW files have no base doc_id
/// and no overlay doc_id until committed.
#[test]
fn pending_new_file_invisible_before_commit() {
    let (repo, _idx, index) = setup();

    let new_path = repo.path().join("src/pending_module.rs");
    fs::write(&new_path, "fn pending_content_xyz() {}\n").unwrap();

    index.notify_change(&new_path).unwrap();
    // Do NOT call commit_batch

    let results = search(&index, "pending_content_xyz");
    assert!(
        results.is_empty(),
        "new file content must be invisible before commit"
    );

    // After commit, it becomes visible
    commit_batch_with_retry(&index);
    let results = search(&index, "pending_content_xyz");
    assert!(
        !results.is_empty(),
        "new file should be visible after commit"
    );
    drop(index);
}

#[test]
fn empty_commit_batch_is_noop() {
    let (_repo, _idx, index) = setup();

    commit_batch_with_retry(&index);
    commit_batch_with_retry(&index);

    assert!(!search(&index, "parse_query").is_empty());
    assert!(!search(&index, "process_batch").is_empty());
    drop(index);
}

#[test]
fn path_index_tracks_incremental_visible_paths() {
    let (repo, _idx, index) = setup();

    let new_path = repo.path().join("src/new_module.rs");
    fs::write(&new_path, "fn brand_new_function() { 42 }\n").unwrap();
    index.notify_change(&new_path).unwrap();
    commit_batch_with_retry(&index);

    let snap = index.snapshot();
    assert!(snap
        .path_index
        .paths
        .iter()
        .any(|p| p.as_ref() == std::path::Path::new("src/new_module.rs")));

    let deleted_path = repo.path().join("src/lib.rs");
    fs::remove_file(&deleted_path).unwrap();
    index.notify_delete(&deleted_path).unwrap();
    commit_batch_with_retry(&index);

    let snap = index.snapshot();
    assert!(!snap
        .path_index
        .paths
        .iter()
        .any(|p| p.as_ref() == std::path::Path::new("src/lib.rs")));
    drop(index);
}

/// Adding a new file makes it searchable after commit.
#[test]
fn add_new_file() {
    let (repo, _idx, index) = setup();

    let new_path = repo.path().join("src/new_module.rs");
    fs::write(&new_path, "fn brand_new_function() { 42 }\n").unwrap();

    index.notify_change(&new_path).unwrap();
    commit_batch_with_retry(&index);

    let results = search(&index, "brand_new_function");
    assert!(!results.is_empty(), "newly added file should be searchable");
    drop(index);
}

/// Interleaved edits and searches maintain consistency.
#[test]
fn interleaved_edit_search() {
    let (repo, _idx, index) = setup();

    let main_path = repo.path().join("src/main.rs");

    // Edit 1: change content
    fs::write(&main_path, "fn first_edit() {}\n").unwrap();
    index.notify_change(&main_path).unwrap();
    commit_batch_with_retry(&index);
    assert!(!search(&index, "first_edit").is_empty());

    // Edit 2: change again
    fs::write(&main_path, "fn second_edit() {}\n").unwrap();
    index.notify_change(&main_path).unwrap();
    commit_batch_with_retry(&index);

    // first_edit should be gone, second_edit should be present
    let first = search(&index, "first_edit");
    let first_in_main: Vec<_> = first.iter().filter(|(p, _)| p == "src/main.rs").collect();
    assert!(
        first_in_main.is_empty(),
        "first_edit should be gone from main.rs"
    );
    assert!(!search(&index, "second_edit").is_empty());
    drop(index);
}

/// Unmodified files remain searchable after overlay commit.
#[test]
fn unmodified_files_still_searchable() {
    let (repo, _idx, index) = setup();

    // Modify one file
    let main_path = repo.path().join("src/main.rs");
    fs::write(&main_path, "fn changed() {}\n").unwrap();
    index.notify_change(&main_path).unwrap();
    commit_batch_with_retry(&index);

    // Other files should still be searchable
    assert!(
        !search(&index, "process_batch").is_empty(),
        "unmodified lib.rs should still be searchable"
    );
    assert!(
        !search(&index, "helper").is_empty(),
        "unmodified util.rs should still be searchable"
    );
    drop(index);
}

// ---------------------------------------------------------------------------
// Security: path traversal rejection
// ---------------------------------------------------------------------------

/// notify_change rejects paths outside the repo root.
#[test]
fn path_outside_repo_rejected() {
    let (_repo, _idx, index) = setup();

    let outside = std::path::Path::new("/tmp/evil_file.rs");
    let result = index.notify_change(outside);
    assert!(result.is_err(), "path outside repo should be rejected");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("outside repo"),
        "error should mention 'outside repo', got: {err_msg}"
    );
    drop(index);
}

/// notify_delete rejects paths outside the repo root.
#[test]
fn delete_path_outside_repo_rejected() {
    let (_repo, _idx, index) = setup();

    let outside = std::path::Path::new("/tmp/evil_file.rs");
    let result = index.notify_delete(outside);
    assert!(result.is_err(), "delete outside repo should be rejected");
    drop(index);
}

/// notify_change rejects lexical traversal outside the repo root.
#[test]
fn path_with_parent_component_outside_repo_rejected() {
    let (repo, _idx, index) = setup();

    let traversal = repo.path().join("../evil_file.rs");
    let result = index.notify_change(&traversal);
    assert!(result.is_err(), "path traversal should be rejected");
    drop(index);
}

/// notify_delete rejects lexical traversal outside the repo root.
#[test]
fn delete_path_with_parent_component_outside_repo_rejected() {
    let (repo, _idx, index) = setup();

    let traversal = repo.path().join("../evil_file.rs");
    let result = index.notify_delete(&traversal);
    assert!(result.is_err(), "delete path traversal should be rejected");
    drop(index);
}

// ---------------------------------------------------------------------------
// Security: file size enforcement during commit
// ---------------------------------------------------------------------------

/// Files exceeding max_file_size are excluded during commit_batch (not a hard
/// error), matching how binary files are handled, so one oversized file cannot
/// wedge the incremental pipeline.
#[test]
fn large_file_excluded_during_commit() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/small.rs"), "fn small() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        max_file_size: 100, // very small limit
        ..Config::default()
    };
    let index = Index::build(config).expect("build");

    // Create a file that exceeds the limit
    let big_path = repo.path().join("src/big.rs");
    fs::write(&big_path, "x".repeat(200)).unwrap();

    index.notify_change(&big_path).unwrap();
    let result = commit_batch_result(&index);
    assert!(
        result.is_ok(),
        "oversized file must be excluded, not fail commit: {result:?}"
    );

    let snap = index.snapshot();
    assert!(
        !snap
            .path_index
            .paths
            .iter()
            .any(|p| p.as_ref() == std::path::Path::new("src/big.rs")),
        "oversized file should not appear in the path index after commit"
    );
    drop(index);
}

/// Incremental updates should skip binary files just like full builds.
#[test]
fn binary_file_added_during_commit_is_not_indexed() {
    let (repo, _idx, index) = setup();

    let binary_path = repo.path().join("src/data.bin");
    let mut binary = vec![0u8; 100];
    binary[0..5].copy_from_slice(b"BINAR");
    fs::write(&binary_path, binary).unwrap();

    index.notify_change(&binary_path).unwrap();
    commit_batch_with_retry(&index);

    let snap = index.snapshot();
    assert!(
        !snap
            .path_index
            .paths
            .iter()
            .any(|p| p.as_ref() == std::path::Path::new("src/data.bin")),
        "binary file should not appear in the path index after incremental commit"
    );
    drop(index);
}

/// A text file changed to binary should disappear from the visible index.
#[test]
fn text_file_changed_to_binary_is_removed_from_visible_index() {
    let (repo, _idx, index) = setup();

    let main_path = repo.path().join("src/main.rs");
    let mut binary = vec![0u8; 64];
    binary[0..4].copy_from_slice(b"BIN!");
    fs::write(&main_path, binary).unwrap();

    index.notify_change(&main_path).unwrap();
    commit_batch_with_retry(&index);

    let snap = index.snapshot();
    assert!(
        !snap
            .path_index
            .paths
            .iter()
            .any(|p| p.as_ref() == std::path::Path::new("src/main.rs")),
        "binary replacement should remove the path from the visible path index"
    );

    let results = search(&index, "parse_query");
    let main_results: Vec<_> = results.iter().filter(|(p, _)| p == "src/main.rs").collect();
    assert!(
        main_results.is_empty(),
        "binary replacement should remove stale search hits from the old text file"
    );
    drop(index);
}

// ---------------------------------------------------------------------------
// Concurrency: write lock conflict
// ---------------------------------------------------------------------------

/// A second concurrent commit_batch must return LockConflict, not block.
#[test]
fn concurrent_commit_batch_returns_lock_conflict() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    // Create two files so we have something to commit.
    fs::write(repo.path().join("a.rs"), "fn aaa() {}\n").unwrap();
    fs::write(repo.path().join("b.rs"), "fn bbb() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // Modify a file and notify.
    fs::write(repo.path().join("a.rs"), "fn aaa_v2() {}\n").unwrap();
    index.notify_change(&repo.path().join("a.rs")).unwrap();

    // Hold the write lock manually to simulate a concurrent writer.
    // Use the same open mode as the production writer-lock helper
    // (OpenOptions + try_lock_exclusive) for consistent macOS flock behavior.
    let lock_path = index_dir.path().join("write.lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock_file.try_lock().unwrap();

    // commit_batch should fail with LockConflict, not block or succeed.
    let result = index.commit_batch();
    assert!(result.is_err(), "should fail when lock is held");
    let err = result.unwrap_err();
    let err_str = format!("{err}");
    assert!(
        err_str.contains("lock") || err_str.contains("Lock"),
        "error should mention lock conflict: {err_str}"
    );

    // Release lock and verify commit succeeds.
    lock_file.unlock().unwrap();
    drop(lock_file);
    commit_batch_with_retry(&index);
    drop(index);
}

/// A full build must reject an in-flight incremental writer.
#[test]
fn build_returns_lock_conflict_while_writer_lock_is_held() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    fs::write(repo.path().join("a.rs"), "fn aaa() {}\n").unwrap();

    // Use the same open mode as the production writer-lock helper.
    let lock_path = index_dir.path().join("write.lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock_file.try_lock().unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let result = Index::build(config.clone());
    let err = match result {
        Ok(_) => panic!("build should fail when writer lock is held"),
        Err(err) => err,
    };
    let err_str = format!("{err}");
    assert!(
        err_str.contains("lock") || err_str.contains("Lock"),
        "error should mention lock conflict: {err_str}"
    );

    lock_file.unlock().unwrap();
    drop(lock_file);
    // After unlocking and closing the FD, build should succeed.
    let index = Index::build(config).unwrap();
    drop(index);
}

// ---------------------------------------------------------------------------
// Concurrency: readers during concurrent writes (ArcSwap snapshot isolation)
// ---------------------------------------------------------------------------

/// Readers calling search() during concurrent commit_batch() must see a
/// consistent snapshot: either pre-commit or post-commit, never a torn state.
#[test]
fn concurrent_reads_during_commit_batch() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};

    const READERS: usize = 4;

    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    // Seed the repo with files that contain a searchable pattern.
    for i in 0..10 {
        fs::write(
            repo.path().join(format!("file{i}.rs")),
            format!("fn func_{i}() {{}}\n"),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Arc::new(Index::build(config).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    // Rendezvous reader startup with writer start so every reader performs at
    // least one search before any commit runs. Without this, slow thread
    // scheduling (observed on Windows CI) can let the writer finish all rounds
    // and flip `stop` before any reader iterates, tripping the
    // `total_searches > 0` soundness assertion below.
    let barrier = Arc::new(Barrier::new(READERS + 1));

    // Spawn reader threads that search continuously.
    let mut readers = Vec::new();
    for _ in 0..READERS {
        let idx = Arc::clone(&index);
        let done = Arc::clone(&stop);
        let gate = Arc::clone(&barrier);
        readers.push(thread::spawn(move || {
            // One guaranteed pre-write search. All 10 seeded files contain
            // "fn" and no writer has run yet, so the match invariant holds
            // trivially. Counting it here guarantees total_searches >= READERS
            // regardless of scheduling.
            let matches = idx
                .search("fn", &SearchOptions::default())
                .expect("pre-write search must not fail");
            assert!(!matches.is_empty(), "pre-write search returned 0 matches");
            let mut search_count = 1u64;
            gate.wait();
            while !done.load(Ordering::Relaxed) {
                let result = idx.search("fn", &SearchOptions::default());
                // Must never error: the snapshot should always be consistent.
                let matches = result.expect("search must not fail during concurrent writes");
                // Every search should find at least one match (all files contain "fn").
                assert!(
                    !matches.is_empty(),
                    "search returned 0 matches during concurrent writes"
                );
                search_count += 1;
            }
            search_count
        }));
    }

    // Release the writer only after every reader has completed its guaranteed
    // search and entered the loop.
    barrier.wait();

    // Writer: modify files and commit several rounds.
    let mut commits_ok = 0u32;
    for round in 0..5 {
        fs::write(
            repo.path().join(format!("file{round}.rs")),
            format!("fn func_{round}_v{round}() {{}}\n"),
        )
        .unwrap();
        index
            .notify_change(&repo.path().join(format!("file{round}.rs")))
            .unwrap();
        // commit_batch may fail with LockConflict if a prior round is still
        // committing; retry once after a short sleep.
        let ok = if index.commit_batch().is_ok() {
            true
        } else {
            thread::sleep(Duration::from_millis(10));
            index.commit_batch().is_ok()
        };
        if ok {
            commits_ok += 1;
        }
    }
    assert!(commits_ok > 0, "writer failed to commit any round");

    // Signal readers to stop and join.
    stop.store(true, Ordering::Relaxed);
    let mut total_searches = 0u64;
    for reader in readers {
        total_searches += reader.join().expect("reader thread panicked");
    }

    // Sanity: readers actually ran (at least a few searches per thread).
    assert!(total_searches > 0, "reader threads performed zero searches");

    let index = Arc::try_unwrap(index).unwrap_or_else(|_| panic!("other Arcs still alive"));
    drop(index);
}

// ---------------------------------------------------------------------------
// Bounded auto-update: a failed/over-budget update must not lose changes
// ---------------------------------------------------------------------------

/// `update_from_git` returning `TooManyFiles` must bail out before queuing or
/// applying anything: `pending_edits` stays at 0 and the new content stays
/// invisible. The detected changes are not lost, though — a second call with
/// a raised `max_files` applies the same changes and the next search finds
/// the previously-detected content (RequeueGuard covers the OverlayFull/error
/// case the same way; see `commit_batch_failure_requeues_pending_edits` in
/// `src/index/tests.rs` for that half of the contract).
#[test]
fn too_many_files_leaves_index_stale_then_raised_limit_applies_changes() {
    use syntext::index::freshness::{UpdateLimits, UpdateOutcome};

    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);

    fs::write(repo.path().join("a.rs"), "fn original_marker() {}\n").unwrap();
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // 3 new untracked files exceed a max_files cap of 1.
    for i in 0..3 {
        fs::write(
            repo.path().join(format!("new_{i}.rs")),
            format!("fn too_many_marker_{i}() {{}}\n"),
        )
        .unwrap();
    }

    let low_limits = UpdateLimits {
        max_files: Some(1),
        budget_ms: None,
    };
    let outcome = index
        .update_from_git(low_limits)
        .expect("update_from_git must not error on a healthy git repo");
    match outcome {
        UpdateOutcome::TooManyFiles { files_behind, .. } => {
            assert_eq!(files_behind, 3, "all 3 new files should be counted");
        }
        other => panic!("expected TooManyFiles, got {other:?}"),
    }

    // Nothing was queued: TooManyFiles bails before the notify_change loop.
    assert_eq!(
        index.stats().pending_edits,
        0,
        "TooManyFiles must bail before queuing any edit"
    );
    // Nothing was applied: the new content is not yet searchable.
    assert!(
        search(&index, "too_many_marker_0").is_empty(),
        "new content must stay invisible while the update is over budget"
    );

    // A second call with a raised max_files applies all 3 changes: the
    // detected files were never lost, just deferred.
    let high_limits = UpdateLimits {
        max_files: Some(10),
        budget_ms: None,
    };
    let outcome2 = index
        .update_from_git(high_limits)
        .expect("update_from_git must succeed once max_files covers the change set");
    match outcome2 {
        UpdateOutcome::Updated { files, skipped, .. } => {
            assert_eq!(files, 3, "all 3 previously-deferred files should apply");
            assert_eq!(skipped, 0);
        }
        other => panic!("expected Updated, got {other:?}"),
    }

    // The next search finds the previously-queued (deferred) content.
    for i in 0..3 {
        assert!(
            !search(&index, &format!("too_many_marker_{i}")).is_empty(),
            "new_{i}.rs content should be searchable after the raised-limit update"
        );
    }
    drop(index);
}

// ---------------------------------------------------------------------------
// End-to-end freshness: real `st` binary, auto-update-on-search
// ---------------------------------------------------------------------------

/// A new untracked file created after `st index` becomes searchable on the
/// very next `st` search invocation, with no explicit `st update` and no
/// library-level `notify_change` call in between. This exercises the actual
/// auto-update-on-search path (git-based change detection bounded by
/// `auto_update_budget_ms` / `auto_update_max_files`, enabled by default),
/// end-to-end through the real CLI binary rather than the `Index` API.
#[test]
fn untracked_file_found_on_next_search_via_auto_update() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::write(repo.path().join("a.rs"), "fn original_marker() {}\n").unwrap();
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    let st = || std::process::Command::new(env!("CARGO_BIN_EXE_st"));

    let index_output = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index_dir.path())
        .args(["index", "--quiet"])
        .output()
        .expect("run st index");
    assert_eq!(
        index_output.status.code(),
        Some(0),
        "st index failed: {}",
        String::from_utf8_lossy(&index_output.stderr)
    );

    // Create a new, untracked file after the index was built. No `st update`
    // and no `notify_change` are issued: the next search alone must catch it.
    fs::write(repo.path().join("b.rs"), "fn untracked_marker_xyz() {}\n").unwrap();

    let search_output = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index_dir.path())
        // Slow git spawns on Windows CI starve the default 150ms budget before
        // ls-files --others detects untracked b.rs; give detection room.
        .env("SYNTEXT_AUTO_UPDATE_BUDGET_MS", "10000")
        .args(["-q", "untracked_marker_xyz"])
        .output()
        .expect("run st search");
    assert_eq!(
        search_output.status.code(),
        Some(0),
        "next `st` search should find the untracked file via auto-update; stderr: {}",
        String::from_utf8_lossy(&search_output.stderr)
    );
}

/// A "branch switch" that changes more files than `auto_update_max_files`
/// permits must leave the search stale with a staleness notice on stderr
/// (rather than block the search on an unbounded git-detection pass), but the
/// detached async catch-up (`st update --quiet`, spawned unbounded per
/// `catchup::maybe_spawn_async_catchup`) must land in the background so a
/// later search finds the content without ever running `st update` by hand.
/// Only the triggering search uses the restrictive
/// `SYNTEXT_AUTO_UPDATE_MAX_FILES=1` cap; the polling searches below use the
/// default (unrestricted) config, matching real usage where a caller doesn't
/// re-apply a synthetic cap on every subsequent invocation. Note this means a
/// poll's own in-band bounded auto-update (default cap of 200, well over the
/// 3-file delta here) could in principle also apply the change directly, not
/// just the async child -- both paths converge on the same on-disk state, so
/// either landing satisfies "the following search finds the content".
/// Re-applying the restrictive cap on every poll was tried and rejected: each
/// poll would then also report `TooManyFiles` and spawn its own competing
/// async child, and the resulting concurrent `st update` processes never
/// converge (verified manually while writing this test).
#[test]
fn branch_switch_stale_then_caught_up_via_async_update() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .unwrap()
    };
    git(&["init"]);
    fs::write(repo.path().join("a.rs"), "fn original_marker() {}\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    let st = || std::process::Command::new(env!("CARGO_BIN_EXE_st"));

    let index_output = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index_dir.path())
        .args(["index", "--quiet"])
        .output()
        .expect("run st index");
    assert_eq!(
        index_output.status.code(),
        Some(0),
        "st index failed: {}",
        String::from_utf8_lossy(&index_output.stderr)
    );

    // Simulate a branch switch that brings in 3 new untracked files, which
    // exceeds a max_files cap of 1.
    for i in 0..3 {
        fs::write(
            repo.path().join(format!("branch_new_{i}.rs")),
            format!("fn branch_switch_marker_{i}() {{}}\n"),
        )
        .unwrap();
    }

    let search_with_cap = || {
        st().arg("--repo-root")
            .arg(repo.path())
            .arg("--index-dir")
            .arg(index_dir.path())
            .env("SYNTEXT_AUTO_UPDATE_MAX_FILES", "1")
            // Budget must not starve ls-files --others (untracked detection) on
            // slow Windows CI, or the 3-file delta is never seen and the
            // max_files=1 cap can't trip. Budget is independent of the cap.
            .env("SYNTEXT_AUTO_UPDATE_BUDGET_MS", "10000")
            .arg("branch_switch_marker_0")
            .output()
            .expect("run st search")
    };

    // First search: the 3-file delta exceeds the cap, so the search's own
    // bounded auto-update reports TooManyFiles, prints the staleness notice,
    // and the new content is not yet visible.
    let first = search_with_cap();
    assert_eq!(
        first.status.code(),
        Some(1),
        "stale search should report no match yet (index not updated in-band); stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stderr = String::from_utf8_lossy(&first.stderr);
    assert!(
        first_stderr.contains("files behind") && first_stderr.contains("searching stale"),
        "expected a staleness notice on stderr, got: {first_stderr}"
    );

    // The staleness notice also spawned a detached, unbounded `st update
    // --quiet` catch-up. Poll with the default (unrestricted) config until
    // the content lands or a generous timeout elapses.
    let poll_search = |pattern: &str| {
        st().arg("--repo-root")
            .arg(repo.path())
            .arg("--index-dir")
            .arg(index_dir.path())
            // Each poll does its own in-band bounded detection (the async
            // catch-up on an unmoved HEAD is in-memory only, non-durable across
            // processes). Slow Windows git spawns must not starve the untracked
            // ls-files pass, or no poll ever sees the change.
            .env("SYNTEXT_AUTO_UPDATE_BUDGET_MS", "10000")
            .args(["-q", pattern])
            .output()
            .expect("run st search")
    };
    let poll_until_found = |pattern: &str, timeout: Duration| -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if poll_search(pattern).status.code() == Some(0) {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    };

    assert!(
        poll_until_found("branch_switch_marker_0", Duration::from_secs(20)),
        "async catch-up should eventually make branch_switch_marker_0 searchable"
    );

    // All 3 files from the branch switch should now be visible, not just the
    // one polled above: the async catch-up is unbounded (max_files: None),
    // so it applies the whole change set in one pass.
    for i in 1..3 {
        assert!(
            poll_until_found(&format!("branch_switch_marker_{i}"), Duration::from_secs(5)),
            "branch_switch_marker_{i} should be searchable after the async catch-up lands"
        );
    }
}

// ---------------------------------------------------------------------------
// Durable delta-segment updates on committed changes.
//
// Verifies CROSS-PROCESS durability: a HEAD move applied via `rebuild_if_stale`
// must survive dropping and reopening the `Index` (since the in-memory overlay
// is empty on reopen). This also tests that the persistent delete-set prevents
// duplicate matches from modified files.
// ---------------------------------------------------------------------------

/// A git repo + built index, with the paths needed to reopen a fresh `Index`.
struct DeltaFixture {
    repo: tempfile::TempDir,
    index_dir: tempfile::TempDir,
    index: Index,
}

impl DeltaFixture {
    fn config(&self) -> Config {
        Config {
            index_dir: self.index_dir.path().to_path_buf(),
            repo_root: self.repo.path().to_path_buf(),
            ..Config::default()
        }
    }

    fn git(&self, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(self.repo.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {out:?}");
    }

    /// Drop the current handle and open a brand-new one from disk. Proves the
    /// change is durable across processes, not just in this process's overlay.
    fn reopen(self) -> (tempfile::TempDir, tempfile::TempDir, Index) {
        let config = self.config();
        let DeltaFixture {
            repo,
            index_dir,
            index,
        } = self;
        drop(index);
        let reopened = Index::open(config).expect("reopen");
        (repo, index_dir, reopened)
    }
}

fn delta_setup() -> DeltaFixture {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {out:?}");
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(
        repo.path().join("src/main.rs"),
        "fn old_alpha_marker() { let shared_token = 1; }\n",
    )
    .unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).expect("build");
    DeltaFixture {
        repo,
        index_dir,
        index,
    }
}

/// A committed MODIFY is durable across processes, old content is gone, and the
/// modified file yields exactly one match (no stale-base + delta duplicate).
#[test]
fn committed_modify_visible_cross_process() {
    let fx = delta_setup();
    // New content keeps `shared_token` but swaps the fn marker.
    fs::write(
        fx.repo.path().join("src/main.rs"),
        "fn new_beta_marker() { let shared_token = 2; }\n",
    )
    .unwrap();
    fx.git(&["commit", "-am", "modify", "--no-gpg-sign"]);

    let stats = fx.index.rebuild_if_stale().expect("rebuild_if_stale");
    assert!(stats.is_some(), "HEAD moved, so an update must have run");

    let (_repo, _index_dir, index) = fx.reopen();
    assert_eq!(
        search(&index, "new_beta_marker").len(),
        1,
        "new content found once"
    );
    assert!(
        search(&index, "old_alpha_marker").is_empty(),
        "old content gone"
    );
    // shared_token is in the live file exactly once; the stale base doc must be
    // hidden by the persistent delete-set or it would match a second time.
    assert_eq!(
        search(&index, "shared_token").len(),
        1,
        "modified file must yield exactly one match, not a stale-base duplicate"
    );
    drop(index);
}

/// A committed ADD is durable across processes.
#[test]
fn committed_add_visible_cross_process() {
    let fx = delta_setup();
    fs::write(
        fx.repo.path().join("src/added.rs"),
        "fn brand_new_marker() {}\n",
    )
    .unwrap();
    fx.git(&["add", "-A"]);
    fx.git(&["commit", "-m", "add", "--no-gpg-sign"]);

    fx.index.rebuild_if_stale().expect("rebuild_if_stale");
    let (_repo, _index_dir, index) = fx.reopen();
    assert_eq!(
        search(&index, "brand_new_marker").len(),
        1,
        "added file found"
    );
    assert_eq!(
        search(&index, "old_alpha_marker").len(),
        1,
        "original still found"
    );
    drop(index);
}

/// A committed DELETE is durable across processes.
#[test]
fn committed_delete_gone_cross_process() {
    let fx = delta_setup();
    // Add a second file so the repo is non-empty after the delete.
    fs::write(fx.repo.path().join("src/keep.rs"), "fn keep_marker() {}\n").unwrap();
    fx.git(&["add", "-A"]);
    fx.git(&["commit", "-m", "add keep", "--no-gpg-sign"]);
    fx.index.rebuild_if_stale().expect("rebuild after add");

    fs::remove_file(fx.repo.path().join("src/main.rs")).unwrap();
    fx.git(&["commit", "-am", "delete main", "--no-gpg-sign"]);
    fx.index.rebuild_if_stale().expect("rebuild after delete");

    let (_repo, _index_dir, index) = fx.reopen();
    assert!(
        search(&index, "old_alpha_marker").is_empty(),
        "deleted file's content must be gone across processes"
    );
    assert_eq!(
        search(&index, "keep_marker").len(),
        1,
        "kept file still found"
    );
    drop(index);
}

/// A corrupt deletes sidecar makes `open()` FAIL CLOSED rather than silently
/// starting with an empty delete-set (which would resurrect stale base docs).
#[test]
fn corrupt_deletes_idx_fails_closed() {
    let fx = delta_setup();
    fs::write(
        fx.repo.path().join("src/main.rs"),
        "fn new_beta_marker() { let shared_token = 2; }\n",
    )
    .unwrap();
    fx.git(&["commit", "-am", "modify", "--no-gpg-sign"]);
    fx.index.rebuild_if_stale().expect("rebuild_if_stale");

    let config = fx.config();
    let index_dir = fx.index_dir.path().to_path_buf();
    drop(fx.index);

    // Find the generation-named deletes sidecar and corrupt its body.
    let deletes = std::fs::read_dir(&index_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("deletes-") && n.ends_with(".idx"))
        .expect("a modify delta must have written a deletes sidecar");
    let path = index_dir.join(&deletes);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();

    assert!(
        Index::open(config).is_err(),
        "a corrupt deletes sidecar must fail open, not start empty"
    );
    let _ = fx.repo;
}

/// A modify goes through the delta path (appends a segment + writes a deletes
/// sidecar), not a full rebuild (which would collapse back to one segment).
#[test]
fn delta_appends_segment_not_full_rebuild() {
    use syntext::__internal::Manifest;
    let fx = delta_setup();
    let before = Manifest::load(fx.index_dir.path()).unwrap();
    assert_eq!(before.segments.len(), 1, "one file -> one base segment");

    fs::write(
        fx.repo.path().join("src/main.rs"),
        "fn new_beta_marker() { let shared_token = 2; }\n",
    )
    .unwrap();
    fx.git(&["commit", "-am", "modify", "--no-gpg-sign"]);
    fx.index.rebuild_if_stale().expect("rebuild_if_stale");

    let after = Manifest::load(fx.index_dir.path()).unwrap();
    assert_eq!(after.segments.len(), 2, "delta appended a segment");
    assert!(
        after.overlay_deletes_file.is_some(),
        "a modify must persist a delete-set sidecar"
    );
    drop(fx.index);
}

/// Repeated deltas past the segment cap trigger compaction, which physically
/// drops superseded docs and clears the delete-set sidecar.
#[test]
fn many_deltas_trigger_compaction_reset() {
    use syntext::__internal::Manifest;
    let fx = delta_setup();
    let cap = fx.config().max_segments; // default 10

    for i in 0..(cap + 3) {
        fs::write(
            fx.repo.path().join("src/main.rs"),
            format!("fn gen_marker_{i}() {{ let shared_token = {i}; }}\n"),
        )
        .unwrap();
        fx.git(&["commit", "-am", &format!("gen {i}"), "--no-gpg-sign"]);
        fx.index.rebuild_if_stale().expect("rebuild_if_stale");
    }

    let manifest = Manifest::load(fx.index_dir.path()).unwrap();
    assert!(
        manifest.segments.len() <= cap,
        "segment count {} must stay bounded by max_segments {cap}",
        manifest.segments.len()
    );

    // Latest content is correct and de-duplicated after compaction.
    let (_repo, _index_dir, index) = fx.reopen();
    let last = cap + 2;
    assert_eq!(
        search(&index, &format!("gen_marker_{last}")).len(),
        1,
        "latest found once"
    );
    assert!(
        search(&index, "gen_marker_0").is_empty(),
        "earliest superseded content gone"
    );
    assert_eq!(
        search(&index, "shared_token").len(),
        1,
        "exactly one live doc for the repeatedly-modified file"
    );
    drop(index);
}

fn search_with_opts(index: &Index, pattern: &str, opts: SearchOptions) -> Vec<(String, u32)> {
    index
        .search(pattern, &opts)
        .unwrap()
        .into_iter()
        .map(|m| (m.path.to_string_lossy().into_owned(), m.line_number))
        .collect()
}

/// Regression test: paths.idx mismatch after delta flush doesn't cause silent false negatives.
#[test]
fn test_paths_idx_mismatch_cross_process() {
    let fx = delta_setup();
    // Add a new file src/helper.rs
    fs::write(
        fx.repo.path().join("src/helper.rs"),
        "fn helper_function() {}\n",
    )
    .unwrap();
    fx.git(&["add", "-A"]);
    fx.git(&["commit", "-m", "add helper", "--no-gpg-sign"]);

    let stats = fx.index.rebuild_if_stale().expect("rebuild_if_stale");
    assert!(stats.is_some(), "HEAD moved, update should run");

    // Reopen index (this loads paths.idx from disk)
    let (_repo, _index_dir, index) = fx.reopen();

    // Verify search with a type filter still finds the helper file!
    let mut opts = SearchOptions::default();
    opts.file_type = Some("rs".to_string());
    let results = search_with_opts(&index, "helper_function", opts);
    assert_eq!(
        results.len(),
        1,
        "helper_function must be found with type filter"
    );
    drop(index);
}

/// Regression test: gapped overlay doc_ids don't corrupt delta segment positional doc table.
#[test]
fn test_gapped_overlay_doc_ids_cross_process() {
    let fx = delta_setup();

    // 1. First commit_batch: modify src/main.rs (creates overlay doc id)
    fs::write(
        fx.repo.path().join("src/main.rs"),
        "fn modified_once() { let x = 1; }\n",
    )
    .unwrap();
    fx.index
        .notify_change(&fx.repo.path().join("src/main.rs"))
        .unwrap();
    commit_batch_with_retry(&fx.index);

    // 2. Second commit_batch: modify src/main.rs again (creating gap in overlay doc ids)
    fs::write(
        fx.repo.path().join("src/main.rs"),
        "fn modified_twice() { let x = 2; }\n",
    )
    .unwrap();
    fx.index
        .notify_change(&fx.repo.path().join("src/main.rs"))
        .unwrap();
    // This second commit_batch will evict the previous overlay doc id and assign a new one, leaving a gap!
    commit_batch_with_retry(&fx.index);

    // Now commit to git and run rebuild_if_stale to trigger delta flush of this gapped overlay!
    fx.git(&["commit", "-am", "twice", "--no-gpg-sign"]);
    let stats = fx.index.rebuild_if_stale().expect("rebuild_if_stale");
    assert!(stats.is_some(), "HEAD moved, update should run");

    // Reopen index
    let (_repo, _index_dir, index) = fx.reopen();

    // Search for the twice modified content and verify it matches exactly once
    assert_eq!(
        search(&index, "modified_twice").len(),
        1,
        "should find modified_twice"
    );
    assert!(
        search(&index, "modified_once").is_empty(),
        "old modification is gone"
    );
    drop(index);
}

/// A committed RENAME is durable across processes and deletes the old file path.
#[test]
fn committed_rename_visible_cross_process() {
    let fx = delta_setup();
    fx.git(&["mv", "src/main.rs", "src/new_main.rs"]);
    fx.git(&["commit", "-m", "rename main", "--no-gpg-sign"]);

    let stats = fx.index.rebuild_if_stale().expect("rebuild_if_stale");
    assert!(stats.is_some(), "HEAD moved, update should run");

    let (_repo, _index_dir, index) = fx.reopen();
    let next_matches = search(&index, "old_alpha_marker");
    assert_eq!(next_matches.len(), 1);
    assert_eq!(next_matches[0].0, "src/new_main.rs");

    assert!(
        search(&index, "old_alpha_marker")
            .iter()
            .all(|(p, _)| p != "src/main.rs"),
        "old path must be deleted"
    );
    drop(index);
}
