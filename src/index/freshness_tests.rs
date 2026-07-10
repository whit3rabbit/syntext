use super::*;
use std::fs;

/// Build a minimal git repo in a temp dir and return the repo path.
fn init_git_repo() -> tempfile::TempDir {
    let repo = tempfile::TempDir::new().unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["init"])
        .output()
        .unwrap();
    repo
}

#[test]
fn detect_changed_files_empty_repo_returns_none() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return; // skip on systems without git
    }
    let canonical = repo.path().canonicalize().unwrap();
    let result = detect_changed_files(&canonical, &git, None).unwrap();
    // No files exist, so nothing to detect.
    assert!(result.paths.is_empty());
    assert!(result.budget_exceeded.is_none());
}

#[test]
fn detect_changed_files_measures_detection_time() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    let canonical = repo.path().canonicalize().unwrap();
    // Sanity bound only: three fast git subprocess calls on an empty repo
    // should never take anywhere near 30s even on a loaded CI box. This
    // guards against `detect_elapsed_ms` being left at 0/uninitialized
    // rather than asserting a tight timing window.
    let result = detect_changed_files(&canonical, &git, None).unwrap();
    assert!(
        result.detect_elapsed_ms < 30_000,
        "detect_elapsed_ms should be a real (small) measurement, got {}",
        result.detect_elapsed_ms
    );

    // A budget of 0ms exercises the early-return `partial()` path and
    // must also report a measured (non-panicking) elapsed time.
    let bounded = detect_changed_files(&canonical, &git, Some(0)).unwrap();
    assert!(bounded.budget_exceeded.is_some());
    assert!(bounded.detect_elapsed_ms < 30_000);
}

#[test]
fn detect_changed_files_finds_untracked_file() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    fs::write(repo.path().join("hello.rs"), "fn hello() {}\n").unwrap();
    let canonical = repo.path().canonicalize().unwrap();
    let result = detect_changed_files(&canonical, &git, None).unwrap();
    assert!(
        result.paths.contains(std::path::Path::new("hello.rs")),
        "untracked file should be detected, got: {:?}",
        result.paths
    );
}

#[test]
fn detect_changed_files_budget_exceeded_bails_early() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    // Create files and make an initial commit so git diff HEAD finds them.
    for i in 0..20 {
        fs::write(repo.path().join(format!("file_{i}.rs")), "// original\n").unwrap();
    }
    // Stage and commit (need a valid HEAD for git diff HEAD to produce
    // output on modified files).
    std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(["add", "-A"])
        .output()
        .unwrap();
    std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(["commit", "-m", "initial", "--no-gpg-sign"])
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();
    // Now modify files so git diff HEAD produces output.
    for i in 0..20 {
        fs::write(repo.path().join(format!("file_{i}.rs")), "// modified\n").unwrap();
    }
    let canonical = repo.path().canonicalize().unwrap();
    // budget=0 means "no time budget": no git command should run at all
    // (the deadline pre-check fires before the first spawn), so the result
    // is budget-exceeded with an empty path set. This is the fix for the
    // previous behavior where the first git command always ran unbounded.
    let result = detect_changed_files(&canonical, &git, Some(0)).unwrap();
    assert!(
        result.budget_exceeded.is_some(),
        "budget of 0ms should trigger BudgetExceeded"
    );
    assert!(
        result.paths.is_empty(),
        "budget=0 must perform no git work; got {:?}",
        result.paths
    );
}

#[test]
fn detect_changed_files_dedupes_path_reported_by_two_git_commands() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    // Commit a file, then `git rm --cached` it (untrack it from the index
    // while leaving the on-disk content untouched). That single logical
    // change is reported by BOTH `git diff HEAD` (path is deleted from
    // the index relative to HEAD) and `git ls-files --others` (the file
    // is now untracked). Without deduplication, ChangeSet.paths would
    // count this one change twice, which could falsely trip a
    // `max_files` cap set just above the true (deduped) delta size.
    fs::write(repo.path().join("dup.rs"), "orig\n").unwrap();
    std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(["add", "-A"])
        .output()
        .unwrap();
    std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(["commit", "-m", "initial", "--no-gpg-sign"])
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();
    let rm_status = std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(["rm", "--cached", "-q", "dup.rs"])
        .status()
        .unwrap();
    assert!(rm_status.success(), "git rm --cached must succeed");

    // Sanity-check the premise: both raw git commands report the path
    // (proves this scenario genuinely exercises the two-command overlap,
    // not just one command finding it).
    let diff_head = std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(["diff", "-z", "--name-only", "HEAD"])
        .output()
        .unwrap();
    let ls_others = std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(["ls-files", "-z", "--others", "--exclude-standard"])
        .output()
        .unwrap();
    assert!(
        !diff_head.stdout.is_empty(),
        "premise check: `git diff HEAD` must report dup.rs"
    );
    assert!(
        !ls_others.stdout.is_empty(),
        "premise check: `git ls-files --others` must report dup.rs"
    );

    let canonical = repo.path().canonicalize().unwrap();
    let result = detect_changed_files(&canonical, &git, None).unwrap();
    assert_eq!(
        result.paths.len(),
        1,
        "path reported by two git commands must collapse to one entry, got: {:?}",
        result.paths
    );
    assert!(result.paths.contains(std::path::Path::new("dup.rs")));

    // A max_files cap set just above the true (deduped) delta size of 1
    // must not be tripped by a double count.
    let limits = UpdateLimits {
        max_files: Some(1),
        budget_ms: None,
    };
    assert!(
        result.paths.len() <= limits.max_files.unwrap(),
        "deduped change set must fit under max_files=1, got {} paths",
        result.paths.len()
    );
}

