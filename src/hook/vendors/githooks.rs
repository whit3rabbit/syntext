//! Git hooks vendor integration (post-commit, post-checkout, post-merge, post-rewrite).
//!
//! Installs a marker-delimited, fire-and-forget `st update` call into each of
//! the four hook files. Existing hook content is never clobbered: the block
//! is appended to a pre-existing file, or the file is created with a
//! `#!/bin/sh` shebang if it does not yet exist. Uninstall removes only the
//! marker-delimited block, leaving any user-authored hook body intact.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::hook::core::{files, shell};

use super::Outcome;

/// The four git hooks that fire after any commit-graph-changing operation.
pub(crate) const HOOK_NAMES: [&str; 4] =
    ["post-commit", "post-checkout", "post-merge", "post-rewrite"];

const MARKER_ID: &str = "githooks";

pub(crate) fn install(st_program: &str) -> Result<Outcome, String> {
    install_at(&hooks_dir()?, st_program)
}

pub(crate) fn uninstall() -> Result<Outcome, String> {
    uninstall_at(&hooks_dir()?)
}

pub(crate) fn show() -> Result<Outcome, String> {
    show_at(&hooks_dir()?)
}

pub(crate) fn install_at(dir: &Path, st_program: &str) -> Result<Outcome, String> {
    let block = fire_and_forget_block(st_program);
    let mut outcome = Outcome::default();
    for name in HOOK_NAMES {
        let path = dir.join(name);
        if ensure_shell_block(&path, &block)? {
            outcome.changed.push(path);
        }
    }
    outcome.installed = true;
    Ok(outcome)
}

pub(crate) fn uninstall_at(dir: &Path) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();
    for name in HOOK_NAMES {
        let path = dir.join(name);
        if remove_shell_block(&path)? {
            outcome.changed.push(path);
        }
    }
    Ok(outcome)
}

pub(crate) fn show_at(dir: &Path) -> Result<Outcome, String> {
    let mut installed = true;
    for name in HOOK_NAMES {
        installed &= contains_marker(&dir.join(name))?;
    }
    Ok(Outcome {
        installed,
        ..Outcome::default()
    })
}

fn hooks_dir() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir()
        .map_err(|err| format!("st: failed to read current directory: {err}"))?;
    Ok(resolve_hooks_dir(&cwd))
}

/// Resolves the hooks directory for the repository containing `cwd`.
///
/// Uses `git rev-parse --git-path hooks`, which resolves to the *common* git
/// directory's `hooks` subdirectory even from a linked worktree (where
/// `.git` is a file pointing elsewhere, not the directory itself). This is
/// worktree-correct in a way `<root>/.git/hooks` is not: in a linked
/// worktree, `.git/hooks` would not exist (or would be the wrong hooks dir),
/// silently no-op'ing hook installation.
///
/// Falls back to `<project_root>/.git/hooks` if `git` is unavailable or the
/// command fails (e.g. not inside a git repository), preserving prior
/// behavior for that case.
///
/// A user-set `core.hooksPath` is handled for free by `git rev-parse
/// --git-path hooks`: git resolves the `hooks` git-path shorthand through its
/// own `core.hooksPath` config internally, so `install_at` always targets the
/// configured directory rather than the default `.git/hooks`. There is no
/// scenario where installation would silently land in the wrong (ignored)
/// directory, so no separate warning path is needed here; see
/// `resolve_hooks_dir_respects_core_hooks_path` below for the test proving
/// this.
fn resolve_hooks_dir(cwd: &Path) -> PathBuf {
    if let Some(dir) = git_path_hooks(cwd) {
        return dir;
    }
    files::project_root(cwd).join(".git").join("hooks")
}

/// Runs `git -C <cwd> rev-parse --git-path hooks` and returns the resolved,
/// canonicalized path, or `None` on any failure (git missing, not a repo,
/// non-UTF8 output, path does not exist yet to canonicalize).
fn git_path_hooks(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new(crate::git_util::resolve_git_binary())
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    // Canonicalize when possible (resolves symlinks, `.`/`..`); the hooks
    // directory may not exist yet on a fresh repo, so fall back to the
    // uncanonicalized absolute path rather than failing outright.
    Some(absolute.canonicalize().unwrap_or(absolute))
}

fn marker_start() -> String {
    format!("# syntext-agent:{MARKER_ID}:start")
}

fn marker_end() -> String {
    format!("# syntext-agent:{MARKER_ID}:end")
}

/// Builds the marker-delimited block. Tries the resolved `st` binary path
/// first, then falls back to a bare `st` lookup on `PATH`, so a moved or
/// renamed binary degrades to a silent no-op rather than breaking the git
/// operation the hook fired from.
fn fire_and_forget_block(st_program: &str) -> String {
    let st_quoted = shell::shell_quote(st_program);
    format!(
        "{start}\nif command -v {st_quoted} >/dev/null 2>&1; then\n    {st_quoted} update --quiet >/dev/null 2>&1 &\nelif command -v st >/dev/null 2>&1; then\n    st update --quiet >/dev/null 2>&1 &\nfi\n{end}\n",
        start = marker_start(),
        end = marker_end(),
    )
}

