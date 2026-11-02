//! Integration tests for indexes built and opened with a `ContentSource`.
//!
//! The repository root in every test is an empty directory that is not a git
//! repository, so any document the index finds can only have come from the
//! source. Durability assertions go through `drop(index)` and
//! `Index::open_with_source`: the overlay is empty on reopen, so anything
//! still visible came off disk.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use syntext::index::manifest::Manifest;
use syntext::index::{ContentSource, Index, SourceFile, UpdateLimits};
use syntext::{Config, IndexError, SearchOptions};

#[path = "lock_retry.rs"]
mod lock_retry;
use lock_retry::{commit_batch_with_retry, is_lock_conflict, retry_lock_contention};

/// An in-memory corpus standing in for a revision that is not checked out.
#[derive(Default)]
struct MapSource {
    files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
    reads: AtomicUsize,
}

impl MapSource {
    fn new(files: &[(&str, &[u8])]) -> Arc<Self> {
        let source = MapSource::default();
        for (path, bytes) in files {
            source.put(path, bytes);
        }
        Arc::new(source)
    }

    /// Like `new`, plus unrelated documents so a few edits stay under the
    /// overlay's 50%-of-base cap, past which `commit_batch` asks for a rebuild.
    fn with_filler(files: &[(&str, &[u8])]) -> Arc<Self> {
        let source = Self::new(files);
        for i in 0..20 {
            source.put(
                &format!("filler/f{i}.rs"),
                format!("fn filler_{i}() {{}}\n").as_bytes(),
            );
        }
        source
    }

    fn put(&self, path: &str, bytes: &[u8]) {
        self.files
            .lock()
            .unwrap()
            .insert(PathBuf::from(path), bytes.to_vec());
    }

    fn remove(&self, path: &str) {
        self.files.lock().unwrap().remove(Path::new(path));
    }
}

impl ContentSource for MapSource {
    fn files(&self) -> io::Result<Vec<SourceFile>> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .iter()
            .map(|(path, bytes)| SourceFile {
                path: path.clone(),
                size_bytes: bytes.len() as u64,
            })
            .collect())
    }

    fn read(&self, path: &Path, _limit: u64) -> io::Result<Option<Vec<u8>>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.files.lock().unwrap().get(path).cloned())
    }
}

/// A source whose corpus lists a path supplied by the test, valid or not.
struct ListedSource(Vec<SourceFile>);

impl ContentSource for ListedSource {
    fn files(&self) -> io::Result<Vec<SourceFile>> {
        Ok(self.0.clone())
    }

    fn read(&self, _path: &Path, _limit: u64) -> io::Result<Option<Vec<u8>>> {
        Ok(Some(b"fn listed() {}\n".to_vec()))
    }
}

struct Fixture {
    repo: tempfile::TempDir,
    index_dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Fixture {
            repo: tempfile::tempdir().unwrap(),
            index_dir: tempfile::tempdir().unwrap(),
        }
    }

    fn config(&self) -> Config {
        Config {
            index_dir: self.index_dir.path().to_path_buf(),
            repo_root: self.repo.path().to_path_buf(),
            ..Config::default()
        }
    }

    fn build(&self, config: Config, source: Arc<dyn ContentSource>) -> Index {
        retry_lock_contention(
            || Index::build_from_source(config.clone(), Arc::clone(&source)),
            is_lock_conflict,
        )
        .expect("build_from_source")
    }

    fn abs(&self, rel: &str) -> PathBuf {
        self.repo.path().join(rel)
    }
}

fn paths(index: &Index, pattern: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = index
        .search(pattern, &SearchOptions::default())
        .expect("search")
        .into_iter()
        .map(|m| m.path)
        .collect();
    found.dedup();
    found
}

fn is_unsupported(err: &IndexError) -> bool {
    matches!(err, IndexError::Io(e) if e.kind() == io::ErrorKind::Unsupported)
}

#[test]
fn builds_and_searches_documents_that_are_not_on_disk() {
    let fx = Fixture::new();
    let source = MapSource::new(&[
        ("src/lib.rs", b"pub fn parse_query() {}\n"),
        ("docs/notes.md", b"retry handling lives here\n"),
    ]);
    let index = fx.build(fx.config(), source.clone());

    assert_eq!(
        paths(&index, "parse_query"),
        vec![PathBuf::from("src/lib.rs")]
    );
    assert_eq!(
        paths(&index, "retry handling"),
        vec![PathBuf::from("docs/notes.md")]
    );
    assert!(
        !fx.abs("src/lib.rs").exists(),
        "the repo root must stay empty"
    );
    assert!(
        source.reads.load(Ordering::Relaxed) > 0,
        "bytes came from the source"
    );

    // A source build never runs git, so it records no HEAD to diff against.
    let manifest = Manifest::load(fx.index_dir.path()).unwrap();
    assert_eq!(manifest.base_commit, None);
    drop(index);
}

#[test]
fn skips_binary_and_oversized_documents() {
    let fx = Fixture::new();
    let mut config = fx.config();
    config.max_file_size = 64;
    let big = format!("needle_term {}\n", "x".repeat(100));
    let source = MapSource::new(&[
        ("small.rs", b"needle_term\n"),
        ("big.rs", big.as_bytes()),
        ("blob.bin", b"needle_term\0\x01\x02\n"),
    ]);
    let index = fx.build(config, source);

    assert_eq!(
        paths(&index, "needle_term"),
        vec![PathBuf::from("small.rs")]
    );
    drop(index);
}