#[test]
fn parse_nul_paths_splits_correctly() {
    let input = b"src/main.rs\0src/lib.rs\0tests/test.rs\0";
    let paths = parse_nul_paths(input);
    assert_eq!(paths.len(), 3);
    assert_eq!(paths[0], PathBuf::from("src/main.rs"));
    assert_eq!(paths[1], PathBuf::from("src/lib.rs"));
    assert_eq!(paths[2], PathBuf::from("tests/test.rs"));
}

#[test]
fn parse_nul_paths_filters_unsafe_paths() {
    let input = b"src/main.rs\0../../etc/passwd\0foo/bar.rs\0";
    let paths = parse_nul_paths(input);
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0], PathBuf::from("src/main.rs"));
    assert_eq!(paths[1], PathBuf::from("foo/bar.rs"));
}

#[test]
fn parse_nul_paths_handles_empty_input() {
    let paths = parse_nul_paths(b"");
    assert!(paths.is_empty());
}

#[test]
fn parse_nul_paths_handles_single_entry_no_trailing_nul() {
    let paths = parse_nul_paths(b"only_file.rs");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("only_file.rs"));
}

#[test]
fn change_set_budget_exceeded_is_none_on_full_detection() {
    let cs = ChangeSet {
        paths: HashSet::new(),
        budget_exceeded: None,
        detect_elapsed_ms: 0,
    };
    assert!(cs.budget_exceeded.is_none());
}

#[test]
fn update_outcome_budget_exceeded_has_nonzero_estimate() {
    let outcome = UpdateOutcome::BudgetExceeded {
        files_behind_estimate: 5,
        detect_elapsed_ms: 42,
    };
    match outcome {
        UpdateOutcome::BudgetExceeded {
            files_behind_estimate: n,
            ..
        } => assert!(n > 0),
        _ => panic!("expected BudgetExceeded"),
    }
}

#[test]
fn update_outcome_detect_elapsed_ms_reads_every_variant() {
    assert_eq!(
        UpdateOutcome::Updated {
            files: 1,
            skipped: 0,
            detect_elapsed_ms: 10,
        }
        .detect_elapsed_ms(),
        10
    );
    assert_eq!(
        UpdateOutcome::NoChanges {
            detect_elapsed_ms: 11
        }
        .detect_elapsed_ms(),
        11
    );
    assert_eq!(
        UpdateOutcome::BudgetExceeded {
            files_behind_estimate: 1,
            detect_elapsed_ms: 12,
        }
        .detect_elapsed_ms(),
        12
    );
    assert_eq!(
        UpdateOutcome::TooManyFiles {
            files_behind: 1,
            detect_elapsed_ms: 13,
        }
        .detect_elapsed_ms(),
        13
    );
}

#[test]
fn fsmonitor_tip_not_printed_below_half_budget() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    let canonical = repo.path().canonicalize().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();
    // 40ms elapsed against a 100ms budget is below half: must not stamp.
    maybe_print_fsmonitor_tip(&canonical, &git, index_dir.path(), 40, 100);
    assert!(
        !index_dir.path().join(FSMONITOR_TIP_STAMP).exists(),
        "stamp file must not be written below half the budget"
    );
}

#[test]
fn fsmonitor_tip_zero_budget_never_fires() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    let canonical = repo.path().canonicalize().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();
    maybe_print_fsmonitor_tip(&canonical, &git, index_dir.path(), 0, 0);
    assert!(!index_dir.path().join(FSMONITOR_TIP_STAMP).exists());
}

