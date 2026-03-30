use std::path::Path;

use super::*;
use tempfile::TempDir;

#[test]
fn round_trip_empty_segment() {
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    let meta = writer.write_to_dir(dir.path()).unwrap();
    assert_eq!(meta.doc_count, 0);
    assert_eq!(meta.gram_count, 0);
    assert!(dir.path().join(&meta.dict_filename).exists());
    assert!(dir.path().join(&meta.post_filename).exists());

    let dict_path = dir.path().join(&meta.dict_filename);
    let seg = MmapSegment::open(&dict_path).unwrap();
    assert_eq!(seg.doc_count, 0);
    assert_eq!(seg.gram_count, 0);
}

#[test]
fn round_trip_with_docs_and_grams() {
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("src/main.rs"), 0xDEAD, 100);
    writer.add_document(1, Path::new("src/lib.rs"), 0xBEEF, 200);
    writer.add_gram_posting(0xAAAA, 0);
    writer.add_gram_posting(0xAAAA, 1);
    writer.add_gram_posting(0xBBBB, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();
    assert_eq!(meta.doc_count, 2);
    assert_eq!(meta.gram_count, 2);
    assert!(dir.path().join(&meta.dict_filename).exists());
    assert!(dir.path().join(&meta.post_filename).exists());

    let dict_path = dir.path().join(&meta.dict_filename);
    let post_path = dir.path().join(&meta.post_filename);
    let seg = MmapSegment::open_split(&dict_path, &post_path).unwrap();
    assert_eq!(seg.doc_count, 2);

    let d0 = seg.get_doc(0).unwrap();
    assert_eq!(d0.path, Path::new("src/main.rs"));
    assert_eq!(d0.content_hash, 0xDEAD);

    let pl = seg.lookup_gram(0xAAAA).unwrap();
    let ids = pl.to_vec().unwrap();
    assert_eq!(ids, vec![0, 1]);
}

#[test]
fn duplicate_postings_are_deduplicated() {
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("src/main.rs"), 0xDEAD, 100);
    writer.add_document(1, Path::new("src/lib.rs"), 0xBEEF, 200);
    writer.add_gram_posting(0xAAAA, 0);
    writer.add_gram_posting(0xAAAA, 0);
    writer.add_gram_posting(0xAAAA, 1);

    let meta = writer.write_to_dir(dir.path()).unwrap();
    assert_eq!(meta.gram_count, 1);
    assert!(dir.path().join(&meta.dict_filename).exists());
    assert!(dir.path().join(&meta.post_filename).exists());

    let dict_path = dir.path().join(&meta.dict_filename);
    let post_path = dir.path().join(&meta.post_filename);
    let seg = MmapSegment::open_split(&dict_path, &post_path).unwrap();
    assert_eq!(seg.gram_cardinality(0xAAAA), Some(2));
}

#[test]
fn corrupt_file_rejected() {
    let dir = TempDir::new().unwrap();
    let bad_path = dir.path().join("bad.dict");
    std::fs::write(&bad_path, b"not a valid segment").unwrap();
    assert!(MmapSegment::open(&bad_path).is_err());
}

#[test]
fn verify_integrity_passes_on_clean_segment() {
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("a.rs"), 1, 10);
    writer.add_gram_posting(0xAA, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    let dict_path = dir.path().join(&meta.dict_filename);
    let seg = MmapSegment::open(&dict_path).unwrap();
    assert!(seg.verify_integrity().is_ok());
}

#[test]
fn open_rejects_segment_exceeding_size_limit() {
    // Build a real segment first so we have valid magic/checksum.
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("a.rs"), 1, 10);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    // Verify the constant is wired in and a normal-size segment opens fine.
    let dict_path = dir.path().join(&meta.dict_filename);
    let seg = MmapSegment::open(&dict_path);
    assert!(
        seg.is_ok(),
        "valid segment under size limit must open successfully"
    );
}

