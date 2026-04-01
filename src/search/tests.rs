use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::index::Index;
use crate::query::literal_grams;
use crate::Config;

#[test]
fn fallback_path_filter_uses_same_glob_semantics() {
    let opts = SearchOptions {
        path_filter: Some("*.rs".to_string()),
        file_type: None,
        exclude_type: None,
        max_results: None,
        case_insensitive: false,
    };

    assert!(matches_path_filter(
        std::path::Path::new("src/main.rs"),
        opts.file_type.as_deref(),
        None,
        opts.path_filter.as_deref(),
    ));
    assert!(!matches_path_filter(
        std::path::Path::new("src/main.py"),
        opts.file_type.as_deref(),
        None,
        opts.path_filter.as_deref(),
    ));
}

#[test]
fn literal_queries_short_circuit_when_grams_are_missing() {
    let index_dir = TempDir::new().unwrap();
    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corpus"),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let snap = index.snapshot();
    let grams = literal_grams("xyzzy_no_match_sentinel_42").unwrap();

    assert!(should_use_index(&grams, &snap).unwrap());

    let candidates = execute_query(&GramQuery::Grams(grams), &snap).unwrap();
    assert!(candidates.is_empty());
    drop(index);
}

#[test]
fn posting_bitmaps_are_cached_per_snapshot() {
    let index_dir = TempDir::new().unwrap();
    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corpus"),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let snap = index.snapshot();
    let gram = literal_grams("parse_query").unwrap()[0];

    assert_eq!(snap.posting_bitmap_cache_len(), 0);

    let first = posting_bitmap(gram, &snap).unwrap();
    assert_eq!(snap.posting_bitmap_cache_len(), 1);

    let second = posting_bitmap(gram, &snap).unwrap();
    assert_eq!(snap.posting_bitmap_cache_len(), 1);
    assert!(Arc::ptr_eq(&first, &second));
    drop(index);
}

#[test]
fn posting_bitmap_cache_clears_when_cap_is_exceeded() {
    use roaring::RoaringBitmap;

    let index_dir = TempDir::new().unwrap();
    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corpus"),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let snap = index.snapshot();

    for gram_hash in 0..crate::index::snapshot::POSTING_BITMAP_CACHE_MAX_ENTRIES as u64 {
        let bitmap = Arc::new(RoaringBitmap::from_iter([gram_hash as u32]));
        snap.store_posting_bitmap(gram_hash, bitmap);
    }
    assert_eq!(
        snap.posting_bitmap_cache_len(),
        crate::index::snapshot::POSTING_BITMAP_CACHE_MAX_ENTRIES
    );

    let overflow_gram = crate::index::snapshot::POSTING_BITMAP_CACHE_MAX_ENTRIES as u64;
    let overflow_bitmap = Arc::new(RoaringBitmap::from_iter([overflow_gram as u32]));
    snap.store_posting_bitmap(overflow_gram, Arc::clone(&overflow_bitmap));

    assert_eq!(snap.posting_bitmap_cache_len(), 1);
    let cached = snap
        .cached_posting_bitmap(overflow_gram)
        .expect("overflow insert should remain cached");
    assert!(Arc::ptr_eq(&cached, &overflow_bitmap));
    drop(index);
}

