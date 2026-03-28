//! Management subcommand handlers: index, status, update.

use std::collections::HashSet;

use crate::index::Index;
use crate::Config;

pub(super) fn cmd_index(mut config: Config, _force: bool, stats: bool, quiet: bool) -> i32 {
    // Index::build always rebuilds; --force is accepted for rg/ug compat.
    // --quiet suppresses library progress output; default CLI behavior is verbose.
    if quiet {
        config.verbose = false;
    } else if !config.verbose {
        // Neither --verbose nor --quiet: default to verbose for CLI users.
        config.verbose = true;
    }
    let index = match Index::build(config) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st index: {e}");
            return 2;
        }
    };

    if stats {
        let s = index.stats();
        println!("Documents: {}", s.total_documents);
        println!("Segments:  {}", s.total_segments);
        println!("Grams:     {}", s.total_grams);
    }
    0
}

pub(super) fn cmd_status(config: Config, json: bool) -> i32 {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st status: {e}");
            return 2;
        }
    };

    let s = index.stats();
    if json {
        // Use serde_json to avoid malformed output when index_dir contains
        // characters that need JSON escaping (quotes, backslashes, etc.).
        let obj = serde_json::json!({
            "documents": s.total_documents,
            "segments": s.total_segments,
            "grams": s.total_grams,
            "index_dir": config.index_dir.display().to_string(),
        });
        println!("{obj}");
    } else {
        println!("Index:     {}", config.index_dir.display());
        println!("Documents: {}", s.total_documents);
        println!("Segments:  {}", s.total_segments);
        println!("Grams:     {}", s.total_grams);
        if let Some(ref commit) = s.base_commit {
            println!("Commit:    {commit}");
        }
    }
    0
}

pub(super) fn cmd_update(config: Config, _flush: bool, quiet: bool) -> i32 {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st update: {e}");
            return 2;
        }
    };

    let mut changed: HashSet<String> = HashSet::new();

    // Detect changed files via git diff against HEAD.
    // This fails on repos with no commits, which is fine -- we fall through
    // to untracked file detection below.
    if let Ok(diff_output) = std::process::Command::new("git")
        .arg("-C")
        .arg(&config.repo_root)
        .args(["diff", "--name-only", "HEAD"])
        .output()
    {
        if diff_output.status.success() {
            let diff_stdout = String::from_utf8_lossy(&diff_output.stdout);
            changed.extend(
                diff_stdout
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(String::from),
            );
        }
    }

    // Pick up staged changes (covers initial commit scenario where HEAD
    // doesn't exist yet).
    if let Ok(staged_output) = std::process::Command::new("git")
        .arg("-C")
        .arg(&config.repo_root)
        .args(["diff", "--name-only", "--cached"])
        .output()
    {
        if staged_output.status.success() {
            let staged_stdout = String::from_utf8_lossy(&staged_output.stdout);
            changed.extend(staged_stdout.lines().filter(|l| !l.is_empty()).map(String::from));
        }
    }

    // Pick up new untracked files that git-diff doesn't report.
    if let Ok(ut_output) = std::process::Command::new("git")
        .arg("-C")
        .arg(&config.repo_root)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
    {
        if ut_output.status.success() {
            let ut_stdout = String::from_utf8_lossy(&ut_output.stdout);
            changed.extend(ut_stdout.lines().filter(|l| !l.is_empty()).map(String::from));
        }
    }

    if changed.is_empty() {
        if !quiet {
            println!("st: no changes detected");
        }
        return 0;
    }

    let mut count = 0;
    let mut notify_errors = 0usize;
    for path in &changed {
        let abs = config.repo_root.join(path);
        if abs.exists() {
            if let Err(e) = index.notify_change(&abs) {
                eprintln!("st update: {path}: {e}");
                notify_errors += 1;
            } else {
                count += 1;
            }
        } else {
            if let Err(e) = index.notify_delete(&abs) {
                eprintln!("st update: {path}: {e}");
                notify_errors += 1;
            } else {
                count += 1;
            }
        }
    }

    if let Err(e) = index.commit_batch() {
        eprintln!("st update: commit failed: {e}");
        return 2;
    }

    if !quiet {
        println!("st: updated {} file(s)", count);
    }
    if notify_errors > 0 { 1 } else { 0 }
}
