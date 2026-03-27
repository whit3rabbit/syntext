//! Unit tests for overlay: incremental updates, batch atomicity, snapshot isolation.
//!
//! T043: single file add, modify, delete, batch atomicity, snapshot isolation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ripline_rs::index::overlay::{compute_delete_set, OverlayView};

/// Helper: build dirty file list with Arc<[u8]> content.
fn dirty(files: &[(&str, &[u8])]) -> Vec<(String, Arc<[u8]>)> {
    files
        .iter()
        .map(|(p, c)| (p.to_string(), Arc::from(*c)))
        .collect()
}

// ---------------------------------------------------------------------------
// OverlayView::build tests
// ---------------------------------------------------------------------------

#[test]
fn overlay_single_file_add() {
    let overlay = OverlayView::build(10, dirty(&[("src/main.rs", b"fn parse_query() { }")]));

    assert_eq!(overlay.docs.len(), 1);
    assert_eq!(overlay.docs[0].doc_id, 10); // starts after base range
    assert_eq!(overlay.docs[0].path, "src/main.rs");
    assert!(!overlay.gram_index.is_empty(), "overlay should have grams");
}

#[test]
fn overlay_multiple_files() {
    let overlay = OverlayView::build(
        5,
        dirty(&[("a.rs", b"fn alpha() {}"), ("b.rs", b"fn beta() {}")]),
    );

    assert_eq!(overlay.docs.len(), 2);
    assert_eq!(overlay.docs[0].doc_id, 5);
    assert_eq!(overlay.docs[1].doc_id, 6);
    assert_eq!(overlay.next_doc_id, 7);
}

#[test]
fn overlay_empty() {
    let overlay = OverlayView::build(100, vec![]);
    assert!(overlay.docs.is_empty());
    assert!(overlay.gram_index.is_empty());
    assert_eq!(overlay.next_doc_id, 100);
}

#[test]
fn overlay_doc_lookup_by_id() {
    let overlay = OverlayView::build(0, dirty(&[("test.rs", b"hello world")]));

    assert!(overlay.get_doc(0).is_some());
    assert!(overlay.get_doc(1).is_none());
}

#[test]
fn overlay_doc_lookup_by_path() {
    let overlay = OverlayView::build(0, dirty(&[("a.rs", b"aaa"), ("b.rs", b"bbb")]));

    assert!(overlay.get_doc_by_path("a.rs").is_some());
    assert!(overlay.get_doc_by_path("b.rs").is_some());
    assert!(overlay.get_doc_by_path("c.rs").is_none());
}

// ---------------------------------------------------------------------------
// Modify semantics: rebuild overlay replaces old content
// ---------------------------------------------------------------------------

#[test]
fn overlay_rebuild_replaces_content() {
    // First version
    let ov1 = OverlayView::build(10, dirty(&[("file.rs", b"fn old_function() {}")]));
    let grams_v1: Vec<u64> = ov1.gram_index.keys().copied().collect();

    // Second version (same file, different content)
    let ov2 = OverlayView::build(10, dirty(&[("file.rs", b"fn new_function() {}")]));
    let grams_v2: Vec<u64> = ov2.gram_index.keys().copied().collect();

    // Content should be updated
    assert_eq!(
        std::str::from_utf8(&ov2.docs[0].content).unwrap(),
        "fn new_function() {}"
    );

    // Grams should differ (old function name grams gone, new ones present)
    assert_ne!(grams_v1, grams_v2, "gram sets should differ after modify");
}

// ---------------------------------------------------------------------------
// Delete semantics: file removed from overlay entirely
// ---------------------------------------------------------------------------

#[test]
fn overlay_delete_removes_file() {
    // Build with a file, then rebuild without it (simulating delete)
    let ov_with = OverlayView::build(10, dirty(&[("file.rs", b"fn something() {}")]));
    assert_eq!(ov_with.docs.len(), 1);

    // After delete, rebuild with empty set
    let ov_without = OverlayView::build(10, vec![]);
    assert_eq!(ov_without.docs.len(), 0);
    assert!(ov_without.gram_index.is_empty());
}

// ---------------------------------------------------------------------------
// Snapshot isolation via Arc
// ---------------------------------------------------------------------------

#[test]
fn snapshot_isolation_via_arc() {
    // Simulate: reader holds old snapshot, writer creates new one.
    let ov1 = Arc::new(OverlayView::build(0, dirty(&[("file.rs", b"version one")])));

    // Reader holds a reference to v1
    let reader_snap = Arc::clone(&ov1);

    // Writer creates v2 (in real code this would be ArcSwap::store)
    let _ov2 = Arc::new(OverlayView::build(0, dirty(&[("file.rs", b"version two")])));

    // Reader still sees v1
    assert_eq!(
        std::str::from_utf8(&reader_snap.docs[0].content).unwrap(),
        "version one"
    );
}