fn ensure_shell_block(path: &Path, block: &str) -> Result<bool, String> {
    let existing = read_optional(path)?;
    if existing.contains(&marker_start()) {
        return Ok(false);
    }
    let mut next = existing;
    if next.is_empty() {
        next.push_str("#!/bin/sh\n");
    } else if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(block);
    let changed = files::write_text_if_changed(path, &next)?;
    if changed {
        files::set_executable(path)?;
    }
    Ok(changed)
}

fn remove_shell_block(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let existing = fs::read_to_string(path)
        .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
    let start_marker = marker_start();
    let end_marker = marker_end();
    let Some(start) = existing.find(&start_marker) else {
        return Ok(false);
    };
    let Some(end_rel) = existing[start..].find(&end_marker) else {
        return Ok(false);
    };
    let end = start + end_rel + end_marker.len();
    let mut next = String::new();
    next.push_str(existing[..start].trim_end());
    if !next.is_empty() {
        next.push('\n');
    }
    next.push_str(existing[end..].trim_start_matches(['\r', '\n']));
    files::write_text_if_changed(path, &next)
}

fn contains_marker(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path)
        .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
    Ok(text.contains(&marker_start()))
}

fn read_optional(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).map_err(|err| format!("st: failed to read {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_git_repo() -> tempfile::TempDir {
        let repo = tempfile::TempDir::new().unwrap();
        std::process::Command::new(crate::git_util::resolve_git_binary())
            .arg("-C")
            .arg(repo.path())
            .args(["init"])
            .output()
            .unwrap();
        repo
    }

    #[test]
    fn resolve_hooks_dir_matches_dot_git_hooks_in_a_normal_repo() {
        let git = crate::git_util::resolve_git_binary();
        if !git.is_file() {
            return; // skip on systems without git
        }
        let repo = init_git_repo();
        let expected = repo
            .path()
            .canonicalize()
            .unwrap()
            .join(".git")
            .join("hooks");
        let resolved = resolve_hooks_dir(repo.path());
        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolve_hooks_dir_respects_core_hooks_path() {
        // `git rev-parse --git-path hooks` (used by resolve_hooks_dir) already
        // honors a configured core.hooksPath internally (git resolves the
        // `hooks` git-path shorthand through hooks_path()), so no separate
        // `git config core.hooksPath` shell-out is needed here. This test
        // proves that behavior rather than assuming it.
        let git = crate::git_util::resolve_git_binary();
        if !git.is_file() {
            return; // skip on systems without git
        }
        let repo = init_git_repo();
        let custom_hooks = tempfile::TempDir::new().unwrap();
        let status = std::process::Command::new(&git)
            .arg("-C")
            .arg(repo.path())
            .args(["config", "core.hooksPath"])
            .arg(custom_hooks.path())
            .status()
            .unwrap();
        assert!(status.success());

        let expected = custom_hooks.path().canonicalize().unwrap();
        let resolved = resolve_hooks_dir(repo.path());
        assert_eq!(resolved, expected);
    }

    #[test]
    fn install_at_targets_custom_hooks_path_not_dot_git_hooks() {
        // End-to-end proof (not just resolution logic): with core.hooksPath
        // set to a custom directory, `install_at` must write the hook files
        // there, and must NOT create/populate `<repo>/.git/hooks` — that
        // directory would be silently ignored by git.
        let git = crate::git_util::resolve_git_binary();
        if !git.is_file() {
            return; // skip on systems without git
        }
        let repo = init_git_repo();
        let custom_hooks = tempfile::TempDir::new().unwrap();
        let status = std::process::Command::new(&git)
            .arg("-C")
            .arg(repo.path())
            .args(["config", "core.hooksPath"])
            .arg(custom_hooks.path())
            .status()
            .unwrap();
        assert!(status.success());

        let resolved = resolve_hooks_dir(repo.path());
        install_at(&resolved, "st").expect("install_at should succeed");

        for name in HOOK_NAMES {
            let installed_path = custom_hooks.path().join(name);
            assert!(
                installed_path.exists(),
                "expected hook {name} to be installed under core.hooksPath, found nothing at {}",
                installed_path.display()
            );
            let dot_git_hooks_path = repo.path().join(".git").join("hooks").join(name);
            assert!(
                !dot_git_hooks_path.exists()
                    || !fs::read_to_string(&dot_git_hooks_path)
                        .unwrap_or_default()
                        .contains(&marker_start()),
                "hook {name} must not be installed into the literal .git/hooks \
                 when core.hooksPath is set, found marker at {}",
                dot_git_hooks_path.display()
            );
        }
    }

    #[test]
    fn resolve_hooks_dir_falls_back_when_not_a_git_repo() {
        let temp = tempfile::TempDir::new().unwrap();
        // No .git anywhere in this temp dir's ancestry (best-effort; the
        // legacy fallback just needs to not panic and to return some path).
        let resolved = resolve_hooks_dir(temp.path());
        assert!(resolved.ends_with(Path::new(".git").join("hooks")));
    }
}