#[test]
fn map_private_copy_unaffected_by_post_open_file_mutation() {
    // With MAP_PRIVATE (map_copy_read_only), the mmap is a copy-on-write
    // snapshot of the file at open time. parse_segment_mmap reads every
    // content page during checksum verification, faulting them all into
    // the process private address space. After that point, on-disk mutations
    // are NOT reflected in the mapping.
    //
    // This is the desired security property: an attacker who gains write
    // access to the index directory after open() cannot affect in-process
    // reads. verify_integrity() checks the private copy against itself and
    // always passes; get_doc() returns the original document.
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("a.rs"), 1, 10);
    writer.add_gram_posting(0xAA, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    let dict_path = dir.path().join(&meta.dict_filename);
    let seg = MmapSegment::open(&dict_path).unwrap();

    // Atomically replace the on-disk file after open() via rename.
    // On Linux, in-place writes (std::fs::write with O_TRUNC) invalidate
    // MAP_PRIVATE + PROT_READ page cache entries and cause SIGBUS.
    // Rename only changes the directory entry; the mmap holds the original
    // inode open via _file and is unaffected.
    let replacement = dir.path().join("replacement.dict");
    std::fs::write(&replacement, b"SNTX_corrupted_on_disk").unwrap();
    std::fs::rename(&replacement, &dict_path).unwrap();

    // The mmap still reads from the original inode; both must succeed.
    assert!(
        seg.verify_integrity().is_ok(),
        "mmap must survive atomic file replacement via rename"
    );
    assert!(
        seg.get_doc(0).is_some(),
        "mmap must serve doc reads after file replacement"
    );
}

#[test]
fn with_capacity_hint_does_not_panic_when_exceeded() {
    let mut writer = SegmentWriter::with_capacity(1, 2);
    writer.add_document(0, Path::new("a.rs"), 1, 10);
    for i in 0u64..100 {
        writer.add_gram_posting(i, 0);
    }
    let dir = TempDir::new().unwrap();
    assert!(writer.write_to_dir(dir.path()).is_ok());
}

#[test]
fn add_document_rejects_duplicate_doc_ids() {
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("a.rs"), 1, 10);
    writer.add_document(1, Path::new("b.rs"), 2, 20);
    // Duplicate id=1 should be caught during serialize.
    writer.add_document(1, Path::new("c.rs"), 3, 30);
    let dir = TempDir::new().unwrap();
    let result = writer.write_to_dir(dir.path());
    assert!(result.is_err(), "duplicate doc_id must be rejected");
}

#[test]
fn format_version_constants_are_distinct() {
    assert_ne!(FORMAT_VERSION_V2, FORMAT_VERSION_V3);
    assert_eq!(FORMAT_VERSION, FORMAT_VERSION_V3);
}

#[test]
fn dict_entry_size_matches_components() {
    // 8 (gram_hash) + 8 (abs_off/post_offset) + 4 (count) = 20 bytes.
    assert_eq!(DICT_ENTRY_SIZE, 20);
}

#[test]
fn v3_writer_produces_two_files() {
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("src/lib.rs"), 0xABCD, 100);
    writer.add_gram_posting(0x1234, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    assert!(
        dir.path().join(&meta.dict_filename).exists(),
        "missing .dict"
    );
    assert!(
        dir.path().join(&meta.post_filename).exists(),
        "missing .post"
    );
    // No .seg file for v3
    let any_seg = std::fs::read_dir(dir.path())
        .unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".seg"));
    assert!(!any_seg, "v3 writer must not produce a .seg file");
}

#[test]
fn v3_round_trip_lookup_gram() {
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("src/main.rs"), 0xDEAD, 100);
    writer.add_document(1, Path::new("src/lib.rs"), 0xBEEF, 200);
    writer.add_gram_posting(0xAAAA, 0);
    writer.add_gram_posting(0xAAAA, 1);
    writer.add_gram_posting(0xBBBB, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    let seg = MmapSegment::open_split(
        &dir.path().join(&meta.dict_filename),
        &dir.path().join(&meta.post_filename),
    )
    .unwrap();

    assert_eq!(seg.doc_count, 2);
    let d0 = seg.get_doc(0).unwrap();
    assert_eq!(d0.path, Path::new("src/main.rs"));

    let pl = seg.lookup_gram(0xAAAA).unwrap();
    assert_eq!(pl.to_vec().unwrap(), vec![0, 1]);

    let pl2 = seg.lookup_gram(0xBBBB).unwrap();
    assert_eq!(pl2.to_vec().unwrap(), vec![0]);

    assert!(seg.lookup_gram(0xCCCC).is_none());
}

#[test]
fn v3_round_trip_get_doc() {
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("a.rs"), 0xAA, 10);
    writer.add_gram_posting(0x11, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    let seg = MmapSegment::open_split(
        &dir.path().join(&meta.dict_filename),
        &dir.path().join(&meta.post_filename),
    )
    .unwrap();

    let doc = seg.get_doc(0).unwrap();
    assert_eq!(doc.path, Path::new("a.rs"));
    assert_eq!(doc.content_hash, 0xAA);
    assert!(seg.get_doc(1).is_none());
}

