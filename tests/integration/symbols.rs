//! Integration tests for symbol-aware search (US4).
//!
//! Requires `--features symbols`.

use std::fs;
use tempfile::TempDir;

use syntext::index::Index;
use syntext::Config;

fn setup(dir: &TempDir) -> Config {
    let repo = dir.path().join("repo");
    let index_dir = dir.path().join("idx");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&index_dir).unwrap();
    Config {
        repo_root: repo,
        index_dir,
        ..Config::default()
    }
}

#[test]
fn symbol_search_finds_rust_function_definition() {
    let dir = TempDir::new().unwrap();
    let cfg = setup(&dir);

    // Write a Rust file with a known function.
    let src = cfg.repo_root.join("lib.rs");
    fs::write(
        &src,
        r#"
pub fn parse_query(input: &str) -> Option<String> {
    Some(input.to_string())
}

fn helper_internal() {}

pub struct QueryBuilder {
    value: u32,
}
"#,
    )
    .unwrap();

    let idx = Index::build(cfg).expect("build failed");

    // Symbol lookup by name (was `sym:parse_query`).
    let results = idx
        .search_symbols("parse_query", None)
        .expect("search failed");
    assert!(
        !results.is_empty(),
        "expected at least one result for symbol parse_query"
    );
    let r = &results[0];
    assert_eq!(r.path.file_name().unwrap(), "lib.rs");
    assert_eq!(r.line_number, 2, "parse_query is on line 2");

    // Lookup for a struct.
    let results = idx
        .search_symbols("QueryBuilder", None)
        .expect("search failed");
    assert!(!results.is_empty(), "expected result for QueryBuilder");

    // Nonexistent symbol returns empty.
    let results = idx
        .search_symbols("nonexistent_xyz_symbol", None)
        .expect("search failed");
    assert!(
        results.is_empty(),
        "expected no results for nonexistent symbol"
    );
    drop(idx);
}

#[test]
fn def_kind_filter_finds_function_definition() {
    let dir = TempDir::new().unwrap();
    let cfg = setup(&dir);

    let src = cfg.repo_root.join("main.rs");
    fs::write(
        &src,
        "pub fn run_server() -> std::io::Result<()> { Ok(()) }\n",
    )
    .unwrap();

    let idx = Index::build(cfg).expect("build failed");

    // Was `def:run_server` (function-kind filter).
    let results = idx
        .search_symbols("run_server", Some("function"))
        .expect("search failed");
    assert!(
        !results.is_empty(),
        "function-kind filter should find the definition"
    );
    assert_eq!(results[0].line_number, 1);

    // Unknown kind is an error, not a silent empty result.
    assert!(idx.search_symbols("run_server", Some("bogus")).is_err());
    drop(idx);
}

/// Bug 3 regression: the symbol index must stay correct after incremental
/// commits, not just full builds. Add + edit + delete flow through commit_batch.
#[test]
fn symbol_index_maintained_incrementally() {
    let dir = TempDir::new().unwrap();
    let cfg = setup(&dir);
    let repo_root = cfg.repo_root.clone();

    // Seed a base large enough that adding one file stays under the overlay
    // enforcement threshold (a 1-file base would reject any incremental add as
    // OverlayFull).
    for i in 0..12 {
        fs::write(
            repo_root.join(format!("seed_{i}.rs")),
            format!("pub fn seed_fn_{i}() {{}}\n"),
        )
        .unwrap();
    }
    let idx = Index::build(cfg).expect("build failed");

    // brand_new_fn does not exist yet.
    assert!(
        idx.search_symbols("brand_new_fn", None).unwrap().is_empty(),
        "symbol should not exist before it is added"
    );

    // Add a new file defining it and commit incrementally.
    let new_path = repo_root.join("added.rs");
    fs::write(&new_path, "pub fn brand_new_fn() {}\n").unwrap();
    idx.notify_change(&new_path).unwrap();
    idx.commit_batch().unwrap();
    assert!(
        !idx.search_symbols("brand_new_fn", None).unwrap().is_empty(),
        "incremental add must make the new symbol visible without a full reindex"
    );

    // Delete the file and commit: the symbol must be evicted.
    fs::remove_file(&new_path).unwrap();
    idx.notify_delete(&new_path).unwrap();
    idx.commit_batch().unwrap();
    assert!(
        idx.search_symbols("brand_new_fn", None).unwrap().is_empty(),
        "incremental delete must evict the symbol"
    );
    drop(idx);
}
