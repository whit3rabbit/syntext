//! Unit tests for the working-tree anchor's semantics (the on-disk format is
//! covered by `worktree_codec_tests.rs`).

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use super::*;

/// Write `rel` under `root` with an mtime `age` in the past, so the racy-mtime
/// rule treats it as settled against a `read_epoch` of "now".
fn write_aged(root: &std::path::Path, rel: &str, contents: &str, age: Duration) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, contents).unwrap();
    let when = SystemTime::now() - age;
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn set(paths: &[&str]) -> HashSet<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

#[test]
fn a_settled_file_is_anchored_and_reads_back_as_unchanged() {
    let repo = tempfile::TempDir::new().unwrap();
    write_aged(repo.path(), "a.rs", "alpha", Duration::from_secs(10));

    let anchor = WorktreeAnchor::build_next(
        &WorktreeAnchor::default(),
        &set(&["a.rs"]),
        &HashSet::new(),
        repo.path(),
        SystemTime::now(),
    )
    .expect("under MAX_ENTRIES");

    assert_eq!(anchor.len(), 1);
    assert!(anchor.is_unchanged(repo.path(), &PathBuf::from("a.rs")));
    assert!(
        !anchor.is_unchanged(repo.path(), &PathBuf::from("never-seen.rs")),
        "a path the anchor does not cover is always 'changed'"
    );
}

#[test]
fn editing_an_anchored_file_makes_it_changed_again() {
    let repo = tempfile::TempDir::new().unwrap();
    write_aged(repo.path(), "a.rs", "alpha", Duration::from_secs(10));

    let anchor = WorktreeAnchor::build_next(
        &WorktreeAnchor::default(),
        &set(&["a.rs"]),
        &HashSet::new(),
        repo.path(),
        SystemTime::now(),
    )
    .unwrap();
    assert!(anchor.is_unchanged(repo.path(), &PathBuf::from("a.rs")));

    write_aged(repo.path(), "a.rs", "alpha edited", Duration::from_secs(1));
    assert!(
        !anchor.is_unchanged(repo.path(), &PathBuf::from("a.rs")),
        "size and mtime both moved"
    );
}

#[test]
fn a_racily_recent_write_is_left_out_rather_than_wrongly_trusted() {
    // The whole point of the racy-mtime rule: a file written in the same tick
    // as the read cannot be distinguished from one written just after it. Being
    // left out means it gets re-applied once, which is the safe direction.
    let repo = tempfile::TempDir::new().unwrap();
    write_aged(
        repo.path(),
        "racy.rs",
        "just written",
        Duration::from_secs(0),
    );

    let anchor = WorktreeAnchor::build_next(
        &WorktreeAnchor::default(),
        &set(&["racy.rs"]),
        &HashSet::new(),
        repo.path(),
        SystemTime::now(),
    )
    .unwrap();

    assert!(anchor.is_empty(), "a same-tick write must not be anchored");
}

#[test]
fn a_future_mtime_is_left_out() {
    // Clock skew or a network filesystem. A future mtime says nothing about
    // whether the file settled before the read.
    let repo = tempfile::TempDir::new().unwrap();
    let path = repo.path().join("ahead.rs");
    fs::write(&path, "from the future").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(3600))
        .unwrap();

    let anchor = WorktreeAnchor::build_next(
        &WorktreeAnchor::default(),
        &set(&["ahead.rs"]),
        &HashSet::new(),
        repo.path(),
        SystemTime::now(),
    )
    .unwrap();

    assert!(anchor.is_empty());
}

#[test]
fn a_deleted_non_doc_path_is_anchored_absent() {
    // Without this, a deleted tracked file costs a notify_delete on every
    // search until it is committed.
    let repo = tempfile::TempDir::new().unwrap();
    let anchor = WorktreeAnchor::build_next(
        &WorktreeAnchor::default(),
        &HashSet::new(),
        &set(&["gone.rs"]),
        repo.path(),
        SystemTime::now(),
    )
    .unwrap();

    assert_eq!(anchor.len(), 1);
    assert!(anchor.is_unchanged(repo.path(), &PathBuf::from("gone.rs")));

    // Recreated: no longer absent, so it must be re-applied.
    write_aged(repo.path(), "gone.rs", "back", Duration::from_secs(10));
    assert!(!anchor.is_unchanged(repo.path(), &PathBuf::from("gone.rs")));
}