#[test]
fn fsmonitor_tip_prints_once_and_stamps_when_fsmonitor_unset() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    let canonical = repo.path().canonicalize().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();
    assert!(!is_fsmonitor_enabled(&canonical, &git));

    // 60ms of a 100ms budget is over half: first call must stamp.
    maybe_print_fsmonitor_tip(&canonical, &git, index_dir.path(), 60, 100);
    let stamp = index_dir.path().join(FSMONITOR_TIP_STAMP);
    assert!(stamp.exists(), "stamp file must be written on first fire");

    // A second call must be a no-op (stamp already present): remove the
    // stamp's content check isn't needed, just confirm no panic/re-fire
    // path exists by calling again and verifying the file still exists
    // untouched (best-effort: this mainly guards against a crash/second
    // eprintln, which is not independently observable here without
    // capturing stderr).
    maybe_print_fsmonitor_tip(&canonical, &git, index_dir.path(), 60, 100);
    assert!(stamp.exists());
}

#[test]
fn enable_fsmonitor_sets_config_and_is_then_detected() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    let canonical = repo.path().canonicalize().unwrap();
    assert!(!is_fsmonitor_enabled(&canonical, &git));

    assert!(
        enable_fsmonitor(&canonical, &git),
        "git config should succeed"
    );
    assert!(is_fsmonitor_enabled(&canonical, &git));

    // Also assert directly via `git config --get`, independent of our
    // own is_fsmonitor_enabled helper.
    let output = std::process::Command::new(&git)
        .arg("-C")
        .arg(&canonical)
        .args(["config", "--get", "core.fsmonitor"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "true");
}

#[test]
fn enable_fsmonitor_returns_false_outside_git_repo() {
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    let non_repo = tempfile::TempDir::new().unwrap();
    let canonical = non_repo.path().canonicalize().unwrap();
    assert!(!enable_fsmonitor(&canonical, &git));
}

#[test]
fn fsmonitor_tip_never_sets_fsmonitor_config() {
    // Bite: "never set it without the flag/consent." The tip path
    // (`maybe_print_fsmonitor_tip`) must only ever print a suggestion and
    // stamp a marker file; it must never itself flip `core.fsmonitor`,
    // no matter how many times it fires over budget. Only the explicit,
    // opt-in `enable_fsmonitor` (wired to `st init --fsmonitor`) may set
    // the config, since enabling fsmonitor starts a background daemon.
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    let canonical = repo.path().canonicalize().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();
    assert!(!is_fsmonitor_enabled(&canonical, &git));

    for _ in 0..5 {
        maybe_print_fsmonitor_tip(&canonical, &git, index_dir.path(), 90, 100);
        assert!(
            !is_fsmonitor_enabled(&canonical, &git),
            "the tip path must never set core.fsmonitor on its own"
        );
    }

    // Also assert directly via `git config --get` that the key was
    // never set at all (not merely "not true"), independent of our own
    // is_fsmonitor_enabled helper.
    let output = std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(["config", "--get", "core.fsmonitor"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "core.fsmonitor must remain unset after repeated tip calls"
    );
}

#[test]
fn fsmonitor_tip_never_fires_when_core_fsmonitor_already_true() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    let canonical = repo.path().canonicalize().unwrap();
    std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(["config", "core.fsmonitor", "true"])
        .output()
        .unwrap();
    assert!(is_fsmonitor_enabled(&canonical, &git));

    let index_dir = tempfile::TempDir::new().unwrap();
    maybe_print_fsmonitor_tip(&canonical, &git, index_dir.path(), 60, 100);
    assert!(
        !index_dir.path().join(FSMONITOR_TIP_STAMP).exists(),
        "stamp file must not be written when core.fsmonitor is already true"
    );
}

/// Bug 9 regression: a change set whose `git` output exceeds the ~64 KB OS
/// pipe buffer must not be lost. Before draining stdout concurrently in bounded
/// mode, git blocked writing the full pipe, was killed at the deadline, and its
/// output was discarded -- so the heavily-behind repos reported 0 changes and
/// never triggered catch-up. With a generous budget, draining lets the (fast
/// but verbose) `git ls-files` finish and every untracked file is detected.
#[test]
fn detect_changed_files_drains_output_larger_than_pipe_buffer() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    // ~2000 files with long names: the NUL-separated `ls-files --others` output
    // is well over the ~64 KB pipe buffer, so a non-draining reader would block.
    const N: usize = 2000;
    for i in 0..N {
        fs::write(
            repo.path()
                .join(format!("some_reasonably_long_source_file_name_{i:05}.rs")),
            "// x\n",
        )
        .unwrap();
    }
    let canonical = repo.path().canonicalize().unwrap();

    // Generous budget: draining means git finishes well within it, so all
    // changes are found and the set is NOT reported as budget-exceeded.
    let result = detect_changed_files(&canonical, &git, Some(30_000)).unwrap();
    assert_eq!(
        result.paths.len(),
        N,
        "all untracked files must be detected once stdout is drained; \
         budget_exceeded={:?}",
        result.budget_exceeded
    );
    assert!(result.budget_exceeded.is_none());
}
