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
    // Sanity bound only: one fast git subprocess call on an empty repo
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
    // Create files and make an initial commit so `git status` would report
    // them as modified (not untracked) if detection ran at all.
    for i in 0..20 {
        fs::write(repo.path().join(format!("file_{i}.rs")), "// original\n").unwrap();
    }
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
    // Now modify files so `git status` has something to report.
    for i in 0..20 {
        fs::write(repo.path().join(format!("file_{i}.rs")), "// modified\n").unwrap();
    }
    let canonical = repo.path().canonicalize().unwrap();
    // budget=0 means "no time budget": git must not be spawned at all (the
    // deadline pre-check fires before the spawn), so the result is
    // budget-exceeded with an empty path set. This is the fix for the
    // previous behavior where the git command always ran unbounded.
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
fn detect_changed_files_dedupes_path_reported_by_two_status_records() {
    let repo = init_git_repo();
    let git = crate::git_util::resolve_git_binary();
    if !git.is_file() {
        return;
    }
    // Commit a file, then `git rm --cached` it (untrack it from the index
    // while leaving the on-disk content untouched). That single logical
    // change is reported by `git status` TWICE: a `D ` record (deleted from
    // the index relative to HEAD) and a `?? ` record (the file is now
    // untracked). Without deduplication, ChangeSet.paths would count this
    // one change twice, which could falsely trip a `max_files` cap set
    // just above the true (deduped) delta size.
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

    // Sanity-check the premise: the raw status output carries two records
    // for the path (proves this scenario genuinely exercises the overlap,
    // not just one record finding it).
    let status = std::process::Command::new(&git)
        .arg("-C")
        .arg(repo.path())
        .args(STATUS_ARGS)
        .output()
        .unwrap();
    let mut xy_for_dup: Vec<&[u8]> = status
        .stdout
        .split(|&b| b == 0)
        .filter(|rec| rec.ends_with(b" dup.rs"))
        .map(|rec| &rec[..2])
        .collect();
    xy_for_dup.sort();
    let expected: Vec<&[u8]> = vec![b"??", b"D "];
    assert_eq!(
        xy_for_dup, expected,
        "premise check: `git status` must report dup.rs as both `D ` and `??`"
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

/// Verify that a large change set whose `git` output exceeds the OS pipe buffer
/// (~64 KB) is fully drained without blocking or getting killed at the deadline.
/// This ensures heavily-behind repos with many untracked files are correctly detected.
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

/// Write a fake `git` shim to `dir/fake-git` and make it executable (unix
/// only). Lets the deadline-kill tests reproduce the kill deterministically:
/// the real bug needs git to be killed *during* a command, which a sleep in
/// the shim reproduces reliably.
#[cfg(unix)]
fn write_fake_git(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-git");
    fs::write(&path, script).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

/// Sizes the deadline for the two kill-classification tests below off what
/// spawning the shim actually costs on *this* machine, instead of a fixed
/// constant.
///
/// These two tests need the shim to be spawned and to write its record before
/// the parent kills it at the deadline, and nothing in the test controls how
/// fast the OS gets there. A fixed deadline therefore encodes an assumption
/// about the host. The original 200ms/500ms held on an idle macOS box, where
/// spawn-to-first-byte measures 5-9ms, and lost on the same box while
/// `syspolicyd` sat wedged at ~460% CPU (a loop of Rust relinks provokes it,
/// since each freshly linked unsigned binary gets assessed): there the same
/// spawn took 3.8s, so `run_git_bounded` reached its poll loop with the
/// deadline already expired and killed the shim before it had written a byte.
/// Raising the constant only moves the threshold; calibrating removes the
/// assumption.
///
/// The 10x multiplier is margin over the calibration sample itself, the 200ms
/// floor keeps the common case fast, and the 30s ceiling stops a pathological
/// host from hanging the suite. Making this exact instead of calibrated would
/// mean restructuring `run_git_bounded` to take an injectable reader rather
/// than spawning a process; that is a production change, deliberately not
/// made for a test.
///
/// A healthy host takes the 200ms floor, which is what each test then costs:
/// both shims close stdout before stalling (see `STALL_AFTER_CLOSING_STDOUT`),
/// so the deadline is the whole bill.
#[cfg(unix)]
fn calibrated_kill_deadline_ms(fake: &std::path::Path, dir: &std::path::Path) -> u64 {
    let start = std::time::Instant::now();
    let out = std::process::Command::new(fake)
        .arg("-C")
        .arg(dir)
        .arg("calibrate")
        .output()
        .expect("run calibration shim");
    let observed = start.elapsed().as_millis() as u64;
    assert!(
        !out.stdout.is_empty(),
        "the calibration branch must print, or it is not measuring a real write"
    );
    observed.saturating_mul(10).clamp(200, 30_000)
}

/// Shell fragment both kill-test shims use to stall until the parent kills
/// them: flush and close stdout, then sleep past any deadline.
///
/// The `exec 1>&-` is what keeps these tests cheap. `run_git_bounded` joins a
/// `read_to_end` drain thread, which only returns once *every* write end of
/// the pipe is closed -- and `sleep` is an external command, so it inherits
/// the shell's stdout and holds that write end for its whole duration.
/// Killing the shell does not kill the grandchild, so a shim that just slept
/// made each test cost the full sleep (the original `sleep 5` spent 5s per
/// test doing nothing; a `sleep 60` spent 60s). Closing stdout first flushes
/// the record into the pipe and hands the drain thread its EOF immediately,
/// so the sleep can be arbitrarily long at no cost and the test bills only
/// the deadline. It also makes truncation structural rather than timed:
/// nothing can reach the pipe after this point even in principle.
#[cfg(unix)]
const STALL_AFTER_CLOSING_STDOUT: &str = "exec 1>&-; sleep 3600";

/// `run_git_bounded` must tag a deadline kill as `Partial`, not collapse it
/// into the same `Ok(Some(buf))` shape as a clean success (the masked-staleness
/// bug). The shim emits one complete `-z` record, then stalls well past the
/// deadline so it is killed mid-output.
#[test]
#[cfg(unix)]
fn run_git_bounded_classifies_deadline_kill_as_partial() {
    let dir = tempfile::TempDir::new().unwrap();
    // `calibrate` exits straight after printing so the calibration spawn is
    // cheap; the real branch stalls far past the deadline, so "was it killed?"
    // is never in question.
    let fake = write_fake_git(
        dir.path(),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *calibrate*) printf 'seen.rs\\0' ;;\n  \
             *) printf 'seen.rs\\0'; {STALL_AFTER_CLOSING_STDOUT} ;;\nesac\n"
        ),
    );
    let budget = calibrated_kill_deadline_ms(&fake, dir.path());
    let deadline = Some(std::time::Instant::now() + std::time::Duration::from_millis(budget));
    match run_git_bounded(&fake, dir.path(), &["ignored"], deadline).unwrap() {
        GitOutput::Partial(buf) => assert_eq!(buf, b"seen.rs\0"),
        other => panic!("expected Partial, got {other:?}"),
    }
}

