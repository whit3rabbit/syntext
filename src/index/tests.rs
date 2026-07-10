use super::*;
use std::sync::{Mutex, MutexGuard, OnceLock};
use tempfile::TempDir;
use xxhash_rust::xxh64::xxh64;

/// Process-local mutex that serializes index-heavy tests within this binary.
/// Cross-binary isolation is unnecessary: every test creates its own `TempDir`
/// for both repo and index directories, so no mutable state is shared between
/// test binaries that `cargo test` runs in parallel.
fn serial_index_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn git(args: &[&str], repo: &std::path::Path) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {:?} failed", args);
}

fn init_git_repo(repo: &std::path::Path) {
    git(&["init"], repo);
    git(&["config", "user.name", "Syntext Tests"], repo);
    git(&["config", "user.email", "syntext@example.com"], repo);
}

fn commit_all(repo: &std::path::Path, message: &str) -> String {
    git(&["add", "."], repo);
    git(&["commit", "-m", message], repo);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success(), "git rev-parse HEAD failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn base_doc_hash(index: &Index, relative_path: &std::path::Path) -> Option<u64> {
    let snapshot = index.snapshot();
    for seg in snapshot.base_segments() {
        for local_doc_id in 0..seg.doc_count {
            let doc = seg.get_doc(local_doc_id)?;
            if doc.path == relative_path {
                return Some(doc.content_hash);
            }
        }
    }
    None
}

fn write_segment_with_global_doc_id(
    index_dir: &std::path::Path,
    doc_id: u32,
    relative_path: &str,
    content: &[u8],
) -> crate::index::manifest::SegmentRef {
    let mut writer = crate::index::segment::SegmentWriter::new();
    writer.add_document(
        doc_id,
        std::path::Path::new(relative_path),
        xxh64(content, 0),
        content.len() as u64,
    );
    for gram_hash in crate::tokenizer::build_all(content) {
        writer.add_gram_posting(gram_hash, doc_id);
    }
    let mut seg_ref: crate::index::manifest::SegmentRef =
        writer.write_to_dir(index_dir).unwrap().into();
    seg_ref.base_doc_id = Some(doc_id);
    seg_ref
}

fn write_sparse_manifest_index(repo: &std::path::Path, index_dir: &std::path::Path) -> Config {
    std::fs::write(repo.join("a.rs"), b"fn alpha() {}\n").unwrap();
    std::fs::write(repo.join("b.rs"), b"fn beta() {}\n").unwrap();

    let seg_a = write_segment_with_global_doc_id(index_dir, 0, "a.rs", b"fn alpha() {}\n");
    let seg_b = write_segment_with_global_doc_id(index_dir, 5, "b.rs", b"fn beta() {}\n");
    let mut manifest = crate::index::manifest::Manifest::new(vec![seg_a, seg_b], 2);
    manifest.scan_threshold_fraction = Some(0.10);
    manifest.save(index_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(index_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    Config {
        index_dir: index_dir.to_path_buf(),
        repo_root: repo.to_path_buf(),
        ..Config::default()
    }
}

#[test]
fn build_produces_calibrated_threshold_in_valid_range() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // A corpus large enough that calibration has real files to sample.
    for i in 0..50 {
        std::fs::write(
            repo.path().join(format!("file_{i:03}.rs")),
            format!("fn func_{i}() {{ let x = {i}; }}\n").repeat(20),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config.clone()).unwrap();

    // The manifest must contain a calibrated threshold.
    let manifest = crate::index::manifest::Manifest::load(&config.index_dir).unwrap();
    let threshold = manifest
        .scan_threshold_fraction
        .expect("build() must populate scan_threshold_fraction");

    assert!(
        (0.01..=0.50).contains(&threshold),
        "calibrated threshold {threshold} must be in [0.01, 0.50]"
    );

    // The loaded snapshot must use the calibrated value.
    let snap = index.snapshot();
    assert_eq!(
        snap.scan_threshold, threshold,
        "snapshot.scan_threshold must match manifest value"
    );
    drop(index);
}

#[test]
fn build_writes_paths_idx_with_current_manifest_version() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("a.rs"), b"fn alpha() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config.clone()).unwrap();
    drop(index);

    assert!(
        index_dir.path().join("paths.idx").exists(),
        "st index must write a paths.idx sidecar"
    );
    let manifest = crate::index::manifest::Manifest::load(&config.index_dir).unwrap();
    assert_eq!(
        manifest.paths_idx_version,
        Some(super::paths_idx::FORMAT_VERSION),
        "manifest must record the format version of the paths.idx it wrote"
    );
}

#[test]
fn open_ignores_paths_idx_when_manifest_version_mismatches() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("a.rs"), b"fn alpha() {}\n").unwrap();
    std::fs::write(repo.path().join("b.rs"), b"fn beta() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config.clone()).unwrap();
    drop(index);

    // Bump the manifest's recorded paths_idx_version past what this binary's
    // `paths_idx::FORMAT_VERSION` understands, simulating a manifest written
    // by a different sidecar-format generation than the paths.idx bytes
    // actually on disk (e.g. an interrupted upgrade). The paths.idx file
    // itself is untouched and still well-formed by its own magic/checksum.
    let mut manifest = crate::index::manifest::Manifest::load(&config.index_dir).unwrap();
    assert_eq!(
        manifest.paths_idx_version,
        Some(super::paths_idx::FORMAT_VERSION)
    );
    manifest.paths_idx_version = Some(super::paths_idx::FORMAT_VERSION + 1);
    manifest.save(&config.index_dir).unwrap();

    // open() must not trust the version-mismatched paths.idx: it should fall
    // back to rebuilding PathIndex from segment doc tables, and search
    // results must still be correct either way.
    let index = Index::open(config).unwrap();
    assert_eq!(
        index
            .search("alpha", &SearchOptions::default())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        index
            .search("beta", &SearchOptions::default())
            .unwrap()
            .len(),
        1
    );
    drop(index);
}

