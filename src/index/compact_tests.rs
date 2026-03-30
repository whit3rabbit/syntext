    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use roaring::RoaringBitmap;
    use tempfile::TempDir;

    use super::*;
    use crate::index::overlay::OverlayView;
    use crate::index::segment::MmapSegment;
    use crate::index::snapshot::{new_snapshot, BaseSegments};
    use crate::path::PathIndex;

    fn build_snapshot(
        segments: &[Vec<(u32, &'static str, u64)>],
        overlay: OverlayView,
        delete_set: RoaringBitmap,
    ) -> (TempDir, IndexSnapshot, Vec<SegmentRef>) {
        let dir = TempDir::new().unwrap();
        let mut mmap_segments = Vec::new();
        let mut seg_refs = Vec::new();
        let mut base_ids = Vec::new();
        let mut base_doc_paths: Vec<Option<PathBuf>> = Vec::new();
        let mut path_doc_ids: HashMap<PathBuf, Vec<u32>> = HashMap::new();
        let mut all_paths = Vec::new();
        let mut total_docs = 0u32;

        for (seg_idx, docs) in segments.iter().enumerate() {
            let mut writer = SegmentWriter::new();
            let base_id = docs.first().map(|doc| doc.0).unwrap_or(total_docs);
            base_ids.push(base_id);
            for &(doc_id, path, size_bytes) in docs {
                writer.add_document(doc_id, Path::new(path), doc_id as u64, size_bytes);
                if base_doc_paths.len() <= doc_id as usize {
                    base_doc_paths.resize(doc_id as usize + 1, None);
                }
                base_doc_paths[doc_id as usize] = Some(PathBuf::from(path));
                path_doc_ids
                    .entry(PathBuf::from(path))
                    .or_default()
                    .push(doc_id);
                all_paths.push(PathBuf::from(path));
                total_docs = total_docs.max(doc_id.saturating_add(1));
            }
            let meta = writer
                .write_to_dir(dir.path())
                .unwrap_or_else(|_| panic!("failed to write segment {seg_idx}"));
            let seg_ref: SegmentRef = meta.clone().into();
            seg_refs.push(seg_ref);
            mmap_segments.push(
                MmapSegment::open_split(
                    &dir.path().join(&meta.dict_filename),
                    &dir.path().join(&meta.post_filename),
                )
                .unwrap(),
            );
        }

        all_paths.sort_unstable();
        all_paths.dedup();
        let path_index = PathIndex::build(&all_paths);
        let mut doc_to_file_id = vec![u32::MAX; total_docs as usize];
        for (global_doc_id, path) in base_doc_paths.iter().enumerate() {
            if let Some(path) = path {
                if let Some(file_id) = path_index.file_id(path) {
                    doc_to_file_id[global_doc_id] = file_id;
                }
            }
        }

        let snapshot = new_snapshot(
            Arc::new(BaseSegments {
                segments: mmap_segments,
                base_ids,
                base_doc_paths,
                path_doc_ids,
            }),
            overlay,
            delete_set,
            path_index,
            doc_to_file_id,
            0.10,
        );
        (dir, snapshot, seg_refs)
    }

    #[test]
    fn plan_uses_segment_limit_and_snapshot_sizes() {
        let (_dir, snapshot, _seg_refs) = build_snapshot(
            &[
                vec![(0, "a.rs", 300_000_000)],
                vec![(1, "b.rs", 400_000_000)],
                vec![(2, "c.rs", 500_000_000)],
            ],
            OverlayView::empty(),
            RoaringBitmap::new(),
        );
        let config = Config {
            max_segments: 2,
            ..Config::default()
        };

        let plan = plan(&snapshot, &config).unwrap();
        assert_eq!(plan.reason, CompactionReason::SegmentLimit);
        assert_eq!(plan.suffix_start, 1);
        assert_eq!(plan.target_segments, 1);
        assert_eq!(plan.batch_size_bytes, 900_000_000);
    }

    #[test]
    fn plan_ignores_deleted_base_docs_when_sizing() {
        let mut delete_set = RoaringBitmap::new();
        delete_set.insert(0);
        let (_dir, snapshot, _seg_refs) = build_snapshot(
            &[
                vec![(0, "a.rs", 300_000_000)],
                vec![(1, "b.rs", 500_000_000)],
            ],
            OverlayView::empty(),
            delete_set,
        );
        let config = Config {
            max_segments: 1,
            ..Config::default()
        };

        let plan = plan(&snapshot, &config).unwrap();
        assert_eq!(plan.reason, CompactionReason::SegmentLimit);
        assert_eq!(plan.suffix_start, 0);
        assert_eq!(plan.batch_size_bytes, 500_000_000);
    }

    #[test]
    fn plan_prioritizes_overlay_ratio_trigger() {
        let overlay = OverlayView::build(
            10,
            vec![
                (
                    PathBuf::from("dirty_1.rs"),
                    Arc::from(&b"fn dirty_1() {}\n"[..]),
                ),
                (
                    PathBuf::from("dirty_2.rs"),
                    Arc::from(&b"fn dirty_2() {}\n"[..]),
                ),
            ],
        )
        .unwrap();
        let (_dir, snapshot, _seg_refs) = build_snapshot(
            &[
                vec![(0, "base_0.rs", 10)],
                vec![(1, "base_1.rs", 10)],
                vec![(2, "base_2.rs", 10)],
                vec![(3, "base_3.rs", 10)],
                vec![(4, "base_4.rs", 10)],
                vec![(5, "base_5.rs", 10)],
                vec![(6, "base_6.rs", 10)],
                vec![(7, "base_7.rs", 10)],
                vec![(8, "base_8.rs", 10)],
                vec![(9, "base_9.rs", 10)],
            ],
            overlay,
            RoaringBitmap::new(),
        );
        let config = Config {
            max_segments: 20,
            ..Config::default()
        };

        let plan = plan(&snapshot, &config).unwrap();
        assert_eq!(plan.reason, CompactionReason::OverlayRatio);
        assert_eq!(plan.suffix_start, 10);
        assert_eq!(plan.target_segments, 1);
        assert_eq!(plan.batch_size_bytes, super::super::build::BATCH_SIZE_BYTES);
    }

    #[test]
    fn forced_plan_rewrites_from_earliest_deleted_segment() {
        let mut delete_set = RoaringBitmap::new();
        delete_set.insert(1);
        let (_dir, snapshot, _seg_refs) = build_snapshot(
            &[
                vec![(0, "a.rs", 10)],
                vec![(1, "b.rs", 10)],
                vec![(2, "c.rs", 10)],
            ],
            OverlayView::empty(),
            delete_set,
        );
        let config = Config {
            max_segments: 10,
            ..Config::default()
        };

        let plan = forced_plan(&snapshot, &config).unwrap();
        assert_eq!(plan.reason, CompactionReason::ExplicitRequest);
        assert_eq!(plan.suffix_start, 1);
        assert_eq!(plan.target_segments, 1);
    }

    #[test]
    fn compact_rejects_snapshot_manifest_base_id_divergence() {
        let repo = TempDir::new().unwrap();
        let (index_dir, snapshot, mut seg_refs) = build_snapshot(
            &[vec![(0, "a.rs", 10)]],
            OverlayView::empty(),
            RoaringBitmap::new(),
        );
        seg_refs[0].base_doc_id = Some(7);
        let manifest = Manifest::new(seg_refs, 1);
        manifest.save(index_dir.path()).unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let plan = CompactionPlan {
            reason: CompactionReason::ExplicitRequest,
            suffix_start: 0,
            batch_size_bytes: 1,
            target_segments: 1,
        };

        let result = compact_index(config, Arc::new(snapshot), plan);
        let err = match result {
            Err(IndexError::CorruptIndex(msg)) => msg,
            Ok(_) => panic!("expected CorruptIndex for base-id divergence, got Ok(_)"),
            Err(other) => panic!("expected CorruptIndex for base-id divergence, got {other}"),
        };
        assert!(
            err.contains("snapshot base_id[0]=0 diverges from manifest base[0]=7"),
            "unexpected error: {err}"
        );
    }
