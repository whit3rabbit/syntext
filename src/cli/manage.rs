//! Management subcommand handlers: index, status, update.

use std::collections::HashSet;
use std::io::{self, Write};

use crate::index::Index;
use crate::Config;

/// Resolves the absolute path to the `git` binary by walking PATH entries.
///
/// Using `Command::new("git")` would resolve via `$PATH`, allowing a
/// malicious `git` binary earlier on PATH to intercept the call.
/// This function walks PATH explicitly and falls back to `/usr/bin/git`
/// rather than a bare name.
fn resolve_git_binary() -> std::path::PathBuf {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("git");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    std::path::PathBuf::from("/usr/bin/git")
}

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
        let stdout = io::stdout();
        let mut out = stdout.lock();
        if let Err(err) = writeln!(out, "Documents: {}", s.total_documents)
            .and_then(|_| writeln!(out, "Segments:  {}", s.total_segments))
            .and_then(|_| writeln!(out, "Grams:     {}", s.total_grams))
        {
            return handle_output(err);
        }
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
        let stdout = io::stdout();
        let mut out = stdout.lock();
        if let Err(err) = writeln!(out, "{obj}") {
            return handle_output(err);
        }
    } else {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        if let Err(err) = writeln!(out, "Index:     {}", config.index_dir.display())
            .and_then(|_| writeln!(out, "Documents: {}", s.total_documents))
            .and_then(|_| writeln!(out, "Segments:  {}", s.total_segments))
            .and_then(|_| writeln!(out, "Grams:     {}", s.total_grams))
        {
            return handle_output(err);
        }
        if let Some(ref commit) = s.base_commit {
            if let Err(err) = writeln!(out, "Commit:    {commit}") {
                return handle_output(err);
            }
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

    // Security: canonicalize repo_root before passing it to `git -C`.
    //
    // `git -C <path>` changes into the given directory before running. If
    // <path> points to an attacker-controlled directory (e.g. --repo-root
    // sourced from an untrusted environment variable or container bind-mount),
    // git will execute hooks in that directory's .git/config (core.hooksPath,
    // post-checkout, etc.) with the invoking user's privileges. Canonicalize
    // resolves symlinks and produces an absolute path, eliminating relative-path
    // tricks and final-component symlink redirections.
    //
    // Note: this does not prevent a user who deliberately passes a malicious
    // path as --repo-root from triggering git hooks in that directory;
    // --repo-root is trusted input and must not be sourced from untrusted data
    // (e.g. artifact paths from untrusted CI jobs, user-supplied config).
    let canonical_root = match config.repo_root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("st update: invalid repo root \'{}\': {e}", config.repo_root.display());
            return 2;
        }
    };

    // Detect changed files via git diff against HEAD.
    // This fails on repos with no commits, which is fine -- we fall through
    // to untracked file detection below.
    if let Ok(diff_output) = std::process::Command::new(resolve_git_binary())
        .arg("-C")
        .arg(&canonical_root)
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
    if let Ok(staged_output) = std::process::Command::new(resolve_git_binary())
        .arg("-C")
        .arg(&canonical_root)
        .args(["diff", "--name-only", "--cached"])
        .output()
    {
        if staged_output.status.success() {
            let staged_stdout = String::from_utf8_lossy(&staged_output.stdout);
            changed.extend(staged_stdout.lines().filter(|l| !l.is_empty()).map(String::from));
        }
    }

    // Pick up new untracked files that git-diff doesn't report.
    if let Ok(ut_output) = std::process::Command::new(resolve_git_binary())
        .arg("-C")
        .arg(&canonical_root)
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
            let stdout = io::stdout();
            let mut out = stdout.lock();
            if let Err(err) = writeln!(out, "st: no changes detected") {
                return handle_output(err);
            }
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
        let stdout = io::stdout();
        let mut out = stdout.lock();
        if let Err(err) = writeln!(out, "st: updated {} file(s)", count) {
            return handle_output(err);
        }
    }
    if notify_errors > 0 { 1 } else { 0 }
}

fn handle_output(err: io::Error) -> i32 {
    if err.kind() == io::ErrorKind::BrokenPipe {
        0
    } else {
        eprintln!("st: {err}");
        2
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_git_binary;

    #[test]
    fn git_binary_resolves_to_absolute_path() {
        let path = resolve_git_binary();
        assert!(path.is_absolute(), "git binary must resolve to absolute path, got: {:?}", path);
    }
}
