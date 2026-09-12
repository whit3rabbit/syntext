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

    let (files, _) = enumerate_files(&config).unwrap();
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

    let (files, _) = enumerate_files(&config).unwrap();
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

    let (files, _) = enumerate_files(&config).unwrap();
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

    let (files, _) = enumerate_files(&config).unwrap();
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

    let (files, _) = enumerate_files(&config).unwrap();

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

#[test]
fn enumerate_files_counts_too_large_skips() {
    use crate::Config;

    let repo = tempfile::TempDir::new().unwrap();
    std::fs::write(repo.path().join("small.rs"), b"fn ok() {}\n").unwrap();
    std::fs::write(repo.path().join("big.rs"), vec![b'a'; 64]).unwrap();

    let config = Config {
        repo_root: repo.path().to_path_buf(),
        max_file_size: 32,
        ..Config::default()
    };

    let (files, skips) = super::enumerate_files(&config).unwrap();
    assert_eq!(skips.too_large, 1, "one file exceeds the 32-byte cap");
    assert_eq!(
        files.iter().map(|(_, rel, _)| rel).collect::<Vec<_>>(),
        vec![&std::path::PathBuf::from("small.rs")],
        "oversized file must be excluded from the candidate list"
    );
}

/// A repo root with a `.git` directory, one tracked file, and a nested
/// checkout at `nested/` whose `.git` is written by `make_dot_git`.
fn repo_with_nested_checkout(make_dot_git: impl Fn(&std::path::Path)) -> tempfile::TempDir {
    let repo = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    std::fs::write(
        repo.path().join(".git").join("HEAD"),
        "ref: refs/heads/main\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("outer.rs"), b"fn outer() {}\n").unwrap();

    let nested = repo.path().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("inner.rs"), b"fn inner() {}\n").unwrap();
    make_dot_git(&nested);
    repo
}

fn rel_paths(files: &[super::FileRecord]) -> Vec<String> {
    files
        .iter()
        .map(|(_, rel, _)| rel.display().to_string())
        .collect()
}

#[test]
fn nested_linked_worktree_is_pruned() {
    use crate::git_checkout::CheckoutKind;
    use crate::Config;

    let repo = repo_with_nested_checkout(|nested| {
        std::fs::write(
            nested.join(".git"),
            "gitdir: /elsewhere/.git/worktrees/foo\n",
        )
        .unwrap();
    });
    let config = Config {
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };

    let (files, skips) = super::enumerate_files(&config).unwrap();
    let paths = rel_paths(&files);

    assert!(
        !paths.iter().any(|p| p.starts_with("nested/")),
        "a linked worktree's files must not be indexed by the outer repo: {paths:?}"
    );
    assert!(paths.contains(&"outer.rs".to_string()));
    assert_eq!(
        skips.nested_checkouts,
        vec![(std::path::PathBuf::from("nested"), CheckoutKind::Linked)],
    );
}

#[test]
fn nested_clone_with_a_git_directory_is_pruned() {
    use crate::git_checkout::CheckoutKind;
    use crate::Config;

    let repo = repo_with_nested_checkout(|nested| {
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        std::fs::write(nested.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
    });
    let config = Config {
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };

    let (files, skips) = super::enumerate_files(&config).unwrap();
    let paths = rel_paths(&files);

    assert!(
        !paths.iter().any(|p| p.starts_with("nested/")),
        "a nested clone's files must not be indexed by the outer repo: {paths:?}"
    );
    assert_eq!(
        skips.nested_checkouts,
        vec![(
            std::path::PathBuf::from("nested"),
            CheckoutKind::NestedClone
        )],
    );
}

#[test]
fn the_repos_own_git_directory_is_still_walked() {
    use crate::Config;

    // Regression guard. The prune predicate asks "does this directory contain
    // a `.git`?", so two things must stay true: the walk root itself is never
    // pruned (it always has one), and `.git/` is not pruned either (there is
    // no `.git/.git`). `Index::build`'s document count depends on the latter,
    // per the `.git/hooks/*.sample` note in CLAUDE.md.
    let repo = repo_with_nested_checkout(|nested| {
        std::fs::write(nested.join(".git"), "gitdir: /elsewhere\n").unwrap();
    });
    std::fs::create_dir_all(repo.path().join(".git").join("hooks")).unwrap();
    std::fs::write(
        repo.path()
            .join(".git")
            .join("hooks")
            .join("pre-commit.sample"),
        b"#!/bin/sh\nexit 0\n",
    )
    .unwrap();

    let config = Config {
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };

    let paths = rel_paths(&super::enumerate_files(&config).unwrap().0);
    assert!(
        paths.contains(&".git/hooks/pre-commit.sample".to_string()),
        "the repo's own .git must still be walked: {paths:?}"
    );
    assert!(paths.contains(&"outer.rs".to_string()));
}

#[test]
fn index_nested_checkouts_opts_back_in() {
    use crate::Config;

    let repo = repo_with_nested_checkout(|nested| {
        std::fs::write(
            nested.join(".git"),
            "gitdir: /elsewhere/.git/worktrees/foo\n",
        )
        .unwrap();
    });
    let config = Config {
        repo_root: repo.path().to_path_buf(),
        index_nested_checkouts: true,
        ..Config::default()
    };

    let (files, skips) = super::enumerate_files(&config).unwrap();
    let paths = rel_paths(&files);

    assert!(
        paths.contains(&"nested/inner.rs".to_string()),
        "--index-nested must index the subtree: {paths:?}"
    );
    assert!(
        skips.nested_checkouts.is_empty(),
        "nothing was skipped, so nothing should be reported"
    );
}