#[test]
fn open_accepts_manifest_with_gapped_base_doc_ids() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let config = write_sparse_manifest_index(repo.path(), index_dir.path());
    let index = Index::open(config).unwrap();

    assert_eq!(index.snapshot().segment_base_ids(), &[0, 5]);
    let all_doc_ids: Vec<u32> = index.snapshot().all_doc_ids().iter().collect();
    assert_eq!(all_doc_ids, vec![0, 5]);
    assert_eq!(
        index
            .search("alpha", &SearchOptions::default())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        index
            .search("beta", &SearchOptions::default())
            .unwrap()
            .len(),
        1
    );
    drop(index);
}

#[test]
fn commit_batch_overlay_ids_start_after_max_base_doc_id() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let config = write_sparse_manifest_index(repo.path(), index_dir.path());
    let index = Index::open(config).unwrap();

    let new_path = repo.path().join("c.rs");
    std::fs::write(&new_path, b"fn gamma() {}\n").unwrap();
    index.notify_change(&new_path).unwrap();
    index.commit_batch().unwrap();

    let overlay_ids: Vec<u32> = index
        .snapshot()
        .overlay
        .docs
        .iter()
        .map(|doc| doc.doc_id)
        .collect();
    assert_eq!(overlay_ids, vec![6]);
    drop(index);
}