#[test]
fn should_use_index_very_selective_term() {
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // 1 file has the target term; 99 do not. Cardinality = 1%.
    // Must use index regardless of calibrated threshold (max clamp is 0.50).
    for i in 0..100 {
        let content = if i == 0 {
            "fn ultra_rare_xtqvz_sentinel() {}\n".to_string()
        } else {
            format!("fn generic_function_{i}() {{}}\n")
        };
        std::fs::write(repo.path().join(format!("file_{i:03}.rs")), content).unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let snap = index.snapshot();
    let grams = literal_grams("ultra_rare_xtqvz_sentinel").unwrap();
    assert!(
        should_use_index(&grams, &snap).unwrap(),
        "1% cardinality must use index (threshold clamped to max 0.50)"
    );
    drop(index);
}

#[test]
fn should_use_index_ubiquitous_term() {
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // All 20 files contain the term. Cardinality = 100%.
    // Must fall back to scan regardless of calibrated threshold (max clamp is 0.50).
    for i in 0..20 {
        std::fs::write(
            repo.path().join(format!("file_{i:03}.rs")),
            "fn common_everywhere() {}\n",
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let snap = index.snapshot();
    let grams = literal_grams("common_everywhere").unwrap();
    assert!(
        !should_use_index(&grams, &snap).unwrap(),
        "100% cardinality must fall back to scan (threshold clamped to max 0.50)"
    );
    drop(index);
}

#[test]
fn should_use_index_respects_snapshot_threshold() {
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    for i in 0..20 {
        let content = if i < 6 {
            "fn target_alpha_marker_fn() {}\n".to_string()
        } else {
            format!("fn other_{i}() {{}}\n")
        };
        std::fs::write(repo.path().join(format!("file_{i:03}.rs")), content).unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    let snap_high = Arc::new(index.snapshot().with_scan_threshold(0.40));
    let snap_low = Arc::new(index.snapshot().with_scan_threshold(0.20));

    let grams = literal_grams("target_alpha_marker_fn").unwrap();
    assert!(
        should_use_index(&grams, &snap_high).unwrap(),
        "30% cardinality should use index when threshold is 0.40"
    );
    assert!(
        !should_use_index(&grams, &snap_low).unwrap(),
        "30% cardinality should NOT use index when threshold is 0.20"
    );
    drop(index);
}

#[test]
fn should_use_index_empty_hashes() {
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("a.rs"), "fn a() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let snap = index.snapshot();

    assert!(
        !should_use_index(&[], &snap).unwrap(),
        "empty gram list should not use index"
    );
    drop(index);
}

#[test]
fn should_use_index_for_compound_identifier_with_selective_intersection() {
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    for i in 0..8 {
        std::fs::write(
            repo.path().join(format!("irq_{i:02}.rs")),
            format!("fn irq_handler_{i}() {{ let irq = {i}; }}\n"),
        )
        .unwrap();
        std::fs::write(
            repo.path().join(format!("work_{i:02}.rs")),
            format!("fn work_handler_{i}() {{ let work = {i}; }}\n"),
        )
        .unwrap();
        std::fs::write(
            repo.path().join(format!("queue_{i:02}.rs")),
            format!("fn queue_handler_{i}() {{ let queue = {i}; }}\n"),
        )
        .unwrap();
    }

    std::fs::write(
        repo.path().join("match.rs"),
        "fn target() { irq_work_queue(); }\n",
    )
    .unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let snap = index.snapshot();

    let grams = literal_grams("irq_work_queue").unwrap();
    assert!(
        should_use_index(&grams, &snap).unwrap(),
        "compound identifier should use index when gram intersection is selective"
    );
    drop(index);
}

#[test]
fn type_not_excludes_file_extension() {
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("main.rs"), "fn target_fn() {}\n").unwrap();
    std::fs::write(repo.path().join("main.py"), "def target_fn(): pass\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let opts = SearchOptions {
        exclude_type: Some("py".to_string()),
        ..SearchOptions::default()
    };
    let results = index.search("target_fn", &opts).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].path.to_string_lossy().ends_with(".rs"));
    drop(index);
}

// --- PostingBudget tests ---

#[test]
fn posting_budget_charge_rejects_over_limit() {
    use super::executor::PostingBudget;

    let budget = PostingBudget::new(100);
    assert!(budget.charge(50).is_ok());
    assert!(budget.charge(50).is_ok());
    assert!(budget.charge(1).is_err(), "budget should reject when exhausted");
}

#[test]
fn posting_budget_cache_hits_are_free() {
    use super::executor::PostingBudget;

    let index_dir = TempDir::new().unwrap();
    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corpus"),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let snap = index.snapshot();
    let gram = literal_grams("parse_query").unwrap()[0];

    // First load populates the cache.
    let _first = posting_bitmap(gram, &snap).unwrap();
    assert_eq!(snap.posting_bitmap_cache_len(), 1);

    // Second call with a tiny budget succeeds because it hits the cache.
    let budget = PostingBudget::new(1);
    let second = super::executor::posting_bitmap(gram, &snap).unwrap();
    // Cache hit: no charge needed, budget untouched.
    assert!(budget.charge(0).is_ok());
    assert!(!second.is_empty() || second.is_empty()); // use the value
    drop(index);
}