#[test]
fn an_excluded_path_still_present_on_disk_is_anchored_present() {
    // Binary and oversized files are not documents but they are on disk.
    // Anchoring them stops the per-search re-classification.
    let repo = tempfile::TempDir::new().unwrap();
    write_aged(repo.path(), "big.bin", "\0\0\0", Duration::from_secs(10));

    let anchor = WorktreeAnchor::build_next(
        &WorktreeAnchor::default(),
        &HashSet::new(),
        &set(&["big.bin"]),
        repo.path(),
        SystemTime::now(),
    )
    .unwrap();

    assert!(anchor.is_unchanged(repo.path(), &PathBuf::from("big.bin")));
}

#[test]
fn a_path_that_is_both_a_doc_and_a_non_doc_is_recorded_as_the_doc() {
    // Deleted in one commit and recreated in the next: the document is what
    // actually went into the segment.
    let repo = tempfile::TempDir::new().unwrap();
    write_aged(repo.path(), "flip.rs", "recreated", Duration::from_secs(10));

    let anchor = WorktreeAnchor::build_next(
        &WorktreeAnchor::default(),
        &set(&["flip.rs"]),
        &set(&["flip.rs"]),
        repo.path(),
        SystemTime::now(),
    )
    .unwrap();

    assert_eq!(anchor.len(), 1);
    assert!(anchor.is_unchanged(repo.path(), &PathBuf::from("flip.rs")));
}

#[test]
fn untouched_previous_entries_carry_forward_and_untrusted_ones_are_dropped() {
    let repo = tempfile::TempDir::new().unwrap();
    write_aged(
        repo.path(),
        "old.rs",
        "flushed earlier",
        Duration::from_secs(10),
    );
    write_aged(
        repo.path(),
        "new.rs",
        "flushed now",
        Duration::from_secs(10),
    );

    let first = WorktreeAnchor::build_next(
        &WorktreeAnchor::default(),
        &set(&["old.rs"]),
        &HashSet::new(),
        repo.path(),
        SystemTime::now(),
    )
    .unwrap();

    let second = WorktreeAnchor::build_next(
        &first,
        &set(&["new.rs"]),
        &HashSet::new(),
        repo.path(),
        SystemTime::now(),
    )
    .unwrap();

    assert_eq!(second.len(), 2, "a path flushed earlier is still flushed");
    assert!(second.is_unchanged(repo.path(), &PathBuf::from("old.rs")));
    assert!(second.is_unchanged(repo.path(), &PathBuf::from("new.rs")));

    // Now touch `old.rs` racily and re-flush it. The stale entry must be
    // dropped, not left behind: keeping it could match by coincidence.
    write_aged(
        repo.path(),
        "old.rs",
        "flushed earlier",
        Duration::from_secs(0),
    );
    let third = WorktreeAnchor::build_next(
        &second,
        &set(&["old.rs"]),
        &HashSet::new(),
        repo.path(),
        SystemTime::now(),
    )
    .unwrap();

    assert_eq!(third.len(), 1);
    assert!(!third.is_unchanged(repo.path(), &PathBuf::from("old.rs")));
    assert!(third.is_unchanged(repo.path(), &PathBuf::from("new.rs")));
}

#[test]
fn exceeding_max_entries_writes_no_anchor_at_all() {
    // No anchor is always correct, just slower. A synthetic previous anchor is
    // cheaper to build than MAX_ENTRIES real files.
    let repo = tempfile::TempDir::new().unwrap();
    let mut entries = std::collections::HashMap::new();
    for i in 0..=MAX_ENTRIES {
        entries.insert(PathBuf::from(format!("f{i}.rs")), Observed::Absent);
    }
    let huge = WorktreeAnchor::from_entries(entries);

    assert!(WorktreeAnchor::build_next(
        &huge,
        &HashSet::new(),
        &HashSet::new(),
        repo.path(),
        SystemTime::now(),
    )
    .is_none());
}
