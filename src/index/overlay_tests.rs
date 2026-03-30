    use super::*;
    use std::collections::HashSet;

    #[test]
    fn incremental_delta_posting_lists_are_sorted() {
        let content_a: Arc<[u8]> = Arc::from(b"fn alpha_one() {}".as_slice());
        let content_b: Arc<[u8]> = Arc::from(b"fn beta_two() {}".as_slice());
        let overlay1 = OverlayView::build(
            0,
            vec![
                (PathBuf::from("a.rs"), Arc::clone(&content_a)),
                (PathBuf::from("b.rs"), Arc::clone(&content_b)),
            ],
        )
        .unwrap();

        let content_a2: Arc<[u8]> = Arc::from(b"fn alpha_changed() {}".as_slice());
        let newly_changed: HashSet<PathBuf> = [PathBuf::from("a.rs")].into();
        let removed: HashSet<PathBuf> = HashSet::new();

        let overlay2 = OverlayView::build_incremental(
            0,
            &overlay1,
            vec![(PathBuf::from("a.rs"), content_a2)],
            &newly_changed,
            &removed,
        )
        .unwrap();

        for (hash, ids) in &overlay2.gram_index {
            assert!(
                ids.windows(2).all(|w| w[0] < w[1]),
                "gram {hash:#x} posting list is not strictly sorted: {ids:?}"
            );
        }
    }

    #[test]
    fn build_incremental_no_underflow() {
        // B05: overlay_docs count uses saturating_sub so that degenerate inputs
        // (newly_changed.len() > old.len() + new_files.len()) produce 0, not
        // usize::MAX. Using base_doc_count=1 != old.base_doc_count=0 forces
        // the full rebuild path rather than the fast delta path.
        let content_a: Arc<[u8]> = Arc::from(b"fn alpha() {}".as_slice());
        let overlay1 = OverlayView::build(
            0,
            vec![(PathBuf::from("a.rs"), Arc::clone(&content_a))],
        )
        .unwrap();
        assert_eq!(overlay1.docs.len(), 1);

        // old has 1 doc, new_files is empty, but newly_changed has 2 paths.
        // (1 + 0).saturating_sub(2) = 0, not usize::MAX.
        let newly_changed: HashSet<PathBuf> =
            [PathBuf::from("a.rs"), PathBuf::from("ghost.rs")].into();
        let removed: HashSet<PathBuf> = HashSet::new();

        let result = OverlayView::build_incremental(
            1, // different from old.base_doc_count=0 → full rebuild, not delta
            &overlay1,
            vec![],
            &newly_changed,
            &removed,
        );
        assert!(result.is_ok(), "must not panic or error");
        assert_eq!(
            result.unwrap().docs.len(),
            0,
            "all old docs are in newly_changed with no replacements → empty overlay"
        );
    }
