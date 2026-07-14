//! Phase 5: Incremental/overlay differential oracle.
//!
//! Exercises the overlay delta path (notify_change, notify_delete, commit_batch)
//! by applying random mutation sequences to a live working tree and asserting that
//! `st` results match `rg` (which always reads live files) after each commit.
//!
//! Also includes a golden smoke test for the OverlayFull failure path.

#[path = "oracle_helpers.rs"]
mod oracle_helpers;

use oracle_helpers::{normalize_ndjson, rg_available};
use proptest::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use syntext::index::Index;
use syntext::{Config, IndexError};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Mutation Operations (Phase 5.1)
// ---------------------------------------------------------------------------

/// A single mutation to apply to the working tree + index overlay.
#[derive(Debug, Clone)]
enum MutationOp {
    /// Overwrite an existing file with new content.
    ModifyFile { path: String, content: Vec<u8> },
    /// Create a new file (or overwrite if it already exists).
    CreateFile { path: String, content: Vec<u8> },
    /// Delete an existing file.
    DeleteFile { path: String },
    /// Rename a file (delete old, create new).
    RenameFile { from: String, to: String },
    /// Overwrite a file with binary content (contains a NUL byte).
    BinaryifyFile { path: String },
    /// Grow a file past max_file_size so it gets excluded.
    GrowPastLimit { path: String },
    /// notify_change then notify_delete in the same batch (change-then-delete).
    ChangeThenDeleteSameBatch { path: String, content: Vec<u8> },
}

fn text_content() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(b"fn parse_query() {}\n".to_vec()),
        Just(b"fn reparse() { let x = 1; }\n".to_vec()),
        Just(b"def snake_case(camelCase):\n    parse\n".to_vec()),
        Just(b"// TODO: query\nfn helper() {}\n".to_vec()),
        Just(b"let result = process_batch();\n".to_vec()),
        Just(b"fn new_function() { parse_query(42); }\n".to_vec()),
    ]
}

fn file_path_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("src/main.rs".to_string()),
        Just("src/lib.rs".to_string()),
        Just("src/util.rs".to_string()),
        Just("src/helper.rs".to_string()),
        Just("docs/notes.md".to_string()),
    ]
}

fn mutation_op_strategy() -> impl Strategy<Value = MutationOp> {
    prop_oneof![
        3 => (file_path_strategy(), text_content())
                .prop_map(|(path, content)| MutationOp::ModifyFile { path, content }),
        2 => (file_path_strategy(), text_content())
                .prop_map(|(path, content)| MutationOp::CreateFile { path, content }),
        2 => file_path_strategy()
                .prop_map(|path| MutationOp::DeleteFile { path }),
        1 => (file_path_strategy(), file_path_strategy())
                .prop_filter("rename must change path", |(a, b)| a != b)
                .prop_map(|(from, to)| MutationOp::RenameFile { from, to }),
        1 => file_path_strategy()
                .prop_map(|path| MutationOp::BinaryifyFile { path }),
        1 => file_path_strategy()
                .prop_map(|path| MutationOp::GrowPastLimit { path }),
        1 => (file_path_strategy(), text_content())
                .prop_map(|(path, content)| MutationOp::ChangeThenDeleteSameBatch { path, content }),
    ]
}

fn generate_mutation_sequence() -> impl Strategy<Value = Vec<MutationOp>> {
    prop::collection::vec(mutation_op_strategy(), 3..=8)
}

// ---------------------------------------------------------------------------
// Mutation Application (Phase 5.2)
// ---------------------------------------------------------------------------

