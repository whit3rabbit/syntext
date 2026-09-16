use super::*;
use std::fs;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn catalogue_lifecycle_and_observation() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    let index_dir = tmp.path().join("idx");
    fs::create_dir_all(&repo_root).unwrap();
    fs::create_dir_all(&index_dir).unwrap();

    let mut cat = Catalogue::open_or_create(&index_dir).unwrap();
    assert_eq!(cat.generation(), 0);
    assert_eq!(cat.len(), 0);

    // 1. Add file
    let file1 = repo_root.join("hello.rs");
    fs::write(&file1, b"fn hello() {}\n").unwrap();
    let t0 = SystemTime::now() - Duration::from_secs(20);
    fs::File::options()
        .write(true)
        .open(&file1)
        .unwrap()
        .set_modified(t0)
        .unwrap();

    // Read epoch past racy margin
    let past = SystemTime::now() + Duration::from_secs(5);
    let batch1 = cat.observe_paths(&repo_root, &[PathBuf::from("hello.rs")], past);
    assert_eq!(batch1.generation, 1);
    assert_eq!(batch1.len(), 1);
    assert_eq!(batch1.records[0].kind, ChangeKind::Added);
    assert_eq!(batch1.records[0].path, PathBuf::from("hello.rs"));
    assert!(batch1.records[0].content.is_some());
    assert_eq!(cat.generation(), 1);
    assert_eq!(cat.len(), 1);

    // 2. Unchanged observation
    let batch2 = cat.observe_paths(&repo_root, &[PathBuf::from("hello.rs")], past);
    assert_eq!(batch2.generation, 1);
    assert_eq!(batch2.records[0].kind, ChangeKind::Unchanged);
    assert_eq!(cat.generation(), 1);

    // 3. Touch file (mtime updated, content same)
    let past_read = SystemTime::now() + Duration::from_secs(10);
    let t1 = SystemTime::now() - Duration::from_secs(10);
    fs::File::options()
        .write(true)
        .open(&file1)
        .unwrap()
        .set_modified(t1)
        .unwrap();
    let batch3 = cat.observe_paths(&repo_root, &[PathBuf::from("hello.rs")], past_read);
    assert_eq!(batch3.records[0].kind, ChangeKind::Touched);
    assert_eq!(cat.generation(), 1); // Touched does not bump generation

    // 4. Modify content
    fs::write(&file1, b"fn hello_world() {}\n").unwrap();
    let batch4 = cat.observe_paths(&repo_root, &[PathBuf::from("hello.rs")], past_read);
    assert_eq!(batch4.records[0].kind, ChangeKind::Modified);
    assert_eq!(batch4.generation, 2);
    assert_eq!(cat.generation(), 2);

    // 5. Consumer tracking
    assert_eq!(cat.consumer_lag("synrepo"), 2);
    cat.acknowledge("synrepo", 2);
    assert_eq!(cat.consumer_lag("synrepo"), 0);

    // 6. Delete file
    fs::remove_file(&file1).unwrap();
    let batch5 = cat.observe_paths(&repo_root, &[PathBuf::from("hello.rs")], past_read);
    assert_eq!(batch5.records[0].kind, ChangeKind::Deleted);
    assert_eq!(batch5.generation, 3);
    assert_eq!(cat.len(), 0);

    // 7. Save and reopen
    cat.save().unwrap();
    let reloaded = Catalogue::open_or_create(&index_dir).unwrap();
    assert_eq!(reloaded.generation(), 3);
    assert_eq!(reloaded.len(), 0);
    assert_eq!(reloaded.consumer_lag("synrepo"), 1); // gen 3 vs ack 2
}
