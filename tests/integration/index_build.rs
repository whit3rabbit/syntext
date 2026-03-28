//! T028: Integration tests for Index::build() and Index::open().
//!
//! Verifies:
//! - Segment files are created with valid RPLX headers.
//! - Binary files (with null bytes) are skipped.
//! - .gitignore-excluded files are not indexed.
//! - `open()` round-trips through the manifest and reconstructs doc counts.

use std::path::Path;
use tempfile::TempDir;

use syntext::index::segment::{MmapSegment, FOOTER_SIZE, MAGIC};
use syntext::index::Index;
use syntext::{Config, SearchOptions};

/// Path to the fixture corpus committed to the repo.
fn corpus_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corpus")
}

/// Create a minimal Config pointing at the fixture corpus.
fn make_config(index_dir: &TempDir) -> Config {
    Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: corpus_dir(),
        ..Config::default()
    }
}

#[test]
fn build_creates_segment_with_valid_rplx_header() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    Index::build(config).expect("build should succeed");

    // At least one .dict file must exist for the v3 split segment format.
    let dict_files: Vec<_> = std::fs::read_dir(index_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "dict").unwrap_or(false))
        .collect();
    assert!(!dict_files.is_empty(), "no dictionary segment files created");

    // Every segment must pass integrity checks (magic, version, checksum).
    for dict_file in &dict_files {
        MmapSegment::open(&dict_file.path())
            .unwrap_or_else(|e| panic!("segment {:?} failed to open: {e}", dict_file.path()));
    }
}

#[test]
fn build_produces_nonzero_doc_count() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    let idx = Index::build(config).unwrap();
    let stats = idx.stats();
    // The corpus has ~39 non-ignored text files; be conservative.
    assert!(
        stats.total_documents >= 30,
        "expected at least 30 docs, got {}",
        stats.total_documents
    );
}

#[test]
fn gitignored_file_not_indexed() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    let idx = Index::build(config).unwrap();
    let snap = idx.snapshot();

    // build/output.txt is in build/ which is gitignored.
    let found = snap
        .path_index
        .paths
        .iter()
        .any(|p| p == std::path::Path::new("build/output.txt"));
    assert!(
        !found,
        "gitignored file appeared in index: build/output.txt"
    );
}

#[test]
fn binary_file_skipped() {
    let corpus_dir = TempDir::new().unwrap();
    // Write a minimal corpus with one normal text file and one binary file.
    let src_dir = corpus_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("hello.rs"),
        b"fn main() { println!(\"hello\"); }",
    )
    .unwrap();

    // Binary file: contains null bytes.
    let mut binary = vec![0u8; 100];
    binary[0..5].copy_from_slice(b"BINAR");
    std::fs::write(corpus_dir.path().join("data.bin"), &binary).unwrap();

    let index_dir = TempDir::new().unwrap();
    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: corpus_dir.path().to_path_buf(),
        ..Config::default()
    };

    let idx = Index::build(config).unwrap();
    let snap = idx.snapshot();

    // The binary file must not appear in the path index.
    let has_binary = snap
        .path_index
        .paths
        .iter()
        .any(|p| p == std::path::Path::new("data.bin"));
    assert!(!has_binary, "binary file appeared in index");

    // The text file must be indexed.
    let has_text = snap
        .path_index
        .paths
        .iter()
        .any(|p| p == std::path::Path::new("src/hello.rs"));
    assert!(has_text, "text file not indexed");
}

#[test]
fn open_round_trips_segment_metadata() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    let idx_built = Index::build(config.clone()).unwrap();
    let stats_built = idx_built.stats();

    // Re-open from disk.
    let idx_opened = Index::open(config).unwrap();
    let stats_opened = idx_opened.stats();

    assert_eq!(
        stats_built.total_documents, stats_opened.total_documents,
        "doc count differs after re-open"
    );
    assert_eq!(
        stats_built.total_segments, stats_opened.total_segments,
        "segment count differs after re-open"
    );
}

#[test]
fn segment_footer_magic_matches() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    Index::build(config).unwrap();

    for entry in std::fs::read_dir(index_dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry
            .path()
            .extension()
            .map(|e| e == "seg")
            .unwrap_or(false)
        {
            let data = std::fs::read(entry.path()).unwrap();
            assert!(data.len() >= FOOTER_SIZE, "segment too small");
            // Footer ends with b"RPLX"
            let footer_end = &data[data.len() - 4..];
            assert_eq!(footer_end, MAGIC, "footer magic mismatch");
        }
    }
}

// ---------------------------------------------------------------------------
// T047: Path/type scoping tests
// ---------------------------------------------------------------------------

/// Search with file_type="py" returns only .py files.
#[test]
fn search_file_type_py_only() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    let idx = Index::build(config).unwrap();

    let opts = SearchOptions {
        file_type: Some("py".to_string()),
        ..SearchOptions::default()
    };
    let results = idx.search("parse_query", &opts).unwrap();
    for m in &results {
        let p = m.path.to_string_lossy();
        assert!(
            p.ends_with(".py"),
            "file_type=py returned non-.py file: {}",
            p
        );
    }
    assert!(
        !results.is_empty(),
        "fixture invariant: parse_query should appear in at least one .py file"
    );
}