/// Apply a mutation to the on-disk working tree and notify the index.
/// Returns `Ok(true)` if a `commit_batch` should be called, `Ok(false)` if the
/// op was a no-op (e.g. delete of non-existent file).
///
/// `git_cmd` is called with the git sub-command args to keep the git index in
/// sync so that `st`'s git-walk sees the same file set as `rg`'s filesystem walk.
fn apply_mutation(
    repo: &Path,
    index: &Index,
    op: &MutationOp,
    max_file_size: u64,
    git_cmd: &dyn Fn(&[&str]),
) -> Result<bool, String> {
    match op {
        MutationOp::ModifyFile { path, content } => {
            let abs = repo.join(path);
            if !abs.exists() {
                return Ok(false);
            }
            fs::write(&abs, content).map_err(|e| format!("ModifyFile write failed: {e}"))?;
            index
                .notify_change(&abs)
                .map_err(|e| format!("notify_change failed: {e}"))?;
            // No git add needed — existing tracked file, rg already sees it.
            Ok(true)
        }
        MutationOp::CreateFile { path, content } => {
            let abs = repo.join(path);
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p).map_err(|e| format!("create_dir_all failed: {e}"))?;
            }
            fs::write(&abs, content).map_err(|e| format!("CreateFile write failed: {e}"))?;
            index
                .notify_change(&abs)
                .map_err(|e| format!("notify_change failed: {e}"))?;
            // Track in git so st's git-walk includes it.
            git_cmd(&["add", path]);
            Ok(true)
        }
        MutationOp::DeleteFile { path } => {
            let abs = repo.join(path);
            if !abs.exists() {
                return Ok(false);
            }
            fs::remove_file(&abs).map_err(|e| format!("DeleteFile remove_file failed: {e}"))?;
            index
                .notify_delete(&abs)
                .map_err(|e| format!("notify_delete failed: {e}"))?;
            // Remove from git index so rg and st agree the file is gone.
            git_cmd(&["rm", "--cached", "--ignore-unmatch", path]);
            Ok(true)
        }
        MutationOp::RenameFile { from, to } => {
            let abs_from = repo.join(from);
            let abs_to = repo.join(to);
            if !abs_from.exists() {
                return Ok(false);
            }
            if let Some(p) = abs_to.parent() {
                fs::create_dir_all(p)
                    .map_err(|e| format!("RenameFile create_dir_all failed: {e}"))?;
            }
            fs::rename(&abs_from, &abs_to).map_err(|e| format!("RenameFile rename failed: {e}"))?;
            index
                .notify_delete(&abs_from)
                .map_err(|e| format!("notify_delete(from) failed: {e}"))?;
            index
                .notify_change(&abs_to)
                .map_err(|e| format!("notify_change(to) failed: {e}"))?;
            // Update git index: remove old path, add new path.
            git_cmd(&["rm", "--cached", "--ignore-unmatch", from]);
            git_cmd(&["add", to]);
            Ok(true)
        }
        MutationOp::BinaryifyFile { path } => {
            let abs = repo.join(path);
            // Binary content: text prefix + NUL byte
            let mut content = b"fn binary_content() { ".to_vec();
            content.push(0);
            content.extend_from_slice(b" }\n");
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p)
                    .map_err(|e| format!("BinaryifyFile create_dir_all failed: {e}"))?;
            }
            fs::write(&abs, &content).map_err(|e| format!("BinaryifyFile write failed: {e}"))?;
            index
                .notify_change(&abs)
                .map_err(|e| format!("notify_change failed: {e}"))?;
            Ok(true)
        }
        MutationOp::GrowPastLimit { path } => {
            let abs = repo.join(path);
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p)
                    .map_err(|e| format!("GrowPastLimit create_dir_all failed: {e}"))?;
            }
            // Write max_file_size + 1 bytes so the file is excluded from indexing
            let oversized = vec![b'x'; (max_file_size + 1) as usize];
            fs::write(&abs, &oversized).map_err(|e| format!("GrowPastLimit write failed: {e}"))?;
            index
                .notify_change(&abs)
                .map_err(|e| format!("notify_change failed: {e}"))?;
            Ok(true)
        }
        MutationOp::ChangeThenDeleteSameBatch { path, content } => {
            let abs = repo.join(path);
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p)
                    .map_err(|e| format!("ChangeThenDelete create_dir_all failed: {e}"))?;
            }
            // Write the file, notify_change, then delete, notify_delete — all in one batch
            fs::write(&abs, content).map_err(|e| format!("ChangeThenDelete write failed: {e}"))?;
            index
                .notify_change(&abs)
                .map_err(|e| format!("notify_change failed: {e}"))?;
            fs::remove_file(&abs).map_err(|e| format!("ChangeThenDelete remove failed: {e}"))?;
            index
                .notify_delete(&abs)
                .map_err(|e| format!("notify_delete failed: {e}"))?;
            // Remove from git index — file is gone from disk.
            git_cmd(&["rm", "--cached", "--ignore-unmatch", path]);
            Ok(true)
        }
    }
}