#[test]
fn commit_batch_skips_file_that_exceeds_limit() {
    // A file that grew past max_file_size is excluded (like a binary file)
    // rather than aborting the batch. commit_batch succeeds and the file is
    // absent from the overlay.
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    let path = repo.path().join("big.rs");
    std::fs::write(&path, b"fn small() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        max_file_size: 10, // very small limit
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // Grow past the limit; the file is excluded, not a hard error.
    std::fs::write(&path, b"fn small_but_now_too_big() { let x = 1; }\n").unwrap();
    index.notify_change(&path).unwrap();
    index
        .commit_batch()
        .expect("oversized file must be excluded, not fail the batch");

    assert!(
        !index
            .snapshot()
            .overlay
            .docs
            .iter()
            .any(|d| d.path == std::path::Path::new("big.rs")),
        "oversized file must be excluded from the overlay"
    );
    assert_eq!(index.stats().pending_edits, 0);
    drop(index);
}

#[test]
fn commit_batch_failure_requeues_pending_edits() {
    // Regression for data-loss: a failed commit must re-queue the drained
    // edits so they survive to retry. Per-file failures no longer abort the
    // batch (they exclude the file — see
    // commit_batch_bad_file_does_not_wedge_good_files), so this exercises the
    // requeue path via a batch-level OverlayFull error.
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // One base doc: a single overlay doc already exceeds the 0.5 ratio.
    std::fs::write(repo.path().join("base.rs"), b"fn base() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    let new_path = repo.path().join("new.rs");
    std::fs::write(&new_path, b"fn new() {}\n").unwrap();
    index.notify_change(&new_path).unwrap();
    let result = index.commit_batch();
    assert!(
        matches!(result, Err(IndexError::OverlayFull { .. })),
        "commit must fail with OverlayFull (1 overlay / 1 base > 0.5): {result:?}"
    );

    // The drained edit survives for retry. OverlayFull resolves via a full
    // rebuild, not another commit_batch, so we assert survival (not re-applied).
    assert_eq!(
        index.stats().pending_edits,
        1,
        "failed commit must re-queue the drained edit so it survives to retry"
    );
    drop(index);
}

#[cfg(unix)]
#[test]
fn commit_batch_skips_symlink_escape_and_commits_rest() {
    use std::os::unix::fs::symlink;
    let _serial = serial_index_lock();

    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // Several base files so a single overlay change stays under the 0.5 ratio.
    for i in 0..6 {
        std::fs::write(
            repo.path().join(format!("base_{i}.rs")),
            format!("fn base_{i}() {{}}\n"),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // An escaping symlink plus a legitimate new file in the same batch. The
    // escape must be excluded; the legitimate file must still index. This is
    // the wedge regression: one bad path must not block unrelated edits.
    let target_outside = std::env::temp_dir().join("syntext_test_escape_target");
    std::fs::write(&target_outside, b"sensitive content").unwrap();
    let link_path = repo.path().join("escape.rs");
    symlink(&target_outside, &link_path).unwrap();
    let good_path = repo.path().join("good.rs");
    std::fs::write(&good_path, b"fn good() {}\n").unwrap();

    index.notify_change(&link_path).unwrap();
    index.notify_change(&good_path).unwrap();
    let result = index.commit_batch();

    // Clean up regardless of result.
    let _ = std::fs::remove_file(&target_outside);
    let _ = std::fs::remove_file(&link_path);

    assert!(
        result.is_ok(),
        "escape symlink must be excluded, not fail the batch: {result:?}"
    );
    assert!(
        !index
            .snapshot()
            .overlay
            .docs
            .iter()
            .any(|d| d.path == std::path::Path::new("escape.rs")),
        "escape symlink must not be indexed"
    );
    assert!(
        index
            .snapshot()
            .overlay
            .docs
            .iter()
            .any(|d| d.path == std::path::Path::new("good.rs")),
        "legitimate file in the same batch must still be indexed"
    );
    drop(index);
}

#[test]
fn commit_batch_bad_file_does_not_wedge_good_files() {
    // Headline fix for the wedge: a persistently-too-large file in a batch must
    // not prevent the other files in the same batch from committing.
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // Enough base docs that a few overlay docs stay under the 0.5 ratio.
    for i in 0..10 {
        std::fs::write(
            repo.path().join(format!("base_{i}.rs")),
            format!("fn base_{i}() {{}}\n"),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        max_file_size: 100,
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // Three good files plus one oversized file, all in one batch.
    for name in ["a.rs", "b.rs", "c.rs"] {
        let p = repo.path().join(name);
        std::fs::write(&p, b"fn x() {}\n").unwrap();
        index.notify_change(&p).unwrap();
    }
    let huge = repo.path().join("huge.rs");
    std::fs::write(&huge, "X".repeat(200)).unwrap();
    index.notify_change(&huge).unwrap();

    index
        .commit_batch()
        .expect("good files must commit even when a sibling is too large");

    let overlay_paths: std::collections::HashSet<_> = index
        .snapshot()
        .overlay
        .docs
        .iter()
        .map(|d| d.path.clone())
        .collect();
    for name in ["a.rs", "b.rs", "c.rs"] {
        assert!(
            overlay_paths.contains(std::path::Path::new(name)),
            "good file {name} must be indexed"
        );
    }
    assert!(
        !overlay_paths.contains(std::path::Path::new("huge.rs")),
        "oversized file must be excluded"
    );
    assert_eq!(index.stats().pending_edits, 0, "nothing requeued");
    drop(index);
}

#[cfg(unix)]
#[test]
fn commit_batch_accepts_symlink_target_inside_repo() {
    use std::os::unix::fs::symlink;
    let _serial = serial_index_lock();

    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    for i in 0..4 {
        std::fs::write(
            repo.path().join(format!("base_{i}.rs")),
            format!("fn base_{i}() {{}}\n"),
        )
        .unwrap();
    }
    let real = repo.path().join("real.rs");
    std::fs::write(&real, b"fn original() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    let link = repo.path().join("alias.rs");
    symlink(&real, &link).unwrap();
    std::fs::write(&real, b"fn alias_visible() {}\n").unwrap();

    index.notify_change(&link).unwrap();
    index.commit_batch().unwrap();

    let matches = index
        .search("alias_visible", &SearchOptions::default())
        .unwrap();
    assert!(
        matches
            .iter()
            .any(|m| m.path.to_string_lossy() == "alias.rs"),
        "symlink inside repo should remain indexable through commit_batch"
    );
    drop(index);
}

#[cfg(unix)]
#[test]
fn commit_batch_normalizes_paths_under_symlinked_directory() {
    use std::os::unix::fs::symlink;
    let _serial = serial_index_lock();

    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    for i in 0..4 {
        std::fs::write(
            repo.path().join(format!("base_{i}.rs")),
            format!("fn base_{i}() {{}}\n"),
        )
        .unwrap();
    }
    let real_dir = repo.path().join("real");
    std::fs::create_dir_all(&real_dir).unwrap();
    let real_file = real_dir.join("nested.rs");
    std::fs::write(&real_file, b"fn original() {}\n").unwrap();
    symlink(&real_dir, repo.path().join("alias")).unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    std::fs::write(&real_file, b"fn original() {}\nfn normalized_alias() {}\n").unwrap();
    index
        .notify_change(&repo.path().join("alias/nested.rs"))
        .unwrap();
    index.commit_batch().unwrap();

    let matches = index
        .search("normalized_alias", &SearchOptions::default())
        .unwrap();
    assert!(
        matches
            .iter()
            .any(|m| m.path.to_string_lossy() == "real/nested.rs"),
        "incremental update through a symlinked directory must update the real path entry"
    );
    assert!(
        matches
            .iter()
            .all(|m| m.path.to_string_lossy() != "alias/nested.rs"),
        "incremental update through a symlinked directory must not reintroduce alias paths"
    );
    drop(index);
}

#[cfg(unix)]
#[test]
fn commit_batch_normalizes_delete_under_symlinked_directory() {
    use std::os::unix::fs::symlink;
    let _serial = serial_index_lock();

    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    let real_dir = repo.path().join("real");
    std::fs::create_dir_all(&real_dir).unwrap();
    let real_file = real_dir.join("nested.rs");
    std::fs::write(&real_file, b"fn remove_me() {}\n").unwrap();
    symlink(&real_dir, repo.path().join("alias")).unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    std::fs::remove_file(&real_file).unwrap();
    index
        .notify_delete(&repo.path().join("alias/nested.rs"))
        .unwrap();
    index.commit_batch().unwrap();

    let matches = index
        .search("remove_me", &SearchOptions::default())
        .unwrap();
    assert!(
        matches.is_empty(),
        "delete through a symlinked directory must remove the real path entry"
    );
    drop(index);
}

// Regression test: directory-component symlink swap between canonicalize and open.
// O_NOFOLLOW only blocks the final path component; an intermediate directory
// replaced by a symlink would escape the repo without this check.
#[cfg(unix)]
#[test]
fn commit_batch_skips_intermediate_symlink_swap() {
    use std::os::unix::fs::symlink;
    let _serial = serial_index_lock();

    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();

    // Create a real directory with a file inside the repo.
    let subdir = repo.path().join("subdir");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(subdir.join("target.rs"), b"fn real() {}").unwrap();
    // Also write a base file so Index::build has at least one document.
    std::fs::write(repo.path().join("base.rs"), b"fn base() {}").unwrap();

    let config = Config {
        repo_root: repo.path().to_path_buf(),
        index_dir: index_dir.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // Notify about a file inside the real directory -- path validation passes.
    index.notify_change(&subdir.join("target.rs")).unwrap();

    // Simulate the race: replace the real directory with a symlink to outside.
    std::fs::remove_dir_all(&subdir).unwrap();
    // Place a file at the expected name in the outside dir so the open succeeds
    // if the symlink is followed (confirming the attack path would work without the fix).
    std::fs::write(outside.path().join("target.rs"), b"fn attacker() {}").unwrap();
    symlink(outside.path(), &subdir).unwrap();

    // commit_batch must detect the swap and exclude the file (not abort the
    // batch). The canonicalize check catches subdir now being a symlink to
    // outside the repo; the inode check covers the narrower race where the
    // swap happens after canonicalize but before open. Either way the escaped
    // file is not indexed.
    let result = index.commit_batch();
    assert!(
        result.is_ok(),
        "swapped path must be excluded, not fail the batch: {result:?}"
    );
    let snap = index.snapshot();
    assert!(
        !snap
            .path_index
            .paths
            .iter()
            .any(|p| p == std::path::Path::new("subdir/target.rs")),
        "intermediate symlink swap target must be excluded from the path index"
    );
    drop(index);
}

// Regression test: repo root reached via a symlink. notify_change() is given a
// canonicalized absolute path (as `st update` does), but config.repo_root is
// the non-canonical symlink path. repo_relative_path must strip against either
// form or every changed file is misreported as PathOutsideRepo.
#[cfg(unix)]
#[test]
fn notify_change_accepts_canonical_path_under_symlinked_repo_root() {
    use std::os::unix::fs::symlink;
    let _serial = serial_index_lock();

    let real_repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // Several base files so a single overlay change stays under the 50% limit.
    for i in 0..6 {
        std::fs::write(
            real_repo.path().join(format!("base_{i}.rs")),
            format!("fn base_{i}() {{}}\n"),
        )
        .unwrap();
    }
    std::fs::write(real_repo.path().join("a.rs"), b"fn alpha() {}\n").unwrap();

    // Create a symlink to the real repo elsewhere (simulates /tmp link or a
    // container bind-mount). Build the index via the symlinked root.
    let link_root = TempDir::new().unwrap();
    let symlinked_root = link_root.path().join("repo-link");
    symlink(real_repo.path(), &symlinked_root).unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: symlinked_root.clone(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // Simulate `st update`: canonicalize the changed file path before notify.
    std::fs::write(real_repo.path().join("a.rs"), b"fn alpha_updated() {}\n").unwrap();
    let changed = symlinked_root.join("a.rs");
    let canonical = changed.canonicalize().unwrap();
    // canonical is under real_repo, NOT under symlinked_root. Without the fix,
    // strip_prefix(symlinked_root) fails and notify_change returns PathOutsideRepo.
    index
        .notify_change(&canonical)
        .expect("notify_change must accept a canonical path when repo_root is a symlink");
    index.commit_batch().unwrap();

    // Sanity: the overlay picked up the edit.
    assert_eq!(index.snapshot().overlay.docs.len(), 1);
    drop(index);
}

#[test]
fn commit_batch_returns_overlay_full_when_overlay_ratio_exceeded() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    for i in 0..10 {
        std::fs::write(
            repo.path().join(format!("base_{i:03}.rs")),
            format!("fn base_{i}() {{ let x = {i}; }}\n"),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    for i in 0..6 {
        let path = repo.path().join(format!("overlay_{i:03}.rs"));
        std::fs::write(&path, format!("fn overlay_{i}() {{}}\n")).unwrap();
        index.notify_change(&path).unwrap();
    }

    let result = index.commit_batch();
    assert!(
        matches!(result, Err(IndexError::OverlayFull { .. })),
        "commit_batch must return OverlayFull when overlay exceeds 50% of base, got: {result:?}"
    );
    drop(index);
}

#[test]
fn commit_batch_binary_changes_do_not_count_toward_overlay_limit() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    for i in 0..10 {
        std::fs::write(
            repo.path().join(format!("base_{i:03}.rs")),
            format!("fn base_{i}() {{ let x = {i}; }}\n"),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    for i in 0..6 {
        let path = repo.path().join(format!("overlay_{i:03}.bin"));
        std::fs::write(&path, b"\0not indexed\n").unwrap();
        index.notify_change(&path).unwrap();
    }

    let result = index.commit_batch();
    assert!(
        result.is_ok(),
        "binary-only changes should be excluded before overlay limit check: {result:?}"
    );
    assert_eq!(
        index.snapshot().overlay.docs.len(),
        0,
        "binary-only changes must not create overlay docs"
    );
    drop(index);
}

#[test]
fn build_succeeds_and_opens_cleanly() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("lib.rs"), b"fn f() {}").unwrap();
    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let result = Index::build(config);
    assert!(result.is_ok(), "build() must succeed: {:?}", result.err());
    drop(result.unwrap());
}

#[cfg(unix)]
#[test]
fn open_rejects_permissive_index_dir_mode() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = serial_index_lock();

    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("lib.rs"), b"fn f() {}").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    Index::build(config).unwrap();

    std::fs::set_permissions(index_dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        strict_permissions: true,
        ..Config::default()
    };
    let result = Index::open(config);
    match &result {
        Err(IndexError::CorruptIndex(msg)) => {
            assert!(
                msg.contains("0755"),
                "error message should mention mode 0755: {msg}"
            );
        }
        Err(e) => panic!("expected CorruptIndex, got: {e}"),
        Ok(_) => panic!("open() must reject permissive dir mode"),
    }
}

#[test]
fn build_index_returns_valid_index_without_lock_gap() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("lib.rs"), b"fn f() {}").unwrap();
    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let snap = index.snapshot();
    assert!(
        snap.base_segments()
            .iter()
            .map(|s| s.doc_count)
            .sum::<u32>()
            > 0
    );
    drop(index);
}

#[test]
fn maintenance_apis_are_noops_when_no_work_is_needed() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    std::fs::write(repo.path().join("main.rs"), b"fn main() {}\n").unwrap();
    init_git_repo(repo.path());
    commit_all(repo.path(), "initial");

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    assert!(!index.maybe_compact().unwrap());
    index.compact().unwrap();
    assert!(index.rebuild_if_stale().unwrap().is_none());
    drop(index);
}

#[cfg(unix)]
#[test]
fn update_from_git_classifies_dangling_symlink_as_change_not_delete() {
    // Regression for the abs.exists()-based misclassification: exists()
    // follows symlinks, so a symlink whose target does not exist reports as
    // absent, causing update_from_git to call notify_delete() for a path git
    // reports as merely changed. The fix uses symlink_metadata() (which does
    // not follow the link) to decide presence, so a dangling symlink takes
    // the "changed" branch, not the "deleted" branch.
    //
    // Both branches are mutually exclusive in the source (`if present {
    // notify_change-or-skip } else { notify_delete-or-skip }`), so a
    // dangling symlink resulting in `Updated { files: 1, skipped: 0 }` can
    // only happen via the notify_change branch succeeding: the notify_delete
    // branch is unreachable when `present` is true.
    use crate::index::freshness::{UpdateLimits, UpdateOutcome};

    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    std::fs::write(repo.path().join("main.rs"), b"fn main() {}\n").unwrap();
    init_git_repo(repo.path());
    commit_all(repo.path(), "initial");

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // A symlink whose target does not exist. Left untracked so `git
    // ls-files --others` reports it as a detected change.
    std::os::unix::fs::symlink(
        "this-target-does-not-exist",
        repo.path().join("dangling.rs"),
    )
    .unwrap();
    assert!(
        std::fs::symlink_metadata(repo.path().join("dangling.rs")).is_ok(),
        "premise check: the symlink entry itself must exist (lstat succeeds)"
    );
    assert!(
        !repo.path().join("dangling.rs").exists(),
        "premise check: exists() must report absent (it follows the broken symlink)"
    );

    let limits = UpdateLimits {
        max_files: None,
        budget_ms: None,
    };
    let outcome = index.update_from_git(limits).unwrap();

    match outcome {
        UpdateOutcome::Updated { files, skipped, .. } => {
            assert_eq!(
                files, 1,
                "the dangling symlink must be applied via notify_change (present branch)"
            );
            assert_eq!(
                skipped, 0,
                "the dangling symlink must not be skipped or misrouted to notify_delete"
            );
        }
        other => panic!("expected Updated, got {other:?}"),
    }
    drop(index);
}

/// Bug 10 regression: a *bounded* `update_from_git` (the search hot path) must
/// never do a synchronous full rebuild when the overlay would exceed its cap.
/// It returns `OverlayFull` (so the caller searches stale and spawns the async
/// catch-up) instead of blocking the latency budget on a `build_index`. The
/// *unbounded* call (`st update`) still rebuilds inline.
#[test]
fn bounded_update_returns_overlay_full_without_inline_rebuild() {
    use crate::index::freshness::{UpdateLimits, UpdateOutcome};

    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // Base = 1 doc. Two untracked files push the projected overlay to 2/1 =
    // 200% of base, well over the 50% OVERLAY_ENFORCE_THRESHOLD.
    std::fs::write(repo.path().join("main.rs"), b"fn main() {}\n").unwrap();
    init_git_repo(repo.path());
    commit_all(repo.path(), "initial");

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let base_docs = index.stats().total_documents;

    // Enough untracked files that the overlay would exceed 50% of the base
    // (build also indexes the repo's own tracked files), tripping OverlayFull.
    let n_new = (base_docs as usize) + 4;
    for i in 0..n_new {
        std::fs::write(
            repo.path().join(format!("added_{i:04}.rs")),
            format!("fn marker_{i:04}() {{}}\n"),
        )
        .unwrap();
    }

    // Bounded: max_files high enough to avoid TooManyFiles, so it reaches the
    // commit_batch OverlayFull path.
    let bounded = UpdateLimits {
        max_files: Some(10_000),
        budget_ms: Some(30_000),
    };
    match index.update_from_git(bounded).unwrap() {
        UpdateOutcome::OverlayFull { files_behind, .. } => {
            assert!(files_behind > 0, "the untracked files are behind");
        }
        other => panic!("bounded update must return OverlayFull, got {other:?}"),
    }
    // Not applied: the stale index does not yet see the new markers.
    assert!(
        index
            .search("marker_0000", &SearchOptions::default())
            .unwrap()
            .is_empty(),
        "bounded OverlayFull must not have applied changes"
    );

    // Unbounded (CLI `st update`): rebuilds inline and picks the files up.
    let unbounded = UpdateLimits {
        max_files: None,
        budget_ms: None,
    };
    match index.update_from_git(unbounded).unwrap() {
        UpdateOutcome::Updated { .. } => {}
        other => panic!("unbounded update must rebuild and Update, got {other:?}"),
    }
    assert!(
        index
            .search("marker_0000", &SearchOptions::default())
            .unwrap()
            .iter()
            .any(|m| m.path == std::path::Path::new("added_0000.rs")),
        "unbounded rebuild must include the new content"
    );
    drop(index);
}

#[test]
fn search_fresh_no_git_changes_returns_no_changes_and_stale_results() {
    // No changes since the index was built: search_fresh must report
    // UpdateOutcome::NoChanges and return the same (already-correct, since
    // nothing changed) results as a plain `search` call would.
    use crate::index::freshness::UpdateLimits;

    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    std::fs::write(repo.path().join("main.rs"), b"fn stale_marker() {}\n").unwrap();
    init_git_repo(repo.path());
    commit_all(repo.path(), "initial");

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    let limits = UpdateLimits {
        max_files: None,
        budget_ms: None,
    };
    let (matches, outcome) = index
        .search_fresh("stale_marker", &SearchOptions::default(), limits)
        .unwrap();

    assert!(
        matches!(outcome, UpdateOutcome::NoChanges { .. }),
        "expected NoChanges with no git activity since build, got {outcome:?}"
    );
    assert_eq!(
        matches.len(),
        1,
        "the already-indexed content must still be found"
    );
    drop(index);
}

#[test]
fn search_fresh_picks_up_changed_file_and_returns_updated_outcome() {
    // A file changed on disk after the index was built (but not yet
    // re-indexed) must be picked up by search_fresh's bounded update before
    // searching: the new content should be matched and UpdateOutcome::Updated
    // returned, not the stale pre-change content.
    use crate::index::freshness::UpdateLimits;

    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    std::fs::write(repo.path().join("main.rs"), b"fn old_marker() {}\n").unwrap();
    init_git_repo(repo.path());
    commit_all(repo.path(), "initial");

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // Sanity: the new marker must not yet be searchable before the change is
    // written, otherwise the test wouldn't prove search_fresh did the update.
    assert_eq!(
        index
            .search("new_marker", &SearchOptions::default())
            .unwrap()
            .len(),
        0,
        "premise check: new_marker must be absent before the file is changed"
    );

    // Modify the file on disk without going through notify_change/commit_batch:
    // only `git diff HEAD` should reveal this to update_from_git.
    std::fs::write(repo.path().join("main.rs"), b"fn new_marker() {}\n").unwrap();

    let limits = UpdateLimits {
        max_files: None,
        budget_ms: None,
    };
    let (matches, outcome) = index
        .search_fresh("new_marker", &SearchOptions::default(), limits)
        .unwrap();

    match outcome {
        UpdateOutcome::Updated { files, .. } => {
            assert_eq!(files, 1, "exactly the one changed file must be applied");
        }
        other => panic!("expected Updated, got {other:?}"),
    }
    assert_eq!(
        matches.len(),
        1,
        "search_fresh must match the new content after the bounded update"
    );
    drop(index);
}

#[cfg(unix)]
#[test]
fn open_allows_permissive_mode_when_strict_permissions_disabled() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = serial_index_lock();

    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("lib.rs"), b"fn f() {}").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    Index::build(config).unwrap();

    std::fs::set_permissions(index_dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        strict_permissions: false,
        ..Config::default()
    };
    let result = Index::open(config);
    assert!(
        result.is_ok(),
        "open() must succeed when strict_permissions is false, got: {}",
        result.err().map(|e| e.to_string()).unwrap_or_default()
    );
    drop(result.unwrap());
}

#[test]
fn compact_reduces_segment_count_to_config_limit() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    for i in 0..6 {
        std::fs::write(
            repo.path().join(format!("file_{i}.rs")),
            format!("fn marker_{i}() {{ println!(\"{i}\"); }}\n"),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        max_segments: 2,
        ..Config::default()
    };
    let index = build::build_index_with_batch_size(config, 1).unwrap();
    assert!(
        index.stats().total_segments > 2,
        "test fixture must start fragmented"
    );

    index.compact().unwrap();

    let stats = index.stats();
    assert!(
        stats.total_segments <= 2,
        "compact() must reduce segment count to config.max_segments, got {}",
        stats.total_segments
    );
    assert!(
        index
            .search("marker_5", &SearchOptions::default())
            .unwrap()
            .iter()
            .any(|m| m.path == std::path::Path::new("file_5.rs")),
        "search results must survive compaction"
    );
    assert_eq!(index.snapshot().overlay.docs.len(), 0);
}

#[test]
fn compact_preserves_untouched_prefix_segments_in_manifest() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    for i in 0..4 {
        std::fs::write(
            repo.path().join(format!("file_{i}.rs")),
            format!("fn marker_{i}() {{ println!(\"{i}\"); }}\n"),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        max_segments: 3,
        ..Config::default()
    };
    let index = build::build_index_with_batch_size(config.clone(), 1).unwrap();
    let before = Manifest::load(&config.index_dir).unwrap();
    assert_eq!(
        before.segments.len(),
        4,
        "fixture must begin with four segments"
    );

    index.compact().unwrap();

    let after = Manifest::load(&config.index_dir).unwrap();
    assert_eq!(
        after.segments.len(),
        3,
        "selective compaction should rewrite only the suffix"
    );
    assert_eq!(after.segments[0].segment_id, before.segments[0].segment_id);
    assert_eq!(after.segments[1].segment_id, before.segments[1].segment_id);
    assert_ne!(after.segments[2].segment_id, before.segments[2].segment_id);
    drop(index);
}

#[test]
fn compact_preserves_actual_total_files_for_gapped_prefix_manifest() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let config = write_sparse_manifest_index(repo.path(), index_dir.path());
    let index = Index::open(config.clone()).unwrap();

    index.compact().unwrap();

    let manifest = Manifest::load(&config.index_dir).unwrap();
    assert_eq!(
            manifest.total_files_indexed, 2,
            "compact() must record actual live file count, not max doc_id + 1, when base ranges are sparse"
        );
    assert_eq!(
            manifest.total_docs(),
            manifest.total_files_indexed,
            "manifest doc_count sum and reported total files should stay aligned after gapped compaction"
        );
    drop(index);
}

#[test]
fn maybe_compact_rebuilds_when_overlay_ratio_exceeds_threshold() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    init_git_repo(repo.path());
    for i in 0..10 {
        std::fs::write(
            repo.path().join(format!("base_{i}.rs")),
            format!("fn base_{i}() {{}}\n"),
        )
        .unwrap();
    }
    commit_all(repo.path(), "initial");

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        max_segments: 10,
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    for i in 0..4 {
        let path = repo.path().join(format!("base_{i}.rs"));
        std::fs::write(&path, format!("fn updated_{i}() {{}}\n")).unwrap();
        index.notify_change(&path).unwrap();
    }
    index.commit_batch().unwrap();
    assert_eq!(index.snapshot().overlay.docs.len(), 4);

    let snap = index.snapshot();
    let base_docs: usize = snap
        .base_segments()
        .iter()
        .map(|s| s.doc_count as usize)
        .sum();
    let overlay_docs = snap.overlay.docs.len();
    let total_segments = snap.base.segments.len();
    drop(snap);

    assert!(
            index.maybe_compact().unwrap(),
            "overlay ratio > 10% should compact (base_docs={base_docs}, overlay_docs={overlay_docs}, total_segments={total_segments})"
        );
    assert_eq!(
        index.snapshot().overlay.docs.len(),
        0,
        "compaction must fold overlay docs back into the base index"
    );
    assert!(
        index
            .search("updated_1", &SearchOptions::default())
            .unwrap()
            .iter()
            .any(|m| m.path == std::path::Path::new("base_1.rs")),
        "compaction must preserve the updated working tree content"
    );
    drop(index);
}

