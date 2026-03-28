//! Integration tests for symbol-aware search (US4).
//!
//! Requires `--features symbols`.

use std::fs;
use tempfile::TempDir;

use syntext::index::Index;
use syntext::{Config, SearchOptions};

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

    // sym: prefix — find any definition named parse_query
    let opts = SearchOptions::default();
    let results = idx.search("sym:parse_query", &opts).expect("search failed");
    assert!(
        !results.is_empty(),
        "expected at least one result for sym:parse_query"
    );
    let r = &results[0];
    assert_eq!(r.path.file_name().unwrap(), "lib.rs");
    assert_eq!(r.line_number, 2, "parse_query is on line 2");

    // sym: for a struct
    let results = idx
        .search("sym:QueryBuilder", &opts)
        .expect("search failed");
    assert!(!results.is_empty(), "expected result for sym:QueryBuilder");

    // sym: for a nonexistent symbol returns empty
    let results = idx
        .search("sym:nonexistent_xyz_symbol", &opts)
        .expect("search failed");
    assert!(
        results.is_empty(),
        "expected no results for nonexistent symbol"
    );
}

#[test]
fn def_prefix_finds_function_definition() {
    let dir = TempDir::new().unwrap();
    let cfg = setup(&dir);

    let src = cfg.repo_root.join("main.rs");
    fs::write(
        &src,
        "pub fn run_server() -> std::io::Result<()> { Ok(()) }\n",
    )
    .unwrap();

    let idx = Index::build(cfg).expect("build failed");
    let opts = SearchOptions::default();

    let results = idx.search("def:run_server", &opts).expect("search failed");
    assert!(!results.is_empty(), "def: should find function definition");
    assert_eq!(results[0].line_number, 1);
}
