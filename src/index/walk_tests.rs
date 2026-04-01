#[cfg(unix)]
use super::*;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn enumerate_files_skips_symlinked_directories() {
    use std::os::unix::fs::symlink;

    let repo = TempDir::new().unwrap();
    let real_dir = repo.path().join("real");
    fs::create_dir_all(&real_dir).unwrap();
    fs::write(real_dir.join("nested.rs"), b"fn linked() {}\n").unwrap();
    symlink(&real_dir, repo.path().join("alias")).unwrap();

    let config = Config {
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };

    let files = enumerate_files(&config).unwrap();
    assert_eq!(
        files.iter().map(|(_, rel, _)| rel).collect::<Vec<_>>(),
        vec![&PathBuf::from("real/nested.rs")],
        "directory symlink contents must not be indexed through alias paths"
    );
}

#[cfg(unix)]
#[test]
fn collect_symlink_entry_rejects_canonical_symlink() {
    // Simulates: symlink in repo -> dir inside repo, but that dir was
    // replaced with a symlink to an outside location (post-canonicalize race).
    // We test the defense: after canonicalize, if the result is itself a
    // symlink, it must be rejected.
    use std::os::unix::fs::symlink;

    let repo = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    // real file outside repo
    std::fs::write(outside.path().join("secret.rs"), b"secret").unwrap();

    // link_b inside repo -> outside/secret.rs (so canonical_target is outside root)
    symlink(outside.path().join("secret.rs"), repo.path().join("link_b")).unwrap();

    // link_a -> link_b (a chain: canonicalize of link_a resolves to outside/secret.rs)
    symlink(repo.path().join("link_b"), repo.path().join("link_a")).unwrap();

    let config = crate::Config {
        repo_root: repo.path().to_path_buf(),
        ..crate::Config::default()
    };

    let files = enumerate_files(&config).unwrap();
    // Neither link_a nor link_b should appear in results (both lead outside repo).
    let found: Vec<_> = files
        .iter()
        .filter(|(_, rel, _)| rel.starts_with("link_a") || rel.starts_with("link_b"))
        .collect();
    assert!(
        found.is_empty(),
        "symlinks pointing outside repo must be rejected, found: {:?}",
        found
    );
}

#[cfg(unix)]
#[test]
fn enumerate_files_skips_symlink_outside_repo() {
    use std::os::unix::fs::symlink;

    let repo = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret.rs"), b"fn secret() {}\n").unwrap();
    symlink(
        outside.path().join("secret.rs"),
        repo.path().join("escape.rs"),
    )
    .unwrap();

    let config = Config {
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };

    let files = enumerate_files(&config).unwrap();
    assert!(
        !files.iter().any(|(_, rel, _)| rel == "escape.rs"),
        "out-of-repo symlink targets must be skipped"
    );
}

#[cfg(unix)]
#[test]
fn enumerate_files_deduplicates_multiple_symlinks_to_same_file() {
    use std::os::unix::fs::symlink;

    let repo = TempDir::new().unwrap();
    let real = repo.path().join("real.rs");
    fs::write(&real, b"fn visible() {}\n").unwrap();
    for i in 0..10u8 {
        symlink(&real, repo.path().join(format!("alias{i}.rs"))).unwrap();
    }

    let config = Config {
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };

    let files = enumerate_files(&config).unwrap();
    let symlinked_files: Vec<_> = files
        .iter()
        .filter(|(_, rel, _)| rel.to_str().unwrap_or("").starts_with("alias"))
        .collect();
    // The real file is indexed; all symlink aliases must be suppressed.
    assert!(
        symlinked_files.is_empty(),
        "symlink aliases to an already-indexed real file must not appear in results, got: {:?}",
        symlinked_files
            .iter()
            .map(|(_, r, _)| r)
            .collect::<Vec<_>>()
    );
    let real_files: Vec<_> = files
        .iter()
        .filter(|(_, rel, _)| rel.to_str().unwrap_or("") == "real.rs")
        .collect();
    assert_eq!(
        real_files.len(),
        1,
        "the real file must appear exactly once"
    );
}

#[cfg(unix)]
#[test]
fn enumerate_files_real_file_wins_over_symlink_alias() {
    use std::os::unix::fs::symlink;

    let repo = TempDir::new().unwrap();
    let real = repo.path().join("real.rs");
    fs::write(&real, b"fn original() {}\n").unwrap();
    // Create a symlink that points to the real file.
    symlink(&real, repo.path().join("alias.rs")).unwrap();

    let config = Config {
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };

    let files = enumerate_files(&config).unwrap();

    // Only one entry should exist (the real file).
    assert_eq!(
        files.len(),
        1,
        "real file + symlink to it must produce exactly one index entry, got: {:?}",
        files.iter().map(|(_, r, _)| r).collect::<Vec<_>>()
    );
    assert_eq!(
        files[0].1,
        std::path::PathBuf::from("real.rs"),
        "the surviving entry must be the real file, not the symlink"
    );
}
