use super::*;
use crate::path::PathIndex;
use std::path::PathBuf;

/// Minimal decoder used only to validate the encoder's byte layout, kept
/// alongside `decode` (which adds bounds-checked error handling for
/// untrusted/corrupt input) as an independent check of the format itself.
fn decode_for_test(bytes: &[u8]) -> PathIndex {
    assert_eq!(&bytes[0..4], MAGIC);
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    assert_eq!(version, FORMAT_VERSION);
    let checksum = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let body = &bytes[16..];
    assert_eq!(xxh64(body, 0), checksum);

    fn read_u32(b: &[u8], pos: &mut usize) -> u32 {
        let v = u32::from_le_bytes(b[*pos..*pos + 4].try_into().unwrap());
        *pos += 4;
        v
    }

    fn read_table(body: &[u8], pos: &mut usize) -> HashMap<Vec<u8>, RoaringBitmap> {
        let count = read_u32(body, pos) as usize;
        let mut table = HashMap::with_capacity(count);
        for _ in 0..count {
            let key_len = read_u32(body, pos) as usize;
            let key = body[*pos..*pos + key_len].to_vec();
            *pos += key_len;
            let bm_len = read_u32(body, pos) as usize;
            let bm_bytes = &body[*pos..*pos + bm_len];
            *pos += bm_len;
            table.insert(key, roaring_util::deserialize(bm_bytes).unwrap());
        }
        table
    }

    let mut pos = 0usize;
    let path_count = read_u32(body, &mut pos) as usize;
    let mut paths = Vec::with_capacity(path_count);
    for _ in 0..path_count {
        let len = read_u32(body, &mut pos) as usize;
        let bytes = &body[pos..pos + len];
        pos += len;
        paths.push(PathBuf::from(String::from_utf8(bytes.to_vec()).unwrap()));
    }

    let extension_to_files = read_table(body, &mut pos);
    let component_to_files = read_table(body, &mut pos);
    assert_eq!(pos, body.len(), "trailing bytes after decode");

    crate::path::from_sidecar_parts(paths, extension_to_files, component_to_files)
}

#[test]
fn round_trip_preserves_paths_and_bitmaps() {
    let index = PathIndex::build(&[
        PathBuf::from("src/lib.rs"),
        PathBuf::from("src/main.rs"),
        PathBuf::from("docs/readme.md"),
    ]);
    let bytes = encode(&index);
    let decoded = decode_for_test(&bytes);

    assert_eq!(decoded.paths, index.paths);
    assert_eq!(
        decoded.file_id(Path::new("src/main.rs")),
        index.file_id(Path::new("src/main.rs"))
    );
    assert_eq!(
        decoded.files_with_extension("rs").cloned(),
        index.files_with_extension("rs").cloned()
    );
    assert_eq!(
        decoded.files_with_component("src").cloned(),
        index.files_with_component("src").cloned()
    );
}

#[test]
fn round_trip_empty_index() {
    let index = PathIndex::build(&[]);
    let bytes = encode(&index);
    let decoded = decode_for_test(&bytes);
    assert!(decoded.paths.is_empty());
}

#[test]
fn write_paths_idx_creates_file_with_valid_checksum() {
    let dir = tempfile::tempdir().unwrap();
    let index = PathIndex::build(&[PathBuf::from("a.rs"), PathBuf::from("b/c.py")]);
    write_paths_idx(dir.path(), &index).unwrap();

    let on_disk = std::fs::read(dir.path().join(PATHS_IDX_FILENAME)).unwrap();
    let decoded = decode_for_test(&on_disk);
    assert_eq!(decoded.paths, index.paths);
    // No leftover tmp files after the atomic rename.
    let leftover_tmp = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
    assert!(!leftover_tmp, "tmp file should be renamed away");
}

#[test]
fn read_paths_idx_round_trips_through_write() {
    let dir = tempfile::tempdir().unwrap();
    let index = PathIndex::build(&[
        PathBuf::from("src/lib.rs"),
        PathBuf::from("src/main.rs"),
        PathBuf::from("docs/readme.md"),
    ]);
    write_paths_idx(dir.path(), &index).unwrap();

    let decoded = read_paths_idx(dir.path()).expect("valid sidecar must decode");
    assert_eq!(decoded.paths, index.paths);
    assert_eq!(
        decoded.file_id(Path::new("src/main.rs")),
        index.file_id(Path::new("src/main.rs"))
    );
    assert_eq!(
        decoded.files_with_extension("rs").cloned(),
        index.files_with_extension("rs").cloned()
    );
}

#[test]
fn read_paths_idx_missing_file_is_io_error() {
    let dir = tempfile::tempdir().unwrap();
    match read_paths_idx(dir.path()).map(|_| ()) {
        Err(SidecarError::Io(_)) => {}
        other => panic!("expected Io error for missing file, got {other:?}"),
    }
}

#[test]
fn read_paths_idx_flipped_byte_fails_checksum() {
    let dir = tempfile::tempdir().unwrap();
    let index = PathIndex::build(&[PathBuf::from("a.rs"), PathBuf::from("b/c.py")]);
    write_paths_idx(dir.path(), &index).unwrap();

    let sidecar_path = dir.path().join(PATHS_IDX_FILENAME);
    let mut bytes = std::fs::read(&sidecar_path).unwrap();
    // Flip a byte well inside the body (past the fixed header) so this
    // exercises the checksum check, not just a header sanity check.
    let flip_at = HEADER_LEN + bytes.len().saturating_sub(HEADER_LEN) / 2;
    bytes[flip_at] ^= 0xFF;
    std::fs::write(&sidecar_path, &bytes).unwrap();

    match read_paths_idx(dir.path()).map(|_| ()) {
        Err(SidecarError::ChecksumMismatch) => {}
        other => panic!("expected ChecksumMismatch, got {other:?}"),
    }
}

#[test]
fn read_paths_idx_bad_magic_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(PATHS_IDX_FILENAME), b"NOPE0000truncated").unwrap();
    match read_paths_idx(dir.path()).map(|_| ()) {
        Err(SidecarError::BadMagic) => {}
        other => panic!("expected BadMagic, got {other:?}"),
    }
}

#[test]
fn read_paths_idx_truncated_body_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let index = PathIndex::build(&[PathBuf::from("a.rs"), PathBuf::from("b/c.py")]);
    let bytes = encode(&index);
    // Truncate mid-body: shorter than the full payload but past the header,
    // so this exercises the bounds-checked reader, not the length check.
    let truncated = &bytes[..bytes.len() - 3];
    std::fs::write(dir.path().join(PATHS_IDX_FILENAME), truncated).unwrap();

    match read_paths_idx(dir.path()).map(|_| ()) {
        Err(SidecarError::ChecksumMismatch) | Err(SidecarError::Truncated) => {}
        other => panic!("expected ChecksumMismatch or Truncated, got {other:?}"),
    }
}

#[test]
fn read_paths_idx_unsupported_version_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let index = PathIndex::build(&[PathBuf::from("a.rs")]);
    let mut bytes = encode(&index);
    // Bump the version field without recomputing the checksum: a real
    // future-format writer would also change the body layout, but this
    // is enough to prove the version gate fires before checksum/body
    // parsing would otherwise reject it for a different reason.
    bytes[4..8].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
    std::fs::write(dir.path().join(PATHS_IDX_FILENAME), &bytes).unwrap();

    match read_paths_idx(dir.path()).map(|_| ()) {
        Err(SidecarError::UnsupportedVersion(v)) => assert_eq!(v, FORMAT_VERSION + 1),
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
}