#[test]
fn compact_preserves_base_snapshot_when_working_tree_drifts() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    let path = repo.path().join("tracked.rs");
    std::fs::write(&path, "fn alpha() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let relative = std::path::Path::new("tracked.rs");
    let alpha_hash = xxh64(b"fn alpha() {}\n", 0);
    let beta_hash = xxh64(b"fn beta() {}\n", 0);
    assert_eq!(base_doc_hash(&index, relative), Some(alpha_hash));

    std::fs::write(&path, "fn beta() {}\n").unwrap();
    index.compact().unwrap();

    assert_eq!(
            base_doc_hash(&index, relative),
            Some(alpha_hash),
            "compact() must preserve the indexed base snapshot, not reread unrelated working tree changes"
        );
    assert!(
        base_doc_hash(&index, relative) != Some(beta_hash),
        "compact() must not absorb uncommitted working tree edits into base metadata"
    );
    drop(index);
}

#[test]
fn compact_folds_overlay_snapshot_without_rereading_disk() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    for i in 0..10 {
        std::fs::write(
            repo.path().join(format!("tracked_{i}.rs")),
            format!("fn alpha_{i}() {{}}\n"),
        )
        .unwrap();
    }
    let path = repo.path().join("tracked_0.rs");

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let relative = std::path::Path::new("tracked_0.rs");
    let bravo_hash = xxh64(b"fn bravo() {}\n", 0);
    let charlie_hash = xxh64(b"fn charlie() {}\n", 0);

    std::fs::write(&path, "fn bravo() {}\n").unwrap();
    index.notify_change(&path).unwrap();
    index.commit_batch().unwrap();

    std::fs::write(&path, "fn charlie() {}\n").unwrap();
    index.compact().unwrap();

    assert_eq!(
        base_doc_hash(&index, relative),
        Some(bravo_hash),
        "compact() must fold the committed overlay snapshot into base segments"
    );
    assert!(
        base_doc_hash(&index, relative) != Some(charlie_hash),
        "compact() must not reread newer uncommitted disk content while folding overlay docs"
    );
    assert_eq!(index.snapshot().overlay.docs.len(), 0);
    drop(index);
}