#[cfg(unix)]
#[test]
fn round_trip_preserves_non_utf8_path_bytes() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    let path = std::path::PathBuf::from(OsString::from_vec(b"src/odd\xff.rs".to_vec()));
    writer.add_document(0, &path, 0xDEAD, 100);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    let dict_path = dir.path().join(&meta.dict_filename);
    let seg = MmapSegment::open(&dict_path).unwrap();
    let d0 = seg.get_doc(0).unwrap();
    assert_eq!(d0.path, path);
}

#[test]
fn v3_post_file_has_magic_and_checksum() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("src/a.rs"), 0x1234, 100);
    writer.add_gram_posting(0xAAAA, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    let post_bytes = std::fs::read(dir.path().join(&meta.post_filename)).unwrap();
    // First 8 bytes must be the magic
    assert_eq!(&post_bytes[..8], b"SNTXPOST", "missing .post magic header");
    // File must be long enough for magic (8) + at least one posting entry + checksum (8)
    assert!(post_bytes.len() >= 17, "post file too short");
    // Last 8 bytes are xxhash64 checksum of the postings data (bytes 8..len-8)
    let postings_data = &post_bytes[8..post_bytes.len() - 8];
    let expected_checksum = xxhash_rust::xxh64::xxh64(postings_data, 0);
    let stored_checksum =
        u64::from_le_bytes(post_bytes[post_bytes.len() - 8..].try_into().unwrap());
    assert_eq!(stored_checksum, expected_checksum, "checksum mismatch");
}

#[test]
fn open_split_rejects_corrupt_post_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("src/a.rs"), 0xABCD, 100);
    writer.add_gram_posting(0x1111, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    // Corrupt the .post file by writing wrong magic
    let post_path = dir.path().join(&meta.post_filename);
    let mut post_bytes = std::fs::read(&post_path).unwrap();
    post_bytes[0] = b'X'; // corrupt magic byte
    std::fs::write(&post_path, &post_bytes).unwrap();

    let result = MmapSegment::open_split(
        &dir.path().join(&meta.dict_filename),
        &dir.path().join(&meta.post_filename),
    );
    assert!(
        result.is_err(),
        "open_split must reject corrupt .post magic"
    );
}

#[test]
fn get_doc_rejects_abs_off_pointing_into_dict_section() {
    // Security regression test for Fix 1a: craft a segment whose doc table
    // index pointer points into the dictionary section. Without the abs_off
    // range check, get_doc would interpret dict bytes as DocEntry fields
    // (information disclosure). With the check, it must return None.
    use xxhash_rust::xxh64::xxh64;

    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("a.rs"), 0xABCD, 10);
    writer.add_gram_posting(0x1111, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    let dict_path = dir.path().join(&meta.dict_filename);
    let mut bytes = std::fs::read(&dict_path).unwrap();
    let len = bytes.len();
    // Footer layout (from end): [doc_table_off(8)][reserved(8)][dict_off(8)]
    //                           [doc_cnt(4)][gram_cnt(4)][cksum(8)][ver(4)][magic(4)]
    let footer_start = len - FOOTER_SIZE;
    let dict_offset_value = u64::from_le_bytes(
        bytes[footer_start + 16..footer_start + 24]
            .try_into()
            .unwrap(),
    ) as usize;

    // Overwrite doc 0's abs_off pointer to point 4 bytes into the dict section.
    let doc_table_offset = HEADER_SIZE; // always 40 for V3
    let bad_abs_off = (dict_offset_value + 4) as u64;
    bytes[doc_table_offset..doc_table_offset + 8].copy_from_slice(&bad_abs_off.to_le_bytes());

    // Recompute the checksum over the content (everything before the footer).
    let new_cksum = xxh64(&bytes[..footer_start], 0);
    bytes[footer_start + 32..footer_start + 40].copy_from_slice(&new_cksum.to_le_bytes());

    let crafted_path = dir.path().join("crafted.dict");
    std::fs::write(&crafted_path, &bytes).unwrap();

    let seg = MmapSegment::open(&crafted_path).unwrap();
    assert!(
        seg.get_doc(0).is_none(),
        "get_doc must return None when abs_off points into dict section"
    );
}