/// Search with file_type="rs" returns only .rs files.
#[test]
fn search_file_type_rs_only() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    let idx = Index::build(config).unwrap();

    let opts = SearchOptions {
        file_type: Some("rs".to_string()),
        ..SearchOptions::default()
    };
    let results = idx.search("parse_query", &opts).unwrap();
    for m in &results {
        let p = m.path.to_string_lossy();
        assert!(
            p.ends_with(".rs"),
            "file_type=rs returned non-.rs file: {}",
            p
        );
    }
}

/// Search with path_filter="python" returns only files under python/.
#[test]
fn search_path_filter_subdirectory() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    let idx = Index::build(config).unwrap();

    let opts = SearchOptions {
        path_filter: Some("python/".to_string()),
        ..SearchOptions::default()
    };
    let results = idx.search("parse_query", &opts).unwrap();
    for m in &results {
        let p = m.path.to_string_lossy();
        assert!(
            p.contains("python/"),
            "path_filter=python/ returned file outside python/: {}",
            p
        );
    }
}

/// Combined: file_type="rs" + path_filter="rust/" narrows to only .rs files under rust/.
#[test]
fn search_combined_type_and_path() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    let idx = Index::build(config).unwrap();

    let opts = SearchOptions {
        file_type: Some("rs".to_string()),
        path_filter: Some("rust/".to_string()),
        ..SearchOptions::default()
    };
    let results = idx.search("parse_query", &opts).unwrap();
    for m in &results {
        let p = m.path.to_string_lossy();
        assert!(p.ends_with(".rs"), "non-.rs: {}", p);
        assert!(p.contains("rust/"), "not under rust/: {}", p);
    }
}

#[test]
fn search_exclude_type_omits_matching_extension() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    let idx = Index::build(config).unwrap();

    let opts = SearchOptions {
        exclude_type: Some("py".to_string()),
        ..SearchOptions::default()
    };
    let results = idx.search("parse_query", &opts).unwrap();
    assert!(
        results
            .iter()
            .all(|m| !m.path.to_string_lossy().ends_with(".py")),
        "exclude_type=py should remove Python files from results"
    );
}

#[cfg(unix)]
#[test]
fn build_indexes_files_under_symlinked_directory_path() {
    use std::os::unix::fs::symlink;

    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let real_dir = repo.path().join("real");
    std::fs::create_dir_all(&real_dir).unwrap();
    std::fs::write(real_dir.join("nested.rs"), "fn symlink_dir_visible() {}\n").unwrap();
    symlink(&real_dir, repo.path().join("alias")).unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let idx = Index::build(config).unwrap();

    let results = idx
        .search("symlink_dir_visible", &SearchOptions::default())
        .unwrap();
    assert!(
        results
            .iter()
            .any(|m| m.path.to_string_lossy() == "alias/nested.rs"),
        "symlinked directory contents should be searchable through the symlink path"
    );
}

// ---------------------------------------------------------------------------
// T048: v3 two-file segment format end-to-end integration
// ---------------------------------------------------------------------------

#[test]
fn v3_format_produces_dict_and_post_files() {
    use std::collections::HashSet;

    let repo_dir = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // Write a small corpus
    std::fs::write(
        repo_dir.path().join("main.rs"),
        b"fn main() { println!(\"hello_syntext\"); }",
    )
    .unwrap();
    std::fs::write(
        repo_dir.path().join("lib.rs"),
        b"pub fn hello_syntext() -> String { String::from(\"hello_syntext\") }",
    )
    .unwrap();

    let config = Config {
        repo_root: repo_dir.path().to_path_buf(),
        index_dir: index_dir.path().to_path_buf(),
        ..Config::default()
    };

    // Build index
    Index::build(config.clone()).expect("v3 build should succeed");

    // Verify file layout: .dict and .post must exist; .seg must not
    let entries: Vec<_> = std::fs::read_dir(index_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    let extensions: HashSet<&str> = entries
        .iter()
        .filter_map(|n| n.rsplit('.').next())
        .collect();

    assert!(
        extensions.contains("dict"),
        "v3 build must produce .dict files; found: {:?}",
        entries
    );
    assert!(
        extensions.contains("post"),
        "v3 build must produce .post files; found: {:?}",
        entries
    );
    assert!(
        !extensions.contains("seg"),
        "v3 build must not produce legacy .seg files; found: {:?}",
        entries
    );

    // Search on the built index
    let index = Index::open(config.clone()).expect("v3 open should succeed");
    let opts = SearchOptions::default();
    let results = index
        .search("hello_syntext", &opts)
        .expect("search should succeed");
    assert!(
        !results.is_empty(),
        "search must find 'hello_syntext' in v3 index"
    );

    // Incremental update: add a new file, notify, commit, search
    std::fs::write(
        repo_dir.path().join("new_file.rs"),
        b"fn new_function_xyz() {}",
    )
    .unwrap();
    index
        .notify_change(&repo_dir.path().join("new_file.rs"))
        .expect("notify_change should succeed");
    index.commit_batch().expect("commit_batch should succeed");

    let results2 = index
        .search("new_function_xyz", &opts)
        .expect("search after incremental update should succeed");
    assert!(
        !results2.is_empty(),
        "incremental update must find 'new_function_xyz' after commit_batch"
    );
}