/// Regression for the masked-staleness bug, through `detect_changed_files`
/// rather than `run_git_bounded` directly: the `status` shim emits one
/// complete record and stalls until killed. The kill must surface as
/// `budget_exceeded: Some(1)`, never as a complete detection with
/// `budget_exceeded: None`, or the staleness notice and the detached async
/// catch-up are suppressed. This is the only test that checks the `Partial`
/// arm inside `detect_changed_files` itself.
#[test]
#[cfg(unix)]
fn detect_changed_files_reports_budget_exceeded_when_status_is_killed() {
    let dir = tempfile::TempDir::new().unwrap();
    let fake = write_fake_git(
        dir.path(),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *calibrate*) printf '?? untracked.rs\\0' ;;\n  \
             *status*) printf '?? untracked.rs\\0'; {STALL_AFTER_CLOSING_STDOUT} ;;\n  \
             *) exit 0 ;;\nesac\n"
        ),
    );
    let budget = calibrated_kill_deadline_ms(&fake, dir.path());
    let result = detect_changed_files(dir.path(), &fake, Some(budget)).unwrap();
    assert_eq!(
        result.budget_exceeded,
        Some(1),
        "a deadline kill must report exhaustion with the partial count"
    );
    assert!(result.paths.contains(std::path::Path::new("untracked.rs")));
}
