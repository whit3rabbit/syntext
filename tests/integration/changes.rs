use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

use syntext::changes::{Catalogue, ChangeKind};
use syntext::index::Index;
use syntext::{Config, SearchOptions};

#[path = "lock_retry.rs"]
mod lock_retry;

fn setup() -> (TempDir, TempDir, Index, Catalogue) {
    let repo_dir = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo_dir.path().to_path_buf(),
        ..Config::default()
    };

    // Initial base build with enough seed files so overlay edits stay under 50% capacity.
    for i in 0..10 {
        fs::write(
            repo_dir.path().join(format!("seed_{i}.rs")),
            format!("fn seed_{i}() {{}}\n"),
        )
        .unwrap();
    }
    let index = Index::build(config).unwrap();
    let cat = Catalogue::open_or_create(index_dir.path()).unwrap();

    (repo_dir, index_dir, index, cat)
}

fn search(index: &Index, pattern: &str) -> Vec<(String, u32)> {
    let opts = SearchOptions::default();
    index
        .search(pattern, &opts)
        .unwrap()
        .into_iter()
        .map(|m| (m.path.to_string_lossy().to_string(), m.line_number))
        .collect()
}

#[test]
fn add_and_query_via_apply_change_batch() {
    let (repo, _idx_dir, index, mut cat) = setup();

    let doc = repo.path().join("doc.rs");
    fs::write(&doc, b"fn alpha_beta_omega() {}\n").unwrap();

    let past_read = SystemTime::now() + Duration::from_secs(5);
    let batch = cat.observe_paths(repo.path(), &[PathBuf::from("doc.rs")], past_read);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch.records[0].kind, ChangeKind::Added);
    assert!(batch.records[0].content.is_some());

    let gen = index.apply_change_batch(&batch).unwrap();
    assert_eq!(gen, batch.generation);

    let hits = search(&index, "alpha_beta_omega");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "doc.rs");
}

#[test]
fn preloaded_content_survives_immediate_disk_deletion() {
    let (repo, _idx_dir, index, mut cat) = setup();

    let transient = repo.path().join("transient.rs");
    fs::write(&transient, b"fn transient_payload_key() {}\n").unwrap();

    let past_read = SystemTime::now() + Duration::from_secs(5);
    let batch = cat.observe_paths(repo.path(), &[PathBuf::from("transient.rs")], past_read);
    assert!(batch.records[0].content.is_some());

    // Delete the file from disk immediately after observation
    fs::remove_file(&transient).unwrap();
    assert!(!transient.exists());

    // apply_change_batch succeeds because the buffer was already loaded in memory
    index.apply_change_batch(&batch).unwrap();

    let hits = search(&index, "transient_payload_key");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "transient.rs");
}

#[test]
fn touched_file_does_not_mutate_index() {
    let (repo, _idx_dir, index, mut cat) = setup();

    let file = repo.path().join("touch_test.rs");
    fs::write(&file, b"fn touch_target() {}\n").unwrap();

    let past_read = SystemTime::now() + Duration::from_secs(5);
    let batch1 = cat.observe_paths(repo.path(), &[PathBuf::from("touch_test.rs")], past_read);
    index.apply_change_batch(&batch1).unwrap();
    assert_eq!(search(&index, "touch_target").len(), 1);

    // Re-writing exact same content updates mtime
    fs::write(&file, b"fn touch_target() {}\n").unwrap();
    let batch2 = cat.observe_paths(repo.path(), &[PathBuf::from("touch_test.rs")], past_read);
    assert_eq!(batch2.records[0].kind, ChangeKind::Touched);

    let gen = index.apply_change_batch(&batch2).unwrap();
    assert_eq!(gen, batch1.generation); // Touched did not increment generation
}

#[test]
fn modify_and_delete_lifecycle() {
    let (repo, _idx_dir, index, mut cat) = setup();

    let file = repo.path().join("lifecycle.rs");
    fs::write(&file, b"fn initial_token_abc() {}\n").unwrap();

    let past_read = SystemTime::now() + Duration::from_secs(5);
    let batch1 = cat.observe_paths(repo.path(), &[PathBuf::from("lifecycle.rs")], past_read);
    index.apply_change_batch(&batch1).unwrap();
    assert_eq!(search(&index, "initial_token_abc").len(), 1);

    // Modify content
    fs::write(&file, b"fn modified_token_xyz() {}\n").unwrap();
    let batch2 = cat.observe_paths(repo.path(), &[PathBuf::from("lifecycle.rs")], past_read);
    assert_eq!(batch2.records[0].kind, ChangeKind::Modified);
    index.apply_change_batch(&batch2).unwrap();

    assert!(search(&index, "initial_token_abc").is_empty());
    assert_eq!(search(&index, "modified_token_xyz").len(), 1);

    // Delete file
    fs::remove_file(&file).unwrap();
    let batch3 = cat.observe_paths(repo.path(), &[PathBuf::from("lifecycle.rs")], past_read);
    assert_eq!(batch3.records[0].kind, ChangeKind::Deleted);
    index.apply_change_batch(&batch3).unwrap();

    assert!(search(&index, "modified_token_xyz").is_empty());
}

#[test]
fn consumer_lag_and_acknowledgement() {
    let (repo, _idx_dir, index, mut cat) = setup();

    let f1 = repo.path().join("f1.rs");
    fs::write(&f1, b"fn one() {}\n").unwrap();
    let past_read = SystemTime::now() + Duration::from_secs(5);

    let b1 = cat.observe_paths(repo.path(), &[PathBuf::from("f1.rs")], past_read);
    index.apply_change_batch(&b1).unwrap();

    assert_eq!(cat.consumer_lag("synrepo"), 1);
    cat.acknowledge("synrepo", 1);
    assert_eq!(cat.consumer_lag("synrepo"), 0);

    let f2 = repo.path().join("f2.rs");
    fs::write(&f2, b"fn two() {}\n").unwrap();
    let b2 = cat.observe_paths(repo.path(), &[PathBuf::from("f2.rs")], past_read);
    index.apply_change_batch(&b2).unwrap();

    assert_eq!(cat.consumer_lag("synrepo"), 1);
    assert_eq!(cat.consumer_lag("other_consumer"), 2);
}