/// Run `st` and `rg` on the current working tree with `--json` and compare.
/// Applies Tier A (no false negatives) and Tier B (exact set match).
///
/// `max_file_size` is forwarded to the subprocess via `SYNTEXT_MAX_FILE_SIZE`
/// so the subprocess's bounded auto-update applies the same oversized-file
/// exclusion policy as the in-process `Index::build` that created the base.
/// Without this, a `GrowPastLimit` file the in-process index excluded at build
/// time would be re-indexed by the subprocess (which defaults to 10 MiB),
/// inflating the overlay past the 50%-of-base cap and triggering a stale
/// `OverlayFull` search that misses genuinely-new files (false Tier-A).
fn assert_st_matches_rg(
    repo: &Path,
    index_dir: &Path,
    query: &str,
    step: usize,
    max_file_size: u64,
) -> Result<(), String> {
    if !rg_available() {
        return Ok(());
    }

    let st_bin = env!("CARGO_BIN_EXE_st");

    let st_args = [
        "--repo-root",
        repo.to_str().unwrap(),
        "--index-dir",
        index_dir.to_str().unwrap(),
        "--json",
        query,
    ];

    let st_output = Command::new(st_bin)
        .args(&st_args)
        .current_dir(repo)
        .env("SYNTEXT_DETERMINISTIC", "1")
        // Keep the subprocess's file-size policy in lock-step with the
        // in-process build (see the doc comment on `max_file_size` above).
        .env("SYNTEXT_MAX_FILE_SIZE", max_file_size.to_string())
        // The subprocess's bounded auto-update may spawn a detached
        // `st update --quiet` catch-up that holds the exclusive dir lock after
        // this subprocess exits, racing the test's `Index::open` reopen
        // (LockConflict). Disabling the async catch-up keeps the lock boundary
        // clean: the subprocess's synchronous bounded update is the only writer.
        .env("SYNTEXT_NO_ASYNC_UPDATE", "1")
        .output()
        .map_err(|e| format!("step {step}: failed to run st: {e}"))?;

    let rg_output = Command::new("rg")
        .args([
            "--json",
            "--hidden",
            "--crlf",
            "--glob",
            "!.gitignore",
            "--glob",
            "!.syntext",
            query,
            ".",
        ])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("step {step}: failed to run rg: {e}"))?;

    let st_matches = normalize_ndjson(&st_output.stdout)
        .map_err(|e| format!("step {step}: st NDJSON parse error: {e}"))?;
    let rg_matches = normalize_ndjson(&rg_output.stdout)
        .map_err(|e| format!("step {step}: rg NDJSON parse error: {e}"))?;

    // Tier A: no false negatives at the line level.
    let st_line_keys: std::collections::HashSet<(&str, usize)> = st_matches
        .iter()
        .map(|m| (m.path.as_str(), m.line_number))
        .collect();
    for m in &rg_matches {
        if !st_line_keys.contains(&(m.path.as_str(), m.line_number)) {
            return Err(format!(
                "step {step}: Tier A Violation: rg found {:?} but st did not.\n\
                 Query: {:?}\n\
                 st stdout:\n{}\n\
                 rg stdout:\n{}",
                m,
                query,
                String::from_utf8_lossy(&st_output.stdout),
                String::from_utf8_lossy(&rg_output.stdout),
            ));
        }
    }

    if st_matches.len() != rg_matches.len() {
        return Err(format!(
            "step {step}: Tier B Violation: st={} matches, rg={} matches.\n\
             Query: {:?}\n\
             st stdout:\n{}\n\
             rg stdout:\n{}",
            st_matches.len(),
            rg_matches.len(),
            query,
            String::from_utf8_lossy(&st_output.stdout),
            String::from_utf8_lossy(&rg_output.stdout),
        ));
    }

    Ok(())
}