#[test]
fn rebuild_if_stale_refreshes_snapshot_after_head_change() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    init_git_repo(repo.path());
    let file = repo.path().join("main.rs");
    std::fs::write(&file, b"fn old_name() {}\n").unwrap();
    let first_head = commit_all(repo.path(), "first");

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    assert_eq!(
        index.stats().base_commit.as_deref(),
        Some(first_head.as_str())
    );

    std::fs::write(&file, b"fn new_name() {}\n").unwrap();
    let second_head = commit_all(repo.path(), "second");

    let (stats, _full) = index
        .rebuild_if_stale()
        .unwrap()
        .expect("HEAD changed, rebuild must run");
    assert_eq!(stats.base_commit.as_deref(), Some(second_head.as_str()));
    assert!(
        index
            .search("new_name", &SearchOptions::default())
            .unwrap()
            .iter()
            .any(|m| m.path == std::path::Path::new("main.rs")),
        "rebuilt snapshot must include the new committed content"
    );
    assert!(
        index
            .search("old_name", &SearchOptions::default())
            .unwrap()
            .is_empty(),
        "rebuilt snapshot must stop returning content from the old HEAD"
    );
    assert_eq!(index.stats().pending_edits, 0);
    drop(index);
}

#[test]
fn base_doc_id_limit_overflow_returns_error() {
    // B01: base_doc_id_limit must return Err when base + doc_count
    // overflows u32, not silently drop via filter_map.
    //
    // In practice MAX_TOTAL_DOCS (50M) prevents near-u32::MAX base_ids
    // from being loaded via open(), so this is defense in depth. We test
    // the function directly via a crafted IndexSnapshot.
    use crate::index::snapshot::{new_snapshot, BaseSegments};

    let _serial = serial_index_lock();
    let index_dir = TempDir::new().unwrap();

    // Create a real segment file (doc_count=1).
    let seg_ref = write_segment_with_global_doc_id(index_dir.path(), 0, "a.rs", b"fn alpha() {}\n");
    let seg_file = index_dir.path().join(&seg_ref.dict_filename);
    let seg = crate::index::segment::MmapSegment::open(&seg_file).unwrap();
    assert_eq!(seg.doc_count, 1);

    // Set base_id = u32::MAX so base + doc_count(1) overflows.
    let base = Arc::new(BaseSegments {
        segments: vec![seg],
        base_ids: vec![u32::MAX],
        base_doc_paths: vec![],
        path_doc_ids: std::collections::HashMap::new(),
        base_doc_to_file_id: std::sync::OnceLock::new(),
    });
    let snap = new_snapshot(
        base,
        crate::index::overlay::OverlayView::empty(),
        roaring::RoaringBitmap::new(),
        crate::path::PathIndex::build(&[]),
        std::collections::HashMap::new(),
        0.10,
    );
    let result = helpers::base_doc_id_limit(&snap);
    assert!(
        result.is_err(),
        "base_doc_id_limit must return Err on overflow, not silently drop"
    );
}

