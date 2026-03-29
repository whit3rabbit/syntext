//! Integration tests for incremental index updates (T044).
//!
//! Build index -> modify file -> commit_batch -> search for new content.
//! Verifies read-your-writes freshness, delete visibility, and
//! interleaved edit+search consistency.

use std::fs;

use syntext::index::Index;
use syntext::{Config, SearchOptions};

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
    index.commit_batch().unwrap();

    // After commit: new content is visible
    let results = search(&index, "transform_data");
    assert!(
        !results.is_empty(),
        "transform_data should be visible after commit"
    );
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
    index.commit_batch().unwrap();

    // parse_query should no longer appear in src/main.rs results
    let results = search(&index, "parse_query");
    let main_results: Vec<_> = results.iter().filter(|(p, _)| p == "src/main.rs").collect();
    assert!(
        main_results.is_empty(),
        "parse_query should not be in modified file, got {:?}",
        main_results
    );
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
    index.commit_batch().unwrap();

    // Should no longer find results from deleted file
    let results = search(&index, "process_batch");
    let lib_results: Vec<_> = results.iter().filter(|(p, _)| p == "src/lib.rs").collect();
    assert!(
        lib_results.is_empty(),
        "deleted file should not appear in results"
    );
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
    index.commit_batch().unwrap();
    let results = search(&index, "pending_content_xyz");
    assert!(
        !results.is_empty(),
        "new file should be visible after commit"
    );
}

#[test]
fn empty_commit_batch_is_noop() {
    let (_repo, _idx, index) = setup();

    index.commit_batch().unwrap();
    index.commit_batch().unwrap();

    assert!(!search(&index, "parse_query").is_empty());
    assert!(!search(&index, "process_batch").is_empty());
}

#[test]
fn path_index_tracks_incremental_visible_paths() {
    let (repo, _idx, index) = setup();

    let new_path = repo.path().join("src/new_module.rs");
    fs::write(&new_path, "fn brand_new_function() { 42 }\n").unwrap();
    index.notify_change(&new_path).unwrap();
    index.commit_batch().unwrap();

    let snap = index.snapshot();
    assert!(snap
        .path_index
        .paths
        .iter()
        .any(|p| p == "src/new_module.rs"));

    let deleted_path = repo.path().join("src/lib.rs");
    fs::remove_file(&deleted_path).unwrap();
    index.notify_delete(&deleted_path).unwrap();
    index.commit_batch().unwrap();

    let snap = index.snapshot();
    assert!(!snap
        .path_index
        .paths
        .iter()
        .any(|p| p == std::path::Path::new("src/lib.rs")));
}

/// Adding a new file makes it searchable after commit.
#[test]
fn add_new_file() {
    let (repo, _idx, index) = setup();

    let new_path = repo.path().join("src/new_module.rs");
    fs::write(&new_path, "fn brand_new_function() { 42 }\n").unwrap();

    index.notify_change(&new_path).unwrap();
    index.commit_batch().unwrap();

    let results = search(&index, "brand_new_function");
    assert!(!results.is_empty(), "newly added file should be searchable");
}

/// Interleaved edits and searches maintain consistency.
#[test]
fn interleaved_edit_search() {
    let (repo, _idx, index) = setup();

    let main_path = repo.path().join("src/main.rs");

    // Edit 1: change content
    fs::write(&main_path, "fn first_edit() {}\n").unwrap();
    index.notify_change(&main_path).unwrap();
    index.commit_batch().unwrap();
    assert!(!search(&index, "first_edit").is_empty());

    // Edit 2: change again
    fs::write(&main_path, "fn second_edit() {}\n").unwrap();
    index.notify_change(&main_path).unwrap();
    index.commit_batch().unwrap();

    // first_edit should be gone, second_edit should be present
    let first = search(&index, "first_edit");
    let first_in_main: Vec<_> = first.iter().filter(|(p, _)| p == "src/main.rs").collect();
    assert!(
        first_in_main.is_empty(),
        "first_edit should be gone from main.rs"
    );
    assert!(!search(&index, "second_edit").is_empty());
}

/// Unmodified files remain searchable after overlay commit.
#[test]
fn unmodified_files_still_searchable() {
    let (repo, _idx, index) = setup();

    // Modify one file
    let main_path = repo.path().join("src/main.rs");
    fs::write(&main_path, "fn changed() {}\n").unwrap();
    index.notify_change(&main_path).unwrap();
    index.commit_batch().unwrap();

    // Other files should still be searchable
    assert!(
        !search(&index, "process_batch").is_empty(),
        "unmodified lib.rs should still be searchable"
    );
    assert!(
        !search(&index, "helper").is_empty(),
        "unmodified util.rs should still be searchable"
    );
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
}

/// notify_delete rejects paths outside the repo root.
#[test]
fn delete_path_outside_repo_rejected() {
    let (_repo, _idx, index) = setup();

    let outside = std::path::Path::new("/tmp/evil_file.rs");
    let result = index.notify_delete(outside);
    assert!(result.is_err(), "delete outside repo should be rejected");
}

/// notify_change rejects lexical traversal outside the repo root.
#[test]
fn path_with_parent_component_outside_repo_rejected() {
    let (repo, _idx, index) = setup();

    let traversal = repo.path().join("../evil_file.rs");
    let result = index.notify_change(&traversal);
    assert!(result.is_err(), "path traversal should be rejected");
}

/// notify_delete rejects lexical traversal outside the repo root.
#[test]
fn delete_path_with_parent_component_outside_repo_rejected() {
    let (repo, _idx, index) = setup();

    let traversal = repo.path().join("../evil_file.rs");
    let result = index.notify_delete(&traversal);
    assert!(result.is_err(), "delete path traversal should be rejected");
}

// ---------------------------------------------------------------------------
// Security: file size enforcement during commit
// ---------------------------------------------------------------------------

/// Files exceeding max_file_size are rejected during commit_batch.
#[test]
fn large_file_rejected_during_commit() {
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
    let result = index.commit_batch();
    assert!(result.is_err(), "oversized file should fail commit");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("too large"),
        "error should mention 'too large', got: {err_msg}"
    );
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
    index.commit_batch().unwrap();

    let snap = index.snapshot();
    assert!(
        !snap
            .path_index
            .paths
            .iter()
            .any(|p| p == std::path::Path::new("src/data.bin")),
        "binary file should not appear in the path index after incremental commit"
    );
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
    index.commit_batch().unwrap();

    let snap = index.snapshot();
    assert!(
        !snap
            .path_index
            .paths
            .iter()
            .any(|p| p == std::path::Path::new("src/main.rs")),
        "binary replacement should remove the path from the visible path index"
    );

    let results = search(&index, "parse_query");
    let main_results: Vec<_> = results.iter().filter(|(p, _)| p == "src/main.rs").collect();
    assert!(
        main_results.is_empty(),
        "binary replacement should remove stale search hits from the old text file"
    );
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
    let lock_path = index_dir.path().join("write.lock");
    let lock_file = std::fs::File::create(&lock_path).unwrap();
    use fs2::FileExt;
    lock_file.lock_exclusive().unwrap();

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
    index.commit_batch().unwrap();
}

/// A full build must reject an in-flight incremental writer.
#[test]
fn build_returns_lock_conflict_while_writer_lock_is_held() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    fs::write(repo.path().join("a.rs"), "fn aaa() {}\n").unwrap();

    let lock_path = index_dir.path().join("write.lock");
    let lock_file = std::fs::File::create(&lock_path).unwrap();
    use fs2::FileExt;
    lock_file.lock_exclusive().unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let result = Index::build(config);
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
}
