//! Unit tests for the delete-set sidecar (`deletes-<uuid>.idx`).

use super::*;
use roaring::RoaringBitmap;

fn sample_bitmap() -> RoaringBitmap {
    let mut bm = RoaringBitmap::new();
    for id in [0u32, 1, 5, 42, 1000, u32::MAX - 1] {
        bm.insert(id);
    }
    bm
}

#[test]
fn round_trip_non_empty() {
    let dir = tempfile::tempdir().unwrap();
    let name = new_filename();
    let bm = sample_bitmap();
    write_deletes_idx(dir.path(), &name, &bm).unwrap();
    let got = read_deletes_idx(dir.path(), &name).unwrap();
    assert_eq!(got, bm);
}

#[test]
fn round_trip_empty() {
    let dir = tempfile::tempdir().unwrap();
    let name = new_filename();
    let bm = RoaringBitmap::new();
    write_deletes_idx(dir.path(), &name, &bm).unwrap();
    let got = read_deletes_idx(dir.path(), &name).unwrap();
    assert!(got.is_empty());
}

#[test]
fn missing_file_is_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = read_deletes_idx(dir.path(), &new_filename()).unwrap_err();
    assert!(matches!(err, SidecarError::Io(_)));
}

#[test]
fn bad_magic_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let name = new_filename();
    write_deletes_idx(dir.path(), &name, &sample_bitmap()).unwrap();
    let path = dir.path().join(&name);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] = b'X';
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        read_deletes_idx(dir.path(), &name).unwrap_err(),
        SidecarError::BadMagic
    ));
}

#[test]
fn wrong_version_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let name = new_filename();
    write_deletes_idx(dir.path(), &name, &sample_bitmap()).unwrap();
    let path = dir.path().join(&name);
    let mut bytes = std::fs::read(&path).unwrap();
    // version lives at bytes[4..8]
    bytes[4] = bytes[4].wrapping_add(9);
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        read_deletes_idx(dir.path(), &name).unwrap_err(),
        SidecarError::UnsupportedVersion(_)
    ));
}

#[test]
fn checksum_mismatch_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let name = new_filename();
    write_deletes_idx(dir.path(), &name, &sample_bitmap()).unwrap();
    let path = dir.path().join(&name);
    let mut bytes = std::fs::read(&path).unwrap();
    // Flip a byte in the body (after the 16-byte header) without touching the
    // stored checksum, so decode detects the corruption.
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        read_deletes_idx(dir.path(), &name).unwrap_err(),
        SidecarError::ChecksumMismatch
    ));
}

#[test]
fn truncated_header_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let name = new_filename();
    let path = dir.path().join(&name);
    std::fs::write(&path, b"ST").unwrap();
    assert!(matches!(
        read_deletes_idx(dir.path(), &name).unwrap_err(),
        SidecarError::TooShort
    ));
}

#[test]
fn escaping_filename_rejected() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["../evil.idx", "sub/deletes.idx", "..", ""] {
        assert!(matches!(
            read_deletes_idx(dir.path(), bad).unwrap_err(),
            SidecarError::BadFilename
        ));
        assert!(write_deletes_idx(dir.path(), bad, &sample_bitmap()).is_err());
    }
}