#[test]
fn overlapping_base_doc_ids_rejected_on_open() {
    // B04: two segments with overlapping [base_id, base_id + doc_count)
    // ranges must be rejected as CorruptIndex on open.
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("a.rs"), b"fn alpha() {}\n").unwrap();
    std::fs::write(repo.path().join("b.rs"), b"fn beta() {}\n").unwrap();

    // Segment A: base_doc_id=0, doc_count=1 -> range [0, 1)
    let seg_a = write_segment_with_global_doc_id(index_dir.path(), 0, "a.rs", b"fn alpha() {}\n");
    // Segment B: base_doc_id=0, doc_count=1 -> range [0, 1) -- overlaps A
    let seg_b = write_segment_with_global_doc_id(index_dir.path(), 0, "b.rs", b"fn beta() {}\n");

    let mut manifest = crate::index::manifest::Manifest::new(vec![seg_a, seg_b], 2);
    manifest.scan_threshold_fraction = Some(0.10);
    manifest.save(index_dir.path()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(index_dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let result = Index::open(config);
    assert!(
        result.is_err(),
        "open must reject overlapping base_doc_id ranges"
    );
    let err_msg = match result {
        Err(e) => format!("{e}"),
        Ok(_) => panic!("expected error"),
    };
    assert!(
        err_msg.contains("regresses") || err_msg.contains("CorruptIndex"),
        "error should indicate corrupt/overlapping segments, got: {err_msg}"
    );
}

#[test]
fn commit_batch_max_file_size_saturates_not_wraps() {
    // Verify that the take() sentinel does not wrap to 0 for u64::MAX.
    // saturating_add(1) stays at u64::MAX; plain + 1 would wrap to 0.
    let sentinel = u64::MAX.saturating_add(1);
    assert_eq!(sentinel, u64::MAX, "saturating_add must not wrap");
    assert_ne!(sentinel, 0u64, "must not wrap to 0");
}

#[test]
fn open_rejects_truncated_post_file() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("a.rs"), b"fn alpha_one() {}\n").unwrap();
    std::fs::write(repo.path().join("b.rs"), b"fn beta_two() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config.clone()).unwrap();
    drop(index);

    // Truncate the .post file by one byte: the manifest's post_len no longer
    // matches, so open must fail before any postings are read.
    let post_path = std::fs::read_dir(index_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "post"))
        .expect("built index must contain a .post file");
    let bytes = std::fs::read(&post_path).unwrap();
    std::fs::write(&post_path, &bytes[..bytes.len() - 1]).unwrap();

    let result = Index::open(config);
    assert!(result.is_err(), "open must reject a truncated .post file");
}