#[test]
fn rejects_escaping_and_duplicate_paths() {
    let fx = Fixture::new();
    let file = |path: &str| SourceFile {
        path: PathBuf::from(path),
        size_bytes: 15,
    };

    let escaping = Arc::new(ListedSource(vec![file("../outside.rs")]));
    match Index::build_from_source(fx.config(), escaping) {
        Err(IndexError::PathOutsideRepo(_)) => {}
        other => panic!("expected PathOutsideRepo, got {:?}", other.err()),
    }

    let duplicate = Arc::new(ListedSource(vec![file("a.rs"), file("a.rs")]));
    match Index::build_from_source(fx.config(), duplicate) {
        Err(IndexError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput => {}
        other => panic!("expected InvalidInput, got {:?}", other.err()),
    }
}

#[test]
fn commits_and_flushes_through_the_source() {
    let fx = Fixture::new();
    let source = MapSource::with_filler(&[
        ("src/lib.rs", b"pub fn parse_query() {}\n"),
        ("docs/notes.md", b"retry handling lives here\n"),
        ("src/old.rs", b"fn legacy_path() {}\n"),
    ]);
    let index = fx.build(fx.config(), source.clone());

    // An edit, a source-side removal, and an explicit delete.
    source.put("src/lib.rs", b"pub fn renamed_query() {}\n");
    source.put("src/new.rs", b"fn brand_new_symbol() {}\n");
    source.remove("docs/notes.md");
    index.notify_change(&fx.abs("src/lib.rs")).unwrap();
    index.notify_change(&fx.abs("src/new.rs")).unwrap();
    index.notify_change(&fx.abs("docs/notes.md")).unwrap();
    index.notify_delete(&fx.abs("src/old.rs")).unwrap();
    commit_batch_with_retry(&index);

    assert_eq!(
        paths(&index, "renamed_query"),
        vec![PathBuf::from("src/lib.rs")]
    );
    assert_eq!(
        paths(&index, "brand_new_symbol"),
        vec![PathBuf::from("src/new.rs")]
    );
    assert!(paths(&index, "parse_query").is_empty());
    assert!(
        paths(&index, "retry handling").is_empty(),
        "missing from source is a deletion"
    );
    assert!(paths(&index, "legacy_path").is_empty());

    assert!(
        index.flush_overlay().expect("flush"),
        "the overlay held changes"
    );
    drop(index);

    let reopened = Index::open_with_source(fx.config(), source).expect("open_with_source");
    assert_eq!(
        paths(&reopened, "renamed_query"),
        vec![PathBuf::from("src/lib.rs")]
    );
    assert_eq!(
        paths(&reopened, "brand_new_symbol"),
        vec![PathBuf::from("src/new.rs")]
    );
    assert!(paths(&reopened, "parse_query").is_empty());
    assert!(paths(&reopened, "legacy_path").is_empty());
    assert!(paths(&reopened, "retry handling").is_empty());
    drop(reopened);
}

#[test]
fn git_freshness_is_unsupported() {
    let fx = Fixture::new();
    let source = MapSource::new(&[("src/lib.rs", b"pub fn parse_query() {}\n")]);
    let index = fx.build(fx.config(), source);
    let limits = UpdateLimits {
        max_files: None,
        budget_ms: None,
    };

    assert!(is_unsupported(
        &index.update_from_git(limits.clone()).unwrap_err()
    ));
    assert!(is_unsupported(&index.rebuild_if_stale().unwrap_err()));
    // search_fresh treats a failed freshness check as "proceed", so it is a
    // plain search here.
    let (matches, _) = index
        .search_fresh("parse_query", &SearchOptions::default(), limits)
        .expect("search_fresh");
    assert_eq!(matches.len(), 1);
    drop(index);
}

#[test]
fn a_drifted_source_only_loses_matches() {
    let fx = Fixture::new();
    let source = MapSource::new(&[("src/lib.rs", b"pub fn parse_query() {}\n")]);
    let index = fx.build(fx.config(), source.clone());

    // Change the bytes without telling the index. Postings still name the old
    // term, but verification re-reads the source and rejects it.
    source.put("src/lib.rs", b"pub fn something_else() {}\n");
    assert!(paths(&index, "parse_query").is_empty());
    drop(index);
}

#[test]
fn preloaded_change_batch_content_still_wins() {
    use syntext::changes::{ChangeBatch, ChangeKind, ChangeRecord};

    let fx = Fixture::new();
    let source = MapSource::with_filler(&[("src/lib.rs", b"pub fn parse_query() {}\n")]);
    let index = fx.build(fx.config(), source.clone());

    let content: Arc<[u8]> = Arc::from(&b"pub fn preloaded_term() {}\n"[..]);
    source.put("src/lib.rs", b"pub fn preloaded_term() {}\n");
    let record = ChangeRecord {
        path: PathBuf::from("src/lib.rs"),
        kind: ChangeKind::Modified,
        current_fingerprint: None,
        previous_fingerprint: None,
        content: Some(content),
    };
    let batch = ChangeBatch::new(1, vec![record], false);
    retry_lock_contention(|| index.apply_change_batch(&batch), is_lock_conflict)
        .expect("apply_change_batch");

    assert_eq!(
        paths(&index, "preloaded_term"),
        vec![PathBuf::from("src/lib.rs")]
    );
    drop(index);
}
