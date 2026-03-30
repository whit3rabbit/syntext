//! Management subcommand handlers: index, status, update.

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::index::Index;
use crate::path_util::path_from_bytes;
use crate::Config;

/// Walk PATH for a file named `filename`, canonicalizing the first match.
/// Returns `None` if no matching file is found.
/// Skips directories (e.g. a dir named "git" on PATH) via `is_file()`.
fn find_in_path(filename: &str) -> Option<PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(filename);
        if candidate.is_file() {
            if let Ok(resolved) = candidate.canonicalize() {
                return Some(resolved);
            }
        }
    }
    None
}

/// Resolves the absolute path to the `git` binary by walking PATH entries.
///
/// On Unix: searches PATH, canonicalizes each candidate, falls back to
/// `/usr/bin/git`. On non-Unix (Windows): searches PATH for `git.exe`,
/// falls back to common Git for Windows install locations.
///
/// Using `Command::new("git")` would resolve via `$PATH`, allowing a
/// malicious `git` binary earlier on PATH to intercept the call.
/// This function walks PATH explicitly, resolves symlinks via `canonicalize`,
/// and falls back to a known location rather than a bare name.
///
/// Security note: `canonicalize` + `is_file` narrows but does not eliminate
/// the TOCTOU window between path resolution and `Command::new` exec.
#[cfg(unix)]
fn resolve_git_binary() -> PathBuf {
    find_in_path("git").unwrap_or_else(|| PathBuf::from("/usr/bin/git"))
}

#[cfg(not(unix))]
fn resolve_git_binary() -> PathBuf {
    if let Some(p) = find_in_path("git.exe") {
        return p;
    }
    // Common Git for Windows install locations.
    for fallback in &[
        r"C:\Program Files\Git\bin\git.exe",
        r"C:\Program Files (x86)\Git\bin\git.exe",
    ] {
        let p = PathBuf::from(fallback);
        if p.is_file() {
            return p;
        }
    }
    // Last resort: git is not in PATH and not at any known install location.
    // Command::new("git.exe") will fail with "not found" rather than succeed,
    // so this is a graceful degradation, not an injection surface.
    PathBuf::from("git.exe")
}

/// Returns `true` if a path line from git stdout is safe to use as a
/// repo-relative path.
///
/// Rejects: empty strings, absolute paths, and any path containing a
/// parent-directory component (`..`). Git always emits repo-relative
/// paths; anything else is unexpected and potentially hostile.
fn is_safe_git_path(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    use std::path::Component;
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
            _ => {}
        }
    }
    true
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
        let abs = config.repo_root.join(path);
        if abs.exists() {
            if let Err(e) = index.notify_change(&abs) {
                eprintln!("st update: {}: {e}", path.display());
                notify_errors += 1;
            } else {
                count += 1;
            }
        } else {
            if let Err(e) = index.notify_delete(&abs) {
                eprintln!("st update: {}: {e}", path.display());
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
    if notify_errors > 0 {
        1
    } else {
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use super::is_safe_git_path;
    use super::resolve_git_binary;

    #[test]
    fn git_binary_resolves_to_absolute_path() {
        let path = resolve_git_binary();
        // On Unix the fallback is /usr/bin/git (absolute).
        // On non-Unix, git may not be installed, in which case
        // the fallback is the bare name "git.exe". Accept either.
        #[cfg(unix)]
        assert!(
            path.is_absolute(),
            "git binary must resolve to absolute path on Unix, got: {:?}",
            path
        );
        #[cfg(not(unix))]
        assert!(
            path.is_absolute() || path == std::path::Path::new("git.exe"),
            "git binary must be absolute or the bare-name fallback, got: {:?}",
            path
        );
    }

    #[test]
    fn is_safe_git_path_rejects_traversal_and_absolute() {
        assert!(!is_safe_git_path(Path::new("../../etc/passwd")));
        assert!(!is_safe_git_path(Path::new("/etc/passwd")));
        assert!(!is_safe_git_path(Path::new("src/../../../etc/passwd")));
        assert!(!is_safe_git_path(Path::new("")));
        assert!(is_safe_git_path(Path::new("src/main.rs")));
        assert!(is_safe_git_path(Path::new("foo/bar/baz.rs")));
        assert!(is_safe_git_path(Path::new("Cargo.toml")));
    }

    #[cfg(unix)]
    #[test]
    fn is_safe_git_path_accepts_non_utf8_relative_paths() {
        let path = PathBuf::from(OsStr::from_bytes(b"src/\xff.rs"));
        assert!(is_safe_git_path(&path));
    }
}
