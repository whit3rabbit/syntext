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
    drop(Index::build(config).expect("build should succeed"));

    // At least one .dict file must exist for the v3 split segment format.
    let dict_files: Vec<_> = std::fs::read_dir(index_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "dict").unwrap_or(false))
        .collect();
    assert!(
        !dict_files.is_empty(),
        "no dictionary segment files created"
    );

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
    drop(idx);
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
    drop(idx);
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
    drop(idx);
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
    drop(idx_built);
    drop(idx_opened);
}

#[test]
fn segment_footer_magic_matches() {
    let index_dir = TempDir::new().unwrap();
    let config = make_config(&index_dir);
    drop(Index::build(config).unwrap());

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
    drop(idx);
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
    drop(idx);
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
    drop(idx);
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
    drop(idx);
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
    drop(idx);
}

#[cfg(unix)]
#[test]
fn build_skips_files_under_symlinked_directory_path() {
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
            .all(|m| m.path.to_string_lossy() != "alias/nested.rs"),
        "directory symlink contents must not be indexed through the alias path"
    );
    drop(idx);
}

// ---------------------------------------------------------------------------
// T049: non-UTF-8 encoding normalization (BOM stripping, UTF-16 transcoding)
// ---------------------------------------------------------------------------

#[test]
fn utf8_bom_file_is_indexed_without_bom_bytes() {
    let corpus_dir = TempDir::new().unwrap();
    let src_dir = corpus_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let mut content = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM
    content.extend_from_slice(b"fn bom_function() {}\n");
    std::fs::write(src_dir.join("bom.rs"), &content).unwrap();

    let index_dir = TempDir::new().unwrap();
    let config = Config {
        repo_root: corpus_dir.path().to_path_buf(),
        index_dir: index_dir.path().to_path_buf(),
        ..Config::default()
    };
    let idx = Index::build(config).unwrap();
    let snap = idx.snapshot();

    assert!(
        snap.path_index
            .paths
            .iter()
            .any(|p| p == std::path::Path::new("src/bom.rs")),
        "UTF-8 BOM file must appear in the path index"
    );

    let opts = SearchOptions::default();
    let matches = idx.search("bom_function", &opts).unwrap();
    assert!(
        !matches.is_empty(),
        "must find 'bom_function' in a UTF-8 BOM file"
    );
    let line = &matches[0].line_content;
    assert!(
        !line.starts_with(&[0xEF, 0xBB, 0xBF]),
        "BOM bytes must not appear in matched line content, got: {line:?}"
    );
    drop(idx);
}

#[test]
fn utf16_le_file_is_indexed_and_searchable() {
    let corpus_dir = TempDir::new().unwrap();
    let src_dir = corpus_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let src = "fn utf16_function() {}\n";
    let mut utf16le: Vec<u8> = vec![0xFF, 0xFE]; // LE BOM
    utf16le.extend(src.encode_utf16().flat_map(|u| u.to_le_bytes()));
    std::fs::write(src_dir.join("utf16le.rs"), &utf16le).unwrap();

    let index_dir = TempDir::new().unwrap();
    let config = Config {
        repo_root: corpus_dir.path().to_path_buf(),
        index_dir: index_dir.path().to_path_buf(),
        ..Config::default()
    };
    let idx = Index::build(config).unwrap();
    let snap = idx.snapshot();

    assert!(
        snap.path_index
            .paths
            .iter()
            .any(|p| p == std::path::Path::new("src/utf16le.rs")),
        "UTF-16 LE file must appear in the path index (not skipped as binary)"
    );

    let opts = SearchOptions::default();
    let matches = idx.search("utf16_function", &opts).unwrap();
    assert!(
        !matches.is_empty(),
        "must find 'utf16_function' in a UTF-16 LE file"
    );
    drop(idx);
}

#[test]
fn utf16_be_file_is_indexed_and_searchable() {
    let corpus_dir = TempDir::new().unwrap();
    let src_dir = corpus_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let src = "fn utf16be_fn() {}\n";
    let mut utf16be: Vec<u8> = vec![0xFE, 0xFF]; // BE BOM
    utf16be.extend(src.encode_utf16().flat_map(|u| u.to_be_bytes()));
    std::fs::write(src_dir.join("utf16be.rs"), &utf16be).unwrap();

    let index_dir = TempDir::new().unwrap();
    let config = Config {
        repo_root: corpus_dir.path().to_path_buf(),
        index_dir: index_dir.path().to_path_buf(),
        ..Config::default()
    };
    let idx = Index::build(config).unwrap();
    let snap = idx.snapshot();

    assert!(
        snap.path_index
            .paths
            .iter()
            .any(|p| p == std::path::Path::new("src/utf16be.rs")),
        "UTF-16 BE file must appear in the path index (not skipped as binary)"
    );

    let opts = SearchOptions::default();
    let matches = idx.search("utf16be_fn", &opts).unwrap();
    assert!(
        !matches.is_empty(),
        "must find 'utf16be_fn' in a UTF-16 BE file"
    );
    drop(idx);
}

#[test]
fn utf16_le_via_incremental_commit_is_searchable() {
    let corpus_dir = TempDir::new().unwrap();
    let src_dir = corpus_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    // Need enough base docs so 1 overlay doc stays within the 50% threshold.
    for i in 0..4 {
        std::fs::write(
            src_dir.join(format!("base_{i}.rs")),
            format!("fn base_fn_{i}() {{}}\n").as_bytes(),
        )
        .unwrap();
    }

    let index_dir = TempDir::new().unwrap();
    let config = Config {
        repo_root: corpus_dir.path().to_path_buf(),
        index_dir: index_dir.path().to_path_buf(),
        ..Config::default()
    };
    let idx = Index::build(config).unwrap();

    let src = "fn incremental_utf16() {}\n";
    let mut utf16le: Vec<u8> = vec![0xFF, 0xFE];
    utf16le.extend(src.encode_utf16().flat_map(|u| u.to_le_bytes()));
    let new_path = src_dir.join("added.rs");
    std::fs::write(&new_path, &utf16le).unwrap();

    idx.notify_change(&new_path).unwrap();
    idx.commit_batch().unwrap();

    let opts = SearchOptions::default();
    let matches = idx.search("incremental_utf16", &opts).unwrap();
    assert!(
        !matches.is_empty(),
        "UTF-16 LE file added via commit_batch must be searchable"
    );
    drop(idx);
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
    drop(Index::build(config.clone()).expect("v3 build should succeed"));

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
    drop(index);
}
