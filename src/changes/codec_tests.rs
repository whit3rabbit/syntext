use super::*;
use tempfile::tempdir;

#[test]
fn codec_round_trip() {
    let dir = tempdir().unwrap();
    let cat_path = dir.path().join("catalogue.idx");

    let mut entries = HashMap::new();
    let p1 = PathBuf::from("src/main.rs");
    let fp1 = FileFingerprint::new(ContentDigest::from_bytes(b"main content"), 12, 1000, 50);
    entries.insert(
        p1.clone(),
        CatalogueEntry {
            fingerprint: fp1,
            observed_generation: 1,
        },
    );

    let p2 = PathBuf::from("docs/README.md");
    let fp2 = FileFingerprint::new(ContentDigest::from_bytes(b"readme"), 6, 2000, 100);
    entries.insert(
        p2.clone(),
        CatalogueEntry {
            fingerprint: fp2,
            observed_generation: 2,
        },
    );

    let mut consumers = HashMap::new();
    consumers.insert("synrepo".to_string(), 1);
    consumers.insert("test-consumer".to_string(), 2);

    write_catalogue(&cat_path, 3, &entries, &consumers).unwrap();

    let decoded = read_catalogue(&cat_path).unwrap().expect("should exist");
    assert_eq!(decoded.generation, 3);
    assert_eq!(decoded.entries.len(), 2);
    assert_eq!(decoded.entries[&p1].fingerprint, fp1);
    assert_eq!(decoded.entries[&p1].observed_generation, 1);
    assert_eq!(decoded.entries[&p2].fingerprint, fp2);
    assert_eq!(decoded.entries[&p2].observed_generation, 2);
    assert_eq!(decoded.consumers["synrepo"], 1);
    assert_eq!(decoded.consumers["test-consumer"], 2);
}

#[test]
fn corrupted_checksum_fails() {
    let dir = tempdir().unwrap();
    let cat_path = dir.path().join("catalogue.idx");

    let mut entries = HashMap::new();
    let p1 = PathBuf::from("foo.rs");
    entries.insert(
        p1,
        CatalogueEntry {
            fingerprint: FileFingerprint::new(ContentDigest::default(), 0, 0, 0),
            observed_generation: 1,
        },
    );

    write_catalogue(&cat_path, 1, &entries, &HashMap::new()).unwrap();

    // Corrupt one byte in the file
    let mut bytes = fs::read(&cat_path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&cat_path, &bytes).unwrap();

    let err = read_catalogue(&cat_path).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("checksum mismatch"));
}