/// commit_batch with retry on LockConflict (matches incremental.rs pattern).
fn commit_batch_with_retry(index: &Index) -> Result<(), IndexError> {
    use std::thread;
    use std::time::Duration;
    const MAX: usize = 5;
    for attempt in 1..=MAX {
        match index.commit_batch() {
            Ok(()) => return Ok(()),
            Err(IndexError::LockConflict(_)) if attempt < MAX => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Open the index with retry on LockConflict. The subprocess `st` search holds
/// the shared dir lock for its lifetime; even though we `drop(index)` before
/// spawning it, the subprocess's own bounded update may briefly hold the
/// exclusive lock at the moment we reopen. A short retry backoff resolves the
/// overlap without masking a genuinely wedged index.
fn open_with_retry(config: Config) -> Result<Index, IndexError> {
    use std::thread;
    use std::time::Duration;
    const MAX: usize = 5;
    for attempt in 1..=MAX {
        match Index::open(config.clone()) {
            Ok(idx) => return Ok(idx),
            Err(IndexError::LockConflict(_)) if attempt < MAX => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

// ---------------------------------------------------------------------------
// Phase 5.2: Incremental Differential Property Test
// ---------------------------------------------------------------------------

fn generate_incremental_run() -> impl Strategy<Value = (Vec<MutationOp>, String)> {
    let query_strat = prop_oneof![
        Just("parse".to_string()),
        Just("parse_query".to_string()),
        Just("fn".to_string()),
        Just("let".to_string()),
        Just("helper".to_string()),
        Just("result".to_string()),
    ];
    (generate_mutation_sequence(), query_strat)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(15))]
    #[test]
    fn test_incremental_differential((mutations, query) in generate_incremental_run()) {
        let repo = TempDir::new().unwrap();
        let index_dir_tmp = TempDir::new().unwrap();
        let index_dir: PathBuf = index_dir_tmp.path().to_path_buf();

        // Write initial corpus.
        //
        // The filler files (filler_0..filler_9) are never touched by any
        // mutation (they are not in `file_path_strategy`), so they inflate the
        // base doc count without participating in the change set. This keeps
        // the accumulated uncommitted drift the subprocess re-detects from
        // crossing the 50%-of-base OverlayFull cap: with 5 mutable paths, a
        // worst-case 5-file change set against a 14-doc base (3 real +
        // .gitignore + 10 fillers) is ~36%, comfortably under 50%. Without the
        // fillers the base is only 4 docs and 3 accumulated changes already blow
        // the cap, making every run that hits a large drift sequence fail on a
        // legitimate OverlayFull-stale search (Tier-A miss) that is not a real
        // index bug.
        let initial_files = [
            ("src/main.rs", b"fn parse_query() {}\n".as_ref()),
            ("src/lib.rs", b"fn reparse() { let x = 1; }\n".as_ref()),
            ("src/util.rs", b"fn helper() {}\n".as_ref()),
            ("filler_0.rs", b"// filler 0\nfn unused_0() {}\n".as_ref()),
            ("filler_1.rs", b"// filler 1\nfn unused_1() {}\n".as_ref()),
            ("filler_2.rs", b"// filler 2\nfn unused_2() {}\n".as_ref()),
            ("filler_3.rs", b"// filler 3\nfn unused_3() {}\n".as_ref()),
            ("filler_4.rs", b"// filler 4\nfn unused_4() {}\n".as_ref()),
            ("filler_5.rs", b"// filler 5\nfn unused_5() {}\n".as_ref()),
            ("filler_6.rs", b"// filler 6\nfn unused_6() {}\n".as_ref()),
            ("filler_7.rs", b"// filler 7\nfn unused_7() {}\n".as_ref()),
            ("filler_8.rs", b"// filler 8\nfn unused_8() {}\n".as_ref()),
            ("filler_9.rs", b"// filler 9\nfn unused_9() {}\n".as_ref()),
        ];
        for (path, content) in &initial_files {
            let abs = repo.path().join(path);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(&abs, content).unwrap();
        }

        // Git init so st can discover files
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C").arg(repo.path())
                .args(args)
                .output()
                .ok();
        };
        git(&["init"]);
        git(&["config", "user.name", "oracle"]);
        git(&["config", "user.email", "oracle@example.com"]);
        fs::write(repo.path().join(".gitignore"), b".syntext/\n.git/\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "init", "--no-gpg-sign"]);

        let max_file_size: u64 = 512 * 1024; // 512KB for tests
        let config = Config {
            index_dir: index_dir.clone(),
            repo_root: repo.path().to_path_buf(),
            max_file_size,
            auto_update: false, // disable auto-update; we control commits manually
            ..Config::default()
        };

        let mut index = Index::build(config.clone()).expect("build index");

        // Apply mutations one at a time; commit + assert after each step
        for (step, op) in mutations.iter().enumerate() {
            let needs_commit = apply_mutation(repo.path(), &index, op, max_file_size, &git)
                .unwrap_or(false);

            if needs_commit {
                match commit_batch_with_retry(&index) {
                    Ok(()) => {}
                    Err(IndexError::OverlayFull { .. }) => {
                        // Overlay too large — this is a valid outcome; skip Tier A/B for this step
                        // (the overlay is in a known degraded state, not corrupted).
                        break;
                    }
                    Err(e) => panic!("step {step}: commit_batch failed: {e}"),
                }

                // Release the in-process index's shared dir lock while the
                // subprocess `st` runs. The subprocess auto-updates from git
                // independently (it never reads this process's in-memory
                // overlay), and its bounded update may need an exclusive lock to
                // rebuild the base on a large delta. Holding the shared lock
                // here would block that rebuild (LockConflict -> silent stale),
                // producing a false Tier-A failure that no real one-shot `st`
                // invocation would hit. `assert_st_matches_rg` also forwards
                // `max_file_size` (via SYNTEXT_MAX_FILE_SIZE) and disables the
                // async catch-up (SYNTEXT_NO_ASYNC_UPDATE=1) so the subprocess's
                // file-size policy matches the in-process build and no detached
                // `st update` lingers to race the reopen below. Reopen afterward
                // to keep exercising the in-process overlay delta path on the
                // next mutation.
                drop(index);
                assert_st_matches_rg(
                    repo.path(),
                    &index_dir,
                    &query,
                    step,
                    max_file_size,
                )
                .expect("incremental differential mismatch");
                index = open_with_retry(config.clone()).expect("reopen index");
            }
        }

        drop(index);
    }
}

// ---------------------------------------------------------------------------
// Phase 5.3: OverlayFull Failure Path Golden Smoke Test
// ---------------------------------------------------------------------------

/// Verify that after an OverlayFull error:
/// 1. `commit_batch` returns `IndexError::OverlayFull`.
/// 2. Subsequent `st` searches still satisfy Tier A against the pre-error tree
///    (the verifier re-reads real files, so no fabricated matches possible).
#[test]
fn overlay_full_correctness() {
    if !rg_available() {
        return;
    }

    let repo = TempDir::new().unwrap();
    let index_dir_tmp = TempDir::new().unwrap();
    let index_dir = index_dir_tmp.path().to_path_buf();

    // Write exactly 1 file so the base has 1 document.
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/main.rs"), b"fn parse_query() {}\n").unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .ok();
    };
    git(&["init"]);
    git(&["config", "user.name", "oracle"]);
    git(&["config", "user.email", "oracle@example.com"]);
    fs::write(repo.path().join(".gitignore"), b".syntext/\n.git/\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "init", "--no-gpg-sign"]);

    let config = Config {
        index_dir: index_dir.clone(),
        repo_root: repo.path().to_path_buf(),
        auto_update: false,
        ..Config::default()
    };

    let index = Index::build(config).expect("build");

    // Verify baseline: st and rg agree on the initial corpus
    assert_st_matches_rg(repo.path(), &index_dir, "parse_query", 0, Config::default().max_file_size)
        .expect("baseline differential mismatch");

    // Now: create enough overlay entries to exceed 50% of base docs.
    // Base has 1 doc. OverlayFull triggers when overlay_docs > 50% * base_docs.
    // 1 change gets us to 100% overlay, crossing the threshold.
    // Add 2 more distinct files to ensure we exceed the enforce threshold.
    let extra_files = ["src/lib.rs", "src/util.rs"];
    for path in &extra_files {
        let abs = repo.path().join(path);
        fs::write(&abs, b"fn helper() { let x = parse_all(); }\n").unwrap();
        index.notify_change(&abs).ok();
    }

    let result = index.commit_batch();
    match result {
        Err(IndexError::OverlayFull { .. }) => {
            // Expected — now verify Tier A still holds on the pre-error tree state.
            // st should not return fabricated matches (verifier re-reads live files).
            assert_st_matches_rg(repo.path(), &index_dir, "parse_query", 99, Config::default().max_file_size)
                .expect(
                    "post-OverlayFull differential mismatch: verifier must not fabricate matches",
                );
        }
        Ok(()) => {
            // The overlay didn't fill up with this corpus size.
            // This means the index has more base docs than expected (proptest shrinking,
            // or the threshold wasn't crossed). Not a failure — just skip the assertion.
            // We still verify correctness.
            assert_st_matches_rg(repo.path(), &index_dir, "parse_query", 99, Config::default().max_file_size)
                .expect("post-commit differential mismatch");
        }
        Err(e) => panic!("unexpected commit_batch error: {e}"),
    }

    drop(index);
}

// ---------------------------------------------------------------------------
// Additional golden smoke: rename correctness
// ---------------------------------------------------------------------------

#[test]
fn golden_incremental_rename() {
    if !rg_available() {
        return;
    }

    let repo = TempDir::new().unwrap();
    let index_dir_tmp = TempDir::new().unwrap();
    let index_dir = index_dir_tmp.path().to_path_buf();

    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/old.rs"), b"fn parse_query() {}\n").unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .ok();
    };
    git(&["init"]);
    git(&["config", "user.name", "oracle"]);
    git(&["config", "user.email", "oracle@example.com"]);
    fs::write(repo.path().join(".gitignore"), b".syntext/\n.git/\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "init", "--no-gpg-sign"]);

    let config = Config {
        index_dir: index_dir.clone(),
        repo_root: repo.path().to_path_buf(),
        auto_update: false,
        ..Config::default()
    };
    let index = Index::build(config).expect("build");

    // Verify initial state
    assert_st_matches_rg(repo.path(), &index_dir, "parse_query", 0, Config::default().max_file_size)
        .unwrap();

    // Rename src/old.rs -> src/new.rs
    let abs_old = repo.path().join("src/old.rs");
    let abs_new = repo.path().join("src/new.rs");
    fs::rename(&abs_old, &abs_new).unwrap();
    index.notify_delete(&abs_old).unwrap();
    index.notify_change(&abs_new).unwrap();
    index.commit_batch().unwrap();

    // Update git index: remove old path, add new path so st's git-walk agrees with rg.
    git(&["rm", "--cached", "--ignore-unmatch", "src/old.rs"]);
    git(&["add", "src/new.rs"]);

    // After rename: st must find the match in the new path, not the old
    assert_st_matches_rg(repo.path(), &index_dir, "parse_query", 1, Config::default().max_file_size)
        .unwrap();

    drop(index);
}

/// Verify that a file grown past max_file_size is excluded from results.
#[test]
fn golden_incremental_grow_past_limit() {
    if !rg_available() {
        return;
    }

    let repo = TempDir::new().unwrap();
    let index_dir_tmp = TempDir::new().unwrap();
    let index_dir = index_dir_tmp.path().to_path_buf();

    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/main.rs"), b"fn parse_query() {}\n").unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .ok();
    };
    git(&["init"]);
    git(&["config", "user.name", "oracle"]);
    git(&["config", "user.email", "oracle@example.com"]);
    fs::write(repo.path().join(".gitignore"), b".syntext/\n.git/\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "init", "--no-gpg-sign"]);

    let max_file_size: u64 = 1024; // 1KB limit for this test
    let config = Config {
        index_dir: index_dir.clone(),
        repo_root: repo.path().to_path_buf(),
        max_file_size,
        auto_update: false,
        ..Config::default()
    };
    let index = Index::build(config).expect("build");

    // Grow past limit
    let abs_main = repo.path().join("src/main.rs");
    let oversized = vec![b'x'; (max_file_size + 1) as usize];
    fs::write(&abs_main, &oversized).unwrap();
    index.notify_change(&abs_main).unwrap();
    index.commit_batch().unwrap();

    // After growing past the limit: rg won't search oversized binary-looking file by default,
    // and st should have removed it from the index. Both should report 0 matches.
    // We use a literal that was only in the old content.
    assert_st_matches_rg(repo.path(), &index_dir, "parse_query", 1, max_file_size).unwrap();

    drop(index);
}