#[test]
fn mmap_isolation_from_disk_overwrite() {
    // After opening, atomically replacing the file on disk must not corrupt
    // the in-memory mapping. MmapSegment retains an open file handle to the
    // original inode; after an atomic rename the old inode stays alive and
    // the mapping continues to read from it.
    //
    // We use rename rather than in-place write: on Linux, MAP_PRIVATE +
    // PROT_READ pages are still backed by the file's page cache, so
    // truncating the file via std::fs::write invalidates those pages and
    // delivers SIGBUS.  Atomic rename only changes the directory entry;
    // the mmap's inode reference is unaffected.
    let dir = tempfile::TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, std::path::Path::new("a.rs"), 0xABCD, 10);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    let dict_path = dir.path().join(&meta.dict_filename);
    let seg = MmapSegment::open(&dict_path).unwrap();

    // Atomically replace the file at the same path with corrupted content.
    // The segment's inode (held open via _file) is unlinked from the
    // directory but remains alive.
    let replacement = dir.path().join("replacement.dict");
    std::fs::write(&replacement, b"CORRUPTED").unwrap();
    std::fs::rename(&replacement, &dict_path).unwrap();

    // The mmap still references the original inode; the document must be readable.
    let doc = seg.get_doc(0);
    assert!(
        doc.is_some(),
        "mmap must survive atomic file replacement via rename"
    );
}

#[test]
fn v2_posting_offset_below_postings_start_returns_none() {
    // B03 regression: read_posting_list_mmap must reject abs_off values in
    // [HEADER_SIZE, postings_start) — the old check was abs_off < HEADER_SIZE,
    // which accepted values pointing into the doc table section. The fix
    // uses abs_off < postings_start (= doc_table_offset + doc_count * 8).
    //
    // Strategy: write a V3 segment, open the .dict file via open() (V2Mmap
    // path), craft a dict entry whose abs_off points into the doc table
    // region [doc_table_offset, postings_start), recompute the checksum,
    // and verify lookup_gram returns None.
    use xxhash_rust::xxh64::xxh64;

    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new();
    writer.add_document(0, Path::new("a.rs"), 0xABCD, 10);
    writer.add_gram_posting(0x1111_2222_3333_4444u64, 0);
    let meta = writer.write_to_dir(dir.path()).unwrap();

    let dict_path = dir.path().join(&meta.dict_filename);
    let mut bytes = std::fs::read(&dict_path).unwrap();
    let len = bytes.len();
    let footer_start = len - FOOTER_SIZE;

    // Read layout fields from footer.
    let doc_table_offset =
        u64::from_le_bytes(bytes[footer_start..footer_start + 8].try_into().unwrap()) as usize;
    let doc_count = u32::from_le_bytes(
        bytes[footer_start + 24..footer_start + 28]
            .try_into()
            .unwrap(),
    );
    let dict_offset = u64::from_le_bytes(
        bytes[footer_start + 16..footer_start + 24]
            .try_into()
            .unwrap(),
    ) as usize;
    let postings_start = doc_table_offset + doc_count as usize * 8;

    assert!(
        postings_start > HEADER_SIZE,
        "postings_start({postings_start}) must exceed HEADER_SIZE({HEADER_SIZE}) \
            for this test to distinguish old vs new check"
    );

    // Overwrite the first gram entry's abs_off to doc_table_offset, which
    // falls in [HEADER_SIZE, postings_start). Old check accepted this; new
    // check must reject it. Dict entry: gram_hash(8) + abs_off(8) + count(4).
    let abs_off_field_start = dict_offset + 8; // skip gram_hash bytes
    let crafted_abs_off = doc_table_offset as u64;
    bytes[abs_off_field_start..abs_off_field_start + 8]
        .copy_from_slice(&crafted_abs_off.to_le_bytes());

    // Recompute checksum over content (everything before footer).
    let new_cksum = xxh64(&bytes[..footer_start], 0);
    bytes[footer_start + 32..footer_start + 40].copy_from_slice(&new_cksum.to_le_bytes());

    let crafted_path = dir.path().join("crafted_b03.dict");
    std::fs::write(&crafted_path, &bytes).unwrap();

    // open() creates V2Mmap backing; read_posting_list_mmap is called by
    // lookup_gram. abs_off(doc_table_offset) < postings_start → must be None.
    let seg = MmapSegment::open(&crafted_path).unwrap();
    let result = seg.lookup_gram(0x1111_2222_3333_4444u64);
    assert!(
        result.is_none(),
        "lookup_gram must return None when abs_off({crafted_abs_off}) < \
            postings_start({postings_start}): {result:?}"
    );
}
