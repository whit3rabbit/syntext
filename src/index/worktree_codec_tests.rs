//! Unit tests for the `worktree-<uuid>.idx` on-disk format.

use std::collections::HashMap;
use std::path::PathBuf;

use super::*;

fn sample() -> WorktreeAnchor {
    let mut entries = HashMap::new();
    entries.insert(PathBuf::from("src/a.rs"), Observed::Absent);
    entries.insert(
        PathBuf::from("src/nested/b.rs"),
        Observed::Present {
            size: 4096,
            mtime_secs: 1_777_000_000,
            mtime_nanos: 123_456_789,
        },
    );
    WorktreeAnchor::from_entries(entries)
}

fn entries_of(anchor: &WorktreeAnchor) -> Vec<(PathBuf, Observed)> {
    let mut v: Vec<(PathBuf, Observed)> = anchor
        .entries
        .iter()
        .map(|(p, o)| (p.clone(), *o))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[test]
fn round_trips_both_observation_kinds() {
    let dir = tempfile::TempDir::new().unwrap();
    let name = new_filename();
    let original = sample();

    write_worktree_anchor(dir.path(), &name, &original).unwrap();
    let loaded = read_worktree_anchor(dir.path(), &name).unwrap();

    assert_eq!(entries_of(&loaded), entries_of(&original));
}

#[test]
fn an_empty_anchor_round_trips() {
    let dir = tempfile::TempDir::new().unwrap();
    let name = new_filename();
    write_worktree_anchor(dir.path(), &name, &WorktreeAnchor::default()).unwrap();
    assert!(read_worktree_anchor(dir.path(), &name).unwrap().is_empty());
}

#[test]
fn encoding_is_deterministic_so_the_checksum_is_reproducible() {
    // `entries` is a HashMap, so the encoder has to sort. Without that, two
    // identical anchors would hash differently and the round-trip tests would
    // be checking nothing in particular.
    assert_eq!(encode(&sample()), encode(&sample()));
}

#[test]
fn a_corrupt_file_is_rejected_rather_than_half_decoded() {
    let dir = tempfile::TempDir::new().unwrap();
    let name = new_filename();
    write_worktree_anchor(dir.path(), &name, &sample()).unwrap();
    let good = std::fs::read(dir.path().join(&name)).unwrap();

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("bad magic", {
            let mut b = good.clone();
            b[0] = b'X';
            b
        }),
        ("unsupported version", {
            let mut b = good.clone();
            b[4] = 99;
            b
        }),
        ("flipped body byte", {
            let mut b = good.clone();
            let last = b.len() - 1;
            b[last] ^= 0xff;
            b
        }),
        ("truncated mid-entry", good[..good.len() - 6].to_vec()),
        ("header only", good[..HEADER_LEN].to_vec()),
        ("shorter than the header", good[..4].to_vec()),
    ];

    for (label, bytes) in cases {
        std::fs::write(dir.path().join(&name), &bytes).unwrap();
        assert!(
            read_worktree_anchor(dir.path(), &name).is_err(),
            "{label} should be rejected"
        );
    }
}

#[test]
fn a_filename_that_could_escape_the_index_dir_is_refused_both_ways() {
    let dir = tempfile::TempDir::new().unwrap();
    for name in ["../escape.idx", "sub/dir.idx", "", "/abs.idx"] {
        assert!(read_worktree_anchor(dir.path(), name).is_err(), "read {name}");
        assert!(
            write_worktree_anchor(dir.path(), name, &sample()).is_err(),
            "write {name}"
        );
    }
}

#[test]
fn a_missing_file_is_an_error_not_an_empty_anchor() {
    // The fail-open decision belongs to the caller (`Index::load_worktree_anchor`),
    // not to the codec: the codec reports what it saw.
    let dir = tempfile::TempDir::new().unwrap();
    assert!(read_worktree_anchor(dir.path(), &new_filename()).is_err());
}