#[test]
fn structural_default_open_survives_postings_corruption_and_full_verify_detects_it() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    for i in 0..10 {
        std::fs::write(
            repo.path().join(format!("file_{i}.rs")),
            format!("fn corrupt_probe_{i}() {{ let value = {i}; }}\n"),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config.clone()).unwrap();
    drop(index);

    // Flip one byte in the middle of the postings data, keeping the length
    // unchanged so only checksum verification can detect it.
    let post_path = std::fs::read_dir(index_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "post"))
        .expect("built index must contain a .post file");
    let mut bytes = std::fs::read(&post_path).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&post_path, &bytes).unwrap();

    // Default (structural) open succeeds; searches degrade gracefully
    // (possibly missing results) but must not panic, and never return
    // fabricated content because candidates are verified against file bytes.
    let index = Index::open(config.clone()).unwrap();
    let _ = index.search("corrupt_probe_3", &SearchOptions::default());
    assert!(
        index.verify().is_err(),
        "Index::verify must detect the flipped postings byte"
    );
    drop(index);

    // Paranoid mode must reject the corrupted index at open time.
    let strict_config = Config {
        verify_on_open: true,
        ..config
    };
    let result = Index::open(strict_config);
    assert!(
        result.is_err(),
        "verify_on_open must reject a corrupted .post file"
    );
}

