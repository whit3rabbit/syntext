//! Management subcommand handlers: index, status, update.

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::index::Index;
use crate::path_util::path_from_bytes;
use crate::Config;

use crate::git_util::{is_safe_git_path, resolve_git_binary};

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
    drop(index);
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
    drop(index);
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

    // Security audit (command injection): no user-controlled data is interpolated
    // as shell arguments. `resolve_git_binary()` resolves the git path via PATH
    // with canonicalize (see its doc comment). `canonical_root` below is
    // canonicalized before passing to `git -C`. All other arguments are static
    // string literals. The only injection surface would be `--repo-root`, which
    // is documented as trusted input.
    let git = resolve_git_binary();

    // The fallback path (/usr/bin/git on Unix) may not exist; verify before spawning.
    if !git.is_file() {
        eprintln!(
            "st update: git not found (looked for {}); install git to detect changed files",
            git.display()
        );
        drop(index);
        return 2;
    }

    let mut changed: HashSet<PathBuf> = HashSet::new();

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
            eprintln!(
                "st update: invalid repo root \'{}\': {e}",
                config.repo_root.display()
            );
            return 2;
        }
    };

    // Parse NUL-terminated git output into changed paths.
    //
    // Using -z / -z causes git to use NUL instead of newline as the record
    // separator, which is the only safe choice: filenames on Linux/macOS can
    // contain literal newline bytes. Splitting on '\n' would produce two tokens
    // from such a name, treating the spurious second token as a changed path
    // and yielding exit code 1 on every update, masking real errors.
    let parse_nul_paths = |bytes: &[u8]| -> Vec<PathBuf> {
        bytes
            .split(|&b| b == 0)
            .map(path_from_bytes)
            .filter(|path| is_safe_git_path(path))
            .collect()
    };

    // Detect changed files via git diff against HEAD.
    // This fails on repos with no commits, which is fine -- we fall through
    // to untracked file detection below.
    if let Ok(diff_output) = std::process::Command::new(&git)
        .arg("-C")
        .arg(&canonical_root)
        .args(["diff", "-z", "--name-only", "HEAD"])
        .output()
    {
        if diff_output.status.success() {
            changed.extend(parse_nul_paths(&diff_output.stdout));
        }
    }

    // Pick up staged changes (covers initial commit scenario where HEAD
    // doesn't exist yet).
    if let Ok(staged_output) = std::process::Command::new(&git)
        .arg("-C")
        .arg(&canonical_root)
        .args(["diff", "-z", "--name-only", "--cached"])
        .output()
    {
        if staged_output.status.success() {
            changed.extend(parse_nul_paths(&staged_output.stdout));
        }
    }

    // Pick up new untracked files that git-diff doesn't report.
    if let Ok(ut_output) = std::process::Command::new(&git)
        .arg("-C")
        .arg(&canonical_root)
        .args(["ls-files", "-z", "--others", "--exclude-standard"])
        .output()
    {
        if ut_output.status.success() {
            changed.extend(parse_nul_paths(&ut_output.stdout));
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
        // Join with canonical_root (not config.repo_root) so symlinked
        // repo roots don't produce paths outside the resolved tree.
        let abs = canonical_root.join(path);
        if abs.exists() {
            // Canonicalize and verify the resolved path is still under
            // canonical_root. A compromised git binary could emit paths
            // that exploit OS-specific resolution (e.g. symlinks inside
            // the repo, Windows junctions) to escape the repo boundary.
            match abs.canonicalize() {
                Ok(resolved) if resolved.starts_with(&canonical_root) => {
                    if let Err(e) = index.notify_change(&resolved) {
                        eprintln!("st update: {}: {e}", path.display());
                        notify_errors += 1;
                    } else {
                        count += 1;
                    }
                }
                Ok(resolved) => {
                    eprintln!(
                        "st update: {}: resolves outside repo root ({})",
                        path.display(),
                        resolved.display()
                    );
                    notify_errors += 1;
                }
                Err(e) => {
                    eprintln!("st update: {}: {e}", path.display());
                    notify_errors += 1;
                }
            }
        } else if let Err(e) = index.notify_delete(&abs) {
            eprintln!("st update: {}: {e}", path.display());
            notify_errors += 1;
        } else {
            count += 1;
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
    if notify_errors > 0 {
        1
    } else {
        drop(index);
        0
    }
}

fn handle_output(err: io::Error) -> i32 {
    if err.kind() == io::ErrorKind::BrokenPipe {
        0
    } else {
        eprintln!("st: {err}");
        2
    }
}

/// Print supported file types in ripgrep-compatible format.
pub(super) fn cmd_type_list() -> i32 {
    use ignore::types::TypesBuilder;
    let mut builder = TypesBuilder::new();
    builder.add_defaults();
    let mut entries: Vec<(String, Vec<String>)> = Vec::new();
    for def in builder.definitions() {
        let globs: Vec<String> = def.globs().iter().map(|g| g.to_string()).collect();
        entries.push((def.name().to_string(), globs));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for (name, globs) in &entries {
        let joined = globs.join(", ");
        if writeln!(out, "{name}: {joined}").is_err() {
            return 0; // broken pipe
        }
    }
    0
}
