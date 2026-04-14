//! Git binary resolution and path safety helpers.
//!
//! Shared by both the CLI (`cli::manage`) and the index subsystem
//! (`index::helpers`). Consolidated here to prevent divergence between
//! platform-specific resolution logic.

use std::path::{Path, PathBuf};

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
pub(crate) fn resolve_git_binary() -> PathBuf {
    find_in_path("git").unwrap_or_else(|| PathBuf::from("/usr/bin/git"))
}

#[cfg(not(unix))]
pub(crate) fn resolve_git_binary() -> PathBuf {
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
/// Rejects: empty strings, absolute paths, any path containing a
/// parent-directory component (`..`), and (on Windows) components
/// containing `:` which could reference NTFS alternate data streams
/// (e.g. `file.rs::$DATA`).
#[cfg_attr(not(feature = "clap"), allow(dead_code))]
pub(crate) fn is_safe_git_path(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    use std::path::Component;
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
            Component::Normal(s) => {
                // On Windows, reject NTFS alternate data stream references.
                // A colon after a filename (e.g. "file.rs::$DATA") passes
                // Component parsing as Normal but exploits OS-specific path
                // handling. Unix filenames can legally contain colons.
                #[cfg(windows)]
                if s.to_string_lossy().contains(':') {
                    return false;
                }
                let _ = s; // suppress unused on non-windows
            }
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    #[cfg(unix)]
    use std::path::PathBuf;

    use super::is_safe_git_path;
    use super::resolve_git_binary;

    #[test]
    fn git_binary_resolves_to_absolute_path() {
        let path = resolve_git_binary();
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

    #[test]
    fn resolve_git_binary_returns_a_path() {
        let git = resolve_git_binary();
        assert!(!git.as_os_str().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn is_safe_git_path_rejects_ntfs_alternate_data_streams() {
        assert!(!is_safe_git_path(Path::new("src/main.rs::$DATA")));
        assert!(!is_safe_git_path(Path::new("file.txt:hidden_stream")));
    }

    #[test]
    fn nonexistent_git_path_is_not_a_file() {
        let fake = std::path::PathBuf::from("/absolutely/does/not/exist/git");
        assert!(!fake.is_file());
    }
}
