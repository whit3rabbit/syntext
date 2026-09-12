use super::*;
use std::io::Cursor;
use tempfile::NamedTempFile;

#[test]
fn digest_from_bytes_matches_from_reader() {
    let data = b"hello syntext changes";
    let d1 = ContentDigest::from_bytes(data);
    let mut cursor = Cursor::new(data);
    let d2 = ContentDigest::from_reader(&mut cursor).unwrap();
    assert_eq!(d1, d2);
}

#[test]
fn digest_hex_round_trip() {
    let data = b"hex test payload";
    let digest = ContentDigest::from_bytes(data);
    let hex_str = digest.to_hex();
    assert_eq!(hex_str.len(), 64);
    let parsed = ContentDigest::from_hex(&hex_str).unwrap();
    assert_eq!(digest, parsed);
}

#[test]
fn racy_margin_settlement() {
    let now = SystemTime::now();
    let old_epoch = now - Duration::from_secs(10);
    let (secs, nanos) = split_system_time(old_epoch).unwrap();

    let fp = FileFingerprint::new(ContentDigest::default(), 100, secs, nanos);
    // When read_epoch is now, old_epoch is 10s earlier > 2s margin -> settled.
    assert!(fp.is_settled(now, now));

    // When read_epoch is only 1s after mtime -> not settled.
    let tight_read = old_epoch + Duration::from_secs(1);
    assert!(!fp.is_settled(tight_read, now));

    // Future mtime -> not settled.
    let future = now + Duration::from_secs(5);
    let (f_secs, f_nanos) = split_system_time(future).unwrap();
    let fp_future = FileFingerprint::new(ContentDigest::default(), 100, f_secs, f_nanos);
    assert!(!fp_future.is_settled(now, now));
}

#[test]
fn hash_file_retains_buffer_when_under_cap() {
    let mut tmp = NamedTempFile::new().unwrap();
    use std::io::Write;
    tmp.write_all(b"small file payload").unwrap();
    tmp.flush().unwrap();

    let res = hash_file(tmp.path(), 1024).unwrap();
    assert_eq!(res.fingerprint.size, 18);
    assert!(res.buffer.is_some());
    assert_eq!(res.buffer.unwrap().as_ref(), b"small file payload");

    // When max_retain_bytes is smaller than file size, buffer is None.
    let res_no_buf = hash_file(tmp.path(), 10).unwrap();
    assert_eq!(res_no_buf.fingerprint.size, 18);
    assert!(res_no_buf.buffer.is_none());
    assert_eq!(res.fingerprint.digest, res_no_buf.fingerprint.digest);
}