// ---------------------------------------------------------------------------
// build_incremental tests
// ---------------------------------------------------------------------------

#[test]
fn incremental_reuses_unchanged_content() {
    let old = OverlayView::build(
        10,
        dirty(&[("a.rs", b"aaa content"), ("b.rs", b"bbb content")]),
    );
    let old_a_ptr = Arc::as_ptr(&old.docs.iter().find(|d| d.path == "a.rs").unwrap().content);

    // Only b.rs changed; a.rs should be reused via Arc::clone.
    let newly_changed: HashSet<String> = ["b.rs".to_string()].into();
    let removed: HashSet<String> = HashSet::new();
    let new_files = dirty(&[("b.rs", b"bbb updated")]);

    let inc = OverlayView::build_incremental(10, &old, new_files, &newly_changed, &removed);

    assert_eq!(inc.docs.len(), 2);

    // a.rs content should be the same Arc (pointer equality).
    let inc_a = inc.docs.iter().find(|d| d.path == "a.rs").unwrap();
    assert!(
        std::ptr::eq(Arc::as_ptr(&inc_a.content), old_a_ptr),
        "unchanged doc should share Arc, not clone"
    );

    // b.rs content should be updated.
    let inc_b = inc.docs.iter().find(|d| d.path == "b.rs").unwrap();
    assert_eq!(std::str::from_utf8(&inc_b.content).unwrap(), "bbb updated");
}

#[test]
fn incremental_removes_deleted() {
    let old = OverlayView::build(10, dirty(&[("a.rs", b"aaa"), ("b.rs", b"bbb")]));

    let newly_changed: HashSet<String> = HashSet::new();
    let removed: HashSet<String> = ["b.rs".to_string()].into();

    let inc = OverlayView::build_incremental(10, &old, vec![], &newly_changed, &removed);

    assert_eq!(inc.docs.len(), 1);
    assert_eq!(inc.docs[0].path, "a.rs");
    assert!(inc.get_doc_by_path("b.rs").is_none());
}

#[test]
fn incremental_from_empty_old_overlay() {
    let old = OverlayView::empty();
    let newly_changed: HashSet<String> = ["new.rs".to_string()].into();
    let removed: HashSet<String> = HashSet::new();
    let new_files = dirty(&[("new.rs", b"fn new() {}")]);

    let inc = OverlayView::build_incremental(5, &old, new_files, &newly_changed, &removed);

    assert_eq!(inc.docs.len(), 1);
    assert_eq!(inc.docs[0].path, "new.rs");
    assert_eq!(inc.docs[0].doc_id, 5);
}

#[test]
fn compute_delete_set_marks_all_base_docs_for_invalidated_paths() {
    let mut base_path_doc_ids = HashMap::new();
    base_path_doc_ids.insert("src/main.rs".to_string(), vec![1, 7]);
    base_path_doc_ids.insert("src/lib.rs".to_string(), vec![3]);

    let delete_set = compute_delete_set(
        &base_path_doc_ids,
        &["src/main.rs".to_string()],
        &["src/missing.rs".to_string()],
    );

    assert!(delete_set.contains(1));
    assert!(delete_set.contains(7));
    assert!(!delete_set.contains(3));
}

#[test]
fn overlay_build_stores_base_doc_count() {
    let ov = OverlayView::build(42, dirty(&[("a.rs", b"fn a() {}")]));
    assert_eq!(ov.base_doc_count, 42);
}

#[test]
fn overlay_empty_base_doc_count_is_zero() {
    let ov = OverlayView::empty();
    assert_eq!(ov.base_doc_count, 0);
}

#[test]
#[should_panic(expected = "doc_id overflow")]
fn overlay_build_panics_on_doc_id_overflow() {
    // base_doc_count near u32::MAX means the first += 1 would overflow.
    let _ = OverlayView::build(u32::MAX, dirty(&[("a.rs", b"fn a() {}")]));
}

#[test]
fn incremental_reuses_cached_grams() {
    let old = OverlayView::build(10, dirty(&[("a.rs", b"fn alpha_function() {}")]));

    // Capture gram set from old overlay.
    let old_a_grams: Vec<u64> = {
        let doc = old.docs.iter().find(|d| d.path == "a.rs").unwrap();
        doc.grams.clone()
    };
    assert!(!old_a_grams.is_empty(), "should have grams");

    // Incremental: only b.rs is new; a.rs should reuse cached grams.
    let newly_changed: HashSet<String> = ["b.rs".to_string()].into();
    let removed: HashSet<String> = HashSet::new();
    let new_files = dirty(&[("b.rs", b"fn beta() {}")]);

    let inc = OverlayView::build_incremental(10, &old, new_files, &newly_changed, &removed);

    let inc_a = inc.docs.iter().find(|d| d.path == "a.rs").unwrap();
    assert_eq!(
        inc_a.grams, old_a_grams,
        "reused doc should have same cached grams"
    );
}