#[test]
fn rebuild_reuses_prior_calibrated_threshold_unless_recalibrate() {
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    for i in 0..50 {
        std::fs::write(
            repo.path().join(format!("file_{i:03}.rs")),
            format!("fn func_{i}() {{ let x = {i}; }}\n").repeat(20),
        )
        .unwrap();
    }

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config.clone()).unwrap();
    drop(index);
    let first = crate::index::manifest::Manifest::load(&config.index_dir)
        .unwrap()
        .scan_threshold_fraction
        .expect("first build must calibrate");

    // Repeat build: must carry the first build's threshold forward verbatim.
    let index = Index::build(config.clone()).unwrap();
    drop(index);
    let second = crate::index::manifest::Manifest::load(&config.index_dir)
        .unwrap()
        .scan_threshold_fraction
        .expect("rebuild must persist a threshold");
    assert_eq!(
        second, first,
        "rebuild without --recalibrate must reuse the prior threshold"
    );

    // Forced recalibration still produces a valid clamped value.
    let recal_config = Config {
        recalibrate: true,
        ..config.clone()
    };
    let index = Index::build(recal_config).unwrap();
    drop(index);
    let third = crate::index::manifest::Manifest::load(&config.index_dir)
        .unwrap()
        .scan_threshold_fraction
        .expect("recalibrated build must persist a threshold");
    assert!(
        (0.01..=0.50).contains(&third),
        "recalibrated threshold {third} must be in [0.01, 0.50]"
    );
}

#[test]
fn build_summary_omits_skips_when_none() {
    assert_eq!(
        crate::index::helpers::format_build_summary(10, 1, 0, 0, 0),
        "syntext: indexed 10 files into 1 segment(s)"
    );
}

#[test]
fn build_summary_breaks_down_skips_by_reason() {
    assert_eq!(
        crate::index::helpers::format_build_summary(3802, 1, 240, 20, 3),
        "syntext: indexed 3802 files into 1 segment(s) \
         (skipped 263: 240 binary, 20 unreadable, 3 too large)"
    );
    assert_eq!(
        crate::index::helpers::format_build_summary(5, 2, 0, 0, 1),
        "syntext: indexed 5 files into 2 segment(s) (skipped 1: 1 too large)"
    );
}

#[test]
fn open_missing_index_dir_reports_index_not_found() {
    let repo = tempfile::TempDir::new().unwrap();
    let parent = tempfile::TempDir::new().unwrap();
    let config = Config {
        repo_root: repo.path().to_path_buf(),
        index_dir: parent.path().join("no-such-index"),
        ..Config::default()
    };
    match Index::open(config) {
        Ok(_) => panic!("open must fail when the index dir does not exist"),
        Err(IndexError::IndexNotFound(p)) => {
            assert_eq!(p, parent.path().join("no-such-index"));
        }
        Err(other) => panic!("expected IndexNotFound, got: {other}"),
    }
}

#[test]
fn base_doc_to_file_id_stays_lazy_until_path_filter_needs_it() {
    // Confirms the OnceLock in BaseSegments is not populated at build/open,
    // stays unbuilt for plain search and for --files-style path listing
    // (both of which can be served without it), and is only built the first
    // time a query actually applies a path/type filter.
    let _serial = serial_index_lock();
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    std::fs::write(repo.path().join("alpha.rs"), b"fn alpha() {}\n").unwrap();
    std::fs::write(repo.path().join("beta.py"), b"def beta(): pass\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();

    // Not built at open/build time.
    assert!(
        index.snapshot().base.base_doc_to_file_id.get().is_none(),
        "base_doc_to_file_id must not be built at open/build"
    );

    // A plain search with no path/type filter must not build it either: the
    // filter bitmap is None, so search/mod.rs never calls the accessor.
    let matches = index.search("alpha", &SearchOptions::default()).unwrap();
    assert_eq!(matches.len(), 1);
    assert!(
        index.snapshot().base.base_doc_to_file_id.get().is_none(),
        "base_doc_to_file_id must not be built by an unfiltered search"
    );

    // --files-style listing walks PathIndex::visible_paths() directly and
    // never touches base_doc_to_file_id.
    let snap = index.snapshot();
    let listed: Vec<_> = snap.path_index.visible_paths().collect();
    assert_eq!(listed.len(), 2);
    assert!(
        index.snapshot().base.base_doc_to_file_id.get().is_none(),
        "path listing (--files) must not build base_doc_to_file_id"
    );

    // A search with a file-type filter goes through the path-filter branch,
    // which is the one caller of the lazy accessor: it must build it now.
    let opts = SearchOptions {
        file_type: Some("rs".to_string()),
        ..SearchOptions::default()
    };
    let matches = index.search("alpha", &opts).unwrap();
    assert_eq!(matches.len(), 1);
    assert!(
        index.snapshot().base.base_doc_to_file_id.get().is_some(),
        "base_doc_to_file_id must be built once a path/type filter is applied"
    );
    drop(index);
}
