//! CLI path resolution: turning user-given path args into repo-relative
//! on-disk locations, shared by the scope filters and the search dispatcher.

use std::path::{Component, Path, PathBuf};

/// Every explicitly named path argument that does not exist on disk (rg
/// reports each one, still searches the rest, and exits 2). `-` is not a
/// filesystem path and is never "missing".
pub(in crate::cli) fn missing_explicit_paths<'a>(
    repo_root: &std::path::Path,
    paths: &'a [PathBuf],
) -> Vec<&'a PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| repo_root.to_path_buf());
    paths
        .iter()
        .filter(|p| p.as_os_str() != "-")
        .filter(|p| !cli_path_on_disk(repo_root, &cwd, p).exists())
        .collect()
}

pub(super) fn path_is_directory(repo_root: &Path, cwd: &Path, path: &Path) -> bool {
    cli_path_on_disk(repo_root, cwd, path)
        .metadata()
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

pub(super) fn relativize_cli_path(repo_root: &Path, cwd: &Path, path: &Path) -> PathBuf {
    // Absolute paths: strip the repo root to get the repo-relative path.
    // Relative paths: see `resolve_relative_base` for the CWD-vs-repo-root rule.
    let base = if path.is_absolute() {
        path.to_path_buf()
    } else {
        resolve_relative_base(repo_root, cwd, path)
    };
    let rel = match base.strip_prefix(repo_root) {
        Ok(rel) => rel,
        Err(_) => base.as_path(),
    };
    crate::path_util::normalize_to_forward_slashes(normalize_relative_path(rel))
}

pub(super) fn cli_path_on_disk(repo_root: &Path, cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        resolve_relative_base(repo_root, cwd, path)
    }
}

/// Pick the absolute base a relative CLI path resolves against.
///
/// ripgrep resolves relative paths against CWD, so when CWD is inside the repo
/// we do too: an agent standing in `<repo>/crates/foo` scopes `st pat src/` to
/// `crates/foo/src`, not `<root>/src`, and `st pat .` scopes to the subdir
/// instead of normalizing to an empty path that searches the whole repo.
///
/// When CWD is outside the repo (an explicit `--repo-root` pointing at a repo
/// the caller is not standing in), fall back to repo-root-relative resolution
/// so the path still reaches the index. This preserves the long-standing
/// `--repo-root <repo> <relpath>` contract.
fn resolve_relative_base(repo_root: &Path, cwd: &Path, path: &Path) -> PathBuf {
    let via_cwd = cwd.join(path);
    if via_cwd.starts_with(repo_root) {
        via_cwd
    } else {
        repo_root.join(path)
    }
}

fn normalize_relative_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                // Collapse `..` against the previous normal component so an
                // in-repo `..` (e.g. `st pat ../sibling` from a subdir, which
                // arrives here as `sub/../sibling`) maps to a real indexed path
                // instead of the literal `sub/../sibling`, which matches nothing
                // and silently returns zero results. A `..` that escapes the
                // repo root (no normal component to pop) is kept verbatim; it
                // still matches no indexed path, same as before.
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else {
                    normalized.push(component.as_os_str());
                }
            }
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}
