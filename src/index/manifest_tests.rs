use super::*;

#[test]
fn load_rejects_oversized_manifest() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join(Manifest::FILENAME);
    let data = "x".repeat(11 * 1024 * 1024);
    std::fs::write(&path, data).unwrap();

    let result = Manifest::load(dir.path());
    assert!(result.is_err(), "should reject manifest > 10MB");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("too large"),
        "error should mention size: {err_msg}"
    );
}

#[test]
fn load_accepts_normal_manifest() {
    let dir = tempfile::TempDir::new().unwrap();
    let manifest = Manifest::new(vec![], 0);
    manifest.save(dir.path()).unwrap();

    let loaded = Manifest::load(dir.path()).unwrap();
    assert_eq!(loaded.total_docs(), 0);
}

#[test]
fn roundtrip_preserves_scan_threshold() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut manifest = Manifest::new(vec![], 0);
    manifest.scan_threshold_fraction = Some(0.23);
    manifest.save(dir.path()).unwrap();

    let loaded = Manifest::load(dir.path()).unwrap();
    assert_eq!(
        loaded.scan_threshold_fraction,
        Some(0.23),
        "scan_threshold_fraction must round-trip through manifest.json"
    );
}

#[test]
fn missing_threshold_deserializes_as_none() {
    let dir = tempfile::TempDir::new().unwrap();
    // Write a manifest without the field (simulates old index).
    let json = r#"{
        "version": 1,
        "base_commit": null,
        "segments": [],
        "overlay_gen": 0,
        "overlay_file": null,
        "overlay_deletes_file": null,
        "total_files_indexed": 0,
        "created_at": 0,
        "opstamp": 0
    }"#;
    std::fs::write(dir.path().join("manifest.json"), json).unwrap();

    let loaded = Manifest::load(dir.path()).unwrap();
    assert!(
        loaded.scan_threshold_fraction.is_none(),
        "old manifests without the field must deserialize as None"
    );
}

#[test]
fn segment_ref_round_trips_with_post_filename() {
    let dir = tempfile::TempDir::new().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let seg_ref = SegmentRef {
        segment_id: id.clone(),
        base_doc_id: Some(12),
        filename: String::new(),
        dict_filename: format!("{id}.dict"),
        post_filename: format!("{id}.post"),
        doc_count: 5,
        gram_count: 10,
    };
    let manifest = Manifest::new(vec![seg_ref], 5);
    manifest.save(dir.path()).unwrap();
    let loaded = Manifest::load(dir.path()).unwrap();
    assert_eq!(loaded.segments[0].base_doc_id, Some(12));
    assert_eq!(loaded.segments[0].dict_filename, format!("{id}.dict"));
    assert_eq!(loaded.segments[0].post_filename, format!("{id}.post"));
}

#[test]
fn gc_removes_orphan_dict_and_post_files() {
    let dir = tempfile::TempDir::new().unwrap();
    // Create orphaned .dict and .post files
    std::fs::write(dir.path().join("orphan.dict"), b"orphan").unwrap();
    std::fs::write(dir.path().join("orphan.post"), b"orphan").unwrap();
    // Also create referenced files
    std::fs::write(dir.path().join("kept.dict"), b"kept").unwrap();
    std::fs::write(dir.path().join("kept.post"), b"kept").unwrap();

    let manifest = Manifest::new(
        vec![SegmentRef {
            segment_id: "kept".into(),
            base_doc_id: Some(0),
            filename: String::new(),
            dict_filename: "kept.dict".into(),
            post_filename: "kept.post".into(),
            doc_count: 0,
            gram_count: 0,
        }],
        0,
    );
    manifest.gc_orphan_segments(dir.path()).unwrap();

    assert!(
        !dir.path().join("orphan.dict").exists(),
        "orphan .dict must be removed"
    );
    assert!(
        !dir.path().join("orphan.post").exists(),
        "orphan .post must be removed"
    );
    assert!(
        dir.path().join("kept.dict").exists(),
        "referenced .dict must be kept"
    );
    assert!(
        dir.path().join("kept.post").exists(),
        "referenced .post must be kept"
    );
}

#[test]
fn gc_removes_stale_manifest_tmp_files() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("manifest-stale.tmp"), b"stale").unwrap();

    let manifest = Manifest::new(vec![], 0);
    manifest.gc_orphan_segments(dir.path()).unwrap();

    assert!(
        !dir.path().join("manifest-stale.tmp").exists(),
        "stale manifest tmp files must be removed"
    );
}

#[test]
fn save_leaves_no_tmp_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let manifest = Manifest::new(vec![], 0);
    manifest.save(dir.path()).unwrap();

    let tmp_count = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(tmp_count, 0, "save() must not leave tmp files behind");
}

#[test]
fn v2_manifest_without_split_filenames_deserializes_cleanly() {
    let dir = tempfile::TempDir::new().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    // Simulate a v2 manifest with no dict_filename / post_filename fields
    let json = format!(
        r#"{{
        "version": 1,
        "base_commit": null,
        "segments": [
            {{
                "segment_id": "{id}",
                "filename": "{id}.seg",
                "doc_count": 10,
                "gram_count": 50
            }}
        ],
        "overlay_gen": 0,
        "overlay_file": null,
        "overlay_deletes_file": null,
        "total_files_indexed": 10,
        "created_at": 0,
        "opstamp": 0
    }}"#
    );
    std::fs::write(dir.path().join("manifest.json"), json).unwrap();
    let loaded = Manifest::load(dir.path()).unwrap();
    assert_eq!(loaded.segments[0].filename, format!("{id}.seg"));
    assert_eq!(loaded.segments[0].dict_filename, ""); // defaults to empty
    assert_eq!(loaded.segments[0].post_filename, ""); // defaults to empty
}

#[test]
fn manifest_rejects_non_uuid_segment_id() {
    let dir = tempfile::TempDir::new().unwrap();
    let seg_ref = SegmentRef {
        segment_id: "../../etc/passwd".into(),
        base_doc_id: None,
        filename: String::new(),
        dict_filename: "a.dict".into(),
        post_filename: "a.post".into(),
        doc_count: 1,
        gram_count: 1,
    };
    let manifest = Manifest::new(vec![seg_ref], 1);
    manifest.save(dir.path()).unwrap();

    let result = Manifest::load(dir.path());
    assert!(result.is_err(), "non-UUID segment_id must be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not a valid UUID"),
        "error should mention UUID: {err_msg}"
    );
}
